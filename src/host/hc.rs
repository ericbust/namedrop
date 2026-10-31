pub mod events;

use super::*;
use crate::{AgentSessionId, HerdrCommandExecutor, WorktreeId};
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

#[derive(Clone, Copy)]
pub struct HcHost<E: HerdrCommandExecutor> {
    transport: E,
}

#[derive(Deserialize)]
struct TerminalList {
    terminals: Vec<TerminalSummary>,
}

#[derive(Deserialize)]
struct TerminalSummary {
    address: String,
    state: String,
}

#[derive(Deserialize)]
pub(super) struct Terminal {
    pub(super) address: String,
    state: String,
    #[serde(default)]
    pub(super) labels: BTreeMap<String, String>,
    #[serde(default)]
    argv: Vec<String>,
    cwd: Option<String>,
    pid: Option<u32>,
    foreground: Option<Foreground>,
    status: Option<Status>,
    reported: bool,
    rows: usize,
}

#[derive(Deserialize)]
struct Foreground {
    pid: i32,
    name: String,
    #[serde(default)]
    argv: Vec<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct Status {
    state: StatusWord,
    app: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum StatusWord {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl StatusWord {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked | Self::Error => "blocked",
        }
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: WireError,
}

#[derive(Deserialize)]
struct WireError {
    kind: String,
    message: String,
    #[serde(default)]
    hint: String,
}

pub(super) fn host_error(raw: String) -> HostError {
    match serde_json::from_str::<ErrorEnvelope>(&raw) {
        Ok(envelope) => HostError {
            code: Some(envelope.error.kind),
            message: if envelope.error.hint.is_empty() {
                envelope.error.message
            } else {
                format!("{} {}", envelope.error.message, envelope.error.hint)
            },
        },
        Err(_) => raw.into(),
    }
}

fn prompt_failure(raw: String) -> PromptFailure {
    let kind = serde_json::from_str::<ErrorEnvelope>(&raw)
        .ok()
        .map(|envelope| match envelope.error.kind.as_str() {
            "not_found" | "finished" | "blocked" | "status_mismatch" => {
                PromptFailureKind::TemporaryPreInvocation
            }
            "unavailable"
                if envelope.error.hint.contains("nothing was typed")
                    || (envelope
                        .error
                        .message
                        .ends_with(" is lost, waiting for its new shell")
                        && envelope.error.hint == "retry after the restarted event") =>
            {
                PromptFailureKind::TemporaryPreInvocation
            }
            "usage" | "invalid_params" | "config" | "machine_unknown" => {
                PromptFailureKind::DefinitiveNonRetryable
            }
            _ => PromptFailureKind::AmbiguousInvocation,
        })
        .unwrap_or(PromptFailureKind::AmbiguousInvocation);
    PromptFailure {
        kind,
        message: host_error(raw).message,
    }
}

impl Terminal {
    fn workspace(&self) -> &str {
        self.labels
            .get("herdr.space")
            .map(String::as_str)
            .filter(|id| !id.is_empty())
            .unwrap_or("headless")
    }

    fn cwd(&self) -> Option<&str> {
        self.foreground
            .as_ref()
            .and_then(|foreground| foreground.cwd.as_deref())
            .or(self.cwd.as_deref())
    }

    fn space_label(&self) -> String {
        self.labels
            .get("where")
            .and_then(|label| label.split_once(" / "))
            .map(|(space, _)| space.to_owned())
            .unwrap_or_else(|| self.workspace().to_owned())
    }

    fn session(&self) -> Option<&str> {
        if let Some(pid) = self.labels.get("namedrop.session.pid")
            && self.pid.map(|pid| pid.to_string()).as_ref() != Some(pid)
        {
            return None;
        }
        self.labels
            .get("namedrop.session")
            .map(String::as_str)
            .filter(|session| !session.is_empty())
    }

    fn pane(&self) -> DiscoveredPane {
        let app = self
            .status
            .as_ref()
            .and_then(|status| status.app.as_deref())
            .filter(|app| !app.is_empty());
        let program = self
            .foreground
            .as_ref()
            .and_then(|foreground| {
                foreground
                    .argv
                    .first()
                    .map(String::as_str)
                    .or(Some(foreground.name.as_str()))
            })
            .or_else(|| self.argv.first().map(String::as_str));
        let agent = app
            .map(|app| match app {
                "claude-code" => "claude".to_owned(),
                _ => app.to_owned(),
            })
            .or_else(|| {
                let name = Path::new(program?).file_name()?.to_str()?;
                let name = if name == "claude" {
                    "claude-code"
                } else {
                    name
                };
                crate::AgentRuntime::parse(name).ok().map(|agent| {
                    match agent {
                        crate::AgentRuntime::ClaudeCode => "claude",
                        crate::AgentRuntime::Codex => "codex",
                    }
                    .to_owned()
                })
            });
        DiscoveredPane {
            pane_id: self.address.clone(),
            terminal_id: self.address.clone(),
            tab_id: self.labels.get("herdr.tab").cloned().unwrap_or_default(),
            workspace_id: self.workspace().to_owned(),
            cwd: self.cwd().map(str::to_owned),
            pane_label: self
                .labels
                .get("namedrop.name")
                .map(|alias| format!("@{alias}")),
            agent_session_id: self.session().map(AgentSessionId::new),
            occupant_label: agent.clone(),
            agent,
            agent_status: self
                .status
                .as_ref()
                .map(|status| status.state.as_str().to_owned()),
            status_reported: Some(self.reported),
            idle_state_label: None,
        }
    }
}

pub(crate) fn label_lock(state: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)
        .map_err(|error| error.to_string())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state.join("hc-labels.lock"))
        .map_err(|error| error.to_string())?;
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
}

impl<E: HerdrCommandExecutor> HcHost<E> {
    pub fn new(transport: E) -> Self {
        Self { transport }
    }

    pub(crate) fn shell_pid(&self, terminal: &str, budget: &dyn Budget) -> Option<u32> {
        let response: serde_json::Value = self
            .execute(&["get", terminal, "--json"], Some(budget))
            .ok()?;
        u32::try_from(response["pid"].as_u64()?)
            .ok()
            .filter(|pid| *pid > 0)
    }

    fn execute<T: DeserializeOwned>(
        &self,
        args: &[&str],
        budget: Option<&dyn Budget>,
    ) -> Result<T, HostError> {
        let output = match budget {
            Some(budget) => self.transport.execute_bounded(
                args,
                budget.remaining()?,
                budget.cancelled(),
                budget.max_output_bytes(),
            ),
            None => self.transport.execute(args),
        }
        .map_err(host_error)?;
        serde_json::from_str(&output.stdout)
            .map_err(|error| format!("hc returned unreadable JSON: {error}").into())
    }

    pub(super) fn terminal(
        &self,
        pane: &str,
        budget: Option<&dyn Budget>,
    ) -> Result<Terminal, HostError> {
        self.execute(&["get", pane, "--json"], budget)
    }

    pub(super) fn terminals(
        &self,
        budget: Option<&dyn Budget>,
    ) -> Result<Vec<Terminal>, HostError> {
        let list: TerminalList = self.execute(&["ls", "--json"], budget)?;
        let mut terminals = Vec::new();
        for summary in list
            .terminals
            .into_iter()
            .filter(|summary| summary.state == "live")
        {
            match self.terminal(&summary.address, budget) {
                Ok(terminal) if terminal.state == "live" => terminals.push(terminal),
                Ok(_) => (),
                Err(error) if matches!(error.code.as_deref(), Some("not_found" | "finished")) => (),
                Err(error) => return Err(error),
            }
        }
        Ok(terminals)
    }

    pub fn set_labels(&self, pane: &str, labels: &[(&str, &str)]) -> Result<(), HostError> {
        let terminal = self.terminal(pane, None)?;
        let changed: Vec<String> = labels
            .iter()
            .filter(|(key, value)| terminal.labels.get(*key).map_or("", String::as_str) != *value)
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        if changed.is_empty() {
            return Ok(());
        }
        let mut args = vec!["label", pane];
        args.extend(changed.iter().map(String::as_str));
        args.push("--json");
        self.transport
            .execute(&args)
            .map_err(host_error)
            .map(|_| ())
    }

    pub fn publish_session(
        &self,
        state: &Path,
        pane: &str,
        session: &str,
    ) -> Result<(), HostError> {
        let _lock = label_lock(state)?;
        let terminal = self.terminal(pane, None)?;
        let pid = terminal
            .pid
            .filter(|pid| *pid > 0)
            .ok_or("hc terminal has no live program to own a session".to_owned())?;
        self.set_labels(
            pane,
            &[
                ("namedrop.session", session),
                ("namedrop.session.pid", &pid.to_string()),
            ],
        )
    }

    pub fn invalidate_session(&self, state: &Path, pane: &str) -> Result<(), HostError> {
        let _lock = label_lock(state)?;
        let terminal = self.terminal(pane, None)?;
        let current_pid = terminal.pid.map(|pid| pid.to_string());
        if current_pid
            .as_ref()
            .is_some_and(|pid| terminal.labels.get("namedrop.session.pid") == Some(pid))
        {
            return Ok(());
        }
        self.set_labels(
            pane,
            &[("namedrop.session", ""), ("namedrop.session.pid", "")],
        )
    }

    pub fn version(&self) -> Result<String, HostError> {
        self.transport
            .execute(&["--version"])
            .map(|output| output.stdout.trim().to_owned())
            .map_err(host_error)
    }
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(PathBuf::from(
        String::from_utf8(output.stdout).ok()?.trim_end(),
    ))
}

impl<E: HerdrCommandExecutor> Host for HcHost<E> {
    fn space(
        &self,
        at: SpaceLocator<'_>,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        let terminals = self.terminals(budget)?;
        let workspace = match at {
            SpaceLocator::Workspace(id) => id.to_owned(),
            SpaceLocator::Checkout(cwd) => {
                let root = git_root(cwd).ok_or_else(|| {
                    "hc cannot resolve this checkout. Pass --hc-space <space-id>.".to_owned()
                })?;
                let spaces: BTreeSet<&str> = terminals
                    .iter()
                    .filter(|terminal| {
                        terminal
                            .cwd()
                            .and_then(|cwd| git_root(Path::new(cwd)))
                            .as_ref()
                            == Some(&root)
                    })
                    .map(Terminal::workspace)
                    .collect();
                match spaces.len() {
                    1 => (*spaces.first().unwrap()).to_owned(),
                    0 => {
                        return Err(
                            "No hc space owns this checkout. Pass --hc-space <space-id>."
                                .to_owned()
                                .into(),
                        );
                    }
                    _ => return Err(
                        "More than one hc space owns this checkout. Pass --hc-space <space-id>."
                            .to_owned()
                            .into(),
                    ),
                }
            }
        };
        let terminals: Vec<_> = terminals
            .iter()
            .filter(|terminal| terminal.workspace() == workspace)
            .collect();
        let roots: Option<Vec<PathBuf>> = terminals
            .iter()
            .map(|terminal| terminal.cwd().and_then(|cwd| git_root(Path::new(cwd))))
            .collect();
        let root = roots
            .as_ref()
            .filter(|roots| !roots.is_empty() && roots.iter().all(|root| root == &roots[0]))
            .map(|roots| &roots[0]);
        Ok(DiscoveredWorktree {
            worktree_id: WorktreeId::new(
                root.map(|root| root.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("hc-space:{workspace}")),
            ),
            workspace_id: workspace,
            worktree_attached: root.is_some(),
            label: terminals.first().map(|terminal| terminal.space_label()),
            branch: None,
            panes: terminals.iter().map(|terminal| terminal.pane()).collect(),
        })
    }

    fn spaces(&self) -> Result<Vec<ListedWorkspace>, HostError> {
        let mut spaces = BTreeMap::new();
        for terminal in self.terminals(None)? {
            spaces
                .entry(terminal.workspace().to_owned())
                .or_insert_with(|| ListedWorkspace {
                    id: terminal.workspace().to_owned(),
                    label: terminal.space_label(),
                    repo: None,
                });
        }
        Ok(spaces.into_values().collect())
    }

    fn panes(&self, scope: PaneScope<'_>) -> Result<Vec<DiscoveredPane>, HostError> {
        Ok(self
            .terminals(None)?
            .iter()
            .filter(|terminal| match scope {
                PaneScope::All => true,
                PaneScope::Workspace(id) => terminal.workspace() == id,
            })
            .map(Terminal::pane)
            .collect())
    }

    fn process_info(&self, pane: &str) -> Option<ProcessInfo> {
        let terminal = self.terminal(pane, None).ok()?;
        let shell_pid = terminal.pid.filter(|pid| *pid > 0)?;
        let foreground = u32::try_from(terminal.foreground?.pid)
            .ok()
            .filter(|pid| *pid > 0)?;
        Some(ProcessInfo {
            shell_pid: u64::from(shell_pid),
            foreground_process_group_id: u64::from(foreground),
            foreground_processes: vec![Process { pid: foreground }],
        })
    }

    fn read(
        &self,
        pane: &str,
        lines: usize,
        budget: Option<&dyn Budget>,
    ) -> Result<String, HostError> {
        #[derive(Deserialize)]
        struct ReadResult {
            text: String,
        }
        let terminal = self.terminal(pane, budget)?;
        let count = lines.to_string();
        let args = if lines <= terminal.rows {
            vec!["read", pane, "--json"]
        } else {
            vec!["read", pane, "--lines", &count, "--json"]
        };
        let result = self.execute::<ReadResult>(&args, budget)?;
        if lines > terminal.rows {
            return Ok(result.text);
        }
        let skip = result
            .text
            .split_inclusive('\n')
            .count()
            .saturating_sub(lines);
        Ok(result.text.split_inclusive('\n').skip(skip).collect())
    }

    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptFailure> {
        self.transport
            .execute(&["send", pane, text, "--json"])
            .map(|_| ())
            .map_err(prompt_failure)
    }

    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), PromptFailure> {
        self.prompt(pane, command)
    }

    fn interrupt(&self, pane: &str) -> Result<(), HostError> {
        self.transport
            .execute(&["interrupt", pane, "--timeout", "2", "--json"])
            .map(|_| ())
            .map_err(host_error)
    }

    fn report_metadata(&self, pane: &str, metadata: &PaneMetadata) -> Result<(), HostError> {
        match metadata {
            PaneMetadata::Identity { title, .. } => self.set_labels(
                pane,
                &[("namedrop.name", title.strip_prefix('@').unwrap_or(title))],
            ),
            PaneMetadata::Counters {
                awaiting,
                attention,
            } => self.set_labels(
                pane,
                &[
                    ("namedrop.awaiting", &awaiting.to_string()),
                    ("namedrop.attention", &attention.to_string()),
                ],
            ),
            PaneMetadata::Relay { .. } => Ok(()),
        }
    }

    fn notify(&self, _: &str, _: &str, _: bool) -> Result<(), HostError> {
        Ok(())
    }
    fn supports_tab_labels(&self) -> bool {
        false
    }
    fn tab_label(&self, _: &str) -> Option<String> {
        None
    }
    fn rename_tab(&self, _: &str, _: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn open_popup(&self, popup: &Popup<'_>) -> Result<serde_json::Value, HostError> {
        let program = std::env::var_os("HERDR_APP_BIN_PATH").unwrap_or_else(|| "herdr-app".into());
        let output = crate::runtime::hc_command(program)
            .args([
                "do",
                "mod.open",
                "--name",
                &format!("namedrop-{}", popup.entrypoint),
            ])
            .output()
            .map_err(|error| {
                format!("Open Namedrop from the Herdr app, or put herdr-app on PATH: {error}")
            })?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr)
                .trim()
                .to_owned()
                .into());
        }
        Ok(serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| serde_json::json!({"opened": true})))
    }
    fn reload_config(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn plugin_root(&self) -> Option<PathBuf> {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(Path::to_path_buf)
    }
    fn socket_id(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct Core {
            id: String,
        }
        #[derive(Deserialize)]
        struct Server {
            core: Core,
        }
        self.execute::<Server>(&["server", "status", "--json"], None)
            .ok()
            .map(|server| format!("hc:{}", server.core.id))
    }
    fn caller_identity(&self, claim: &CallerClaim) -> Result<CallerIdentity, HostError> {
        if let Some(pane) = &claim.pane {
            let workspace = match &claim.workspace {
                Some(workspace) => workspace.clone(),
                None => self.terminal(pane, None)?.workspace().to_owned(),
            };
            return Ok(CallerIdentity {
                workspace: Some(workspace),
                pane: Some(pane.clone()),
            });
        }
        if let Some(pane) = &claim.inherited_pane
            && let Ok(terminal) = self.terminal(pane, None)
            && terminal.state == "live"
            && terminal.pid.is_some_and(|pid| {
                crate::terminal::ancestor_pids(std::process::id()).any(|ancestor| ancestor == pid)
            })
        {
            return Ok(CallerIdentity {
                workspace: claim
                    .workspace
                    .clone()
                    .or_else(|| Some(terminal.workspace().to_owned())),
                pane: Some(pane.clone()),
            });
        }
        if !claim.agent_sessions.is_empty() {
            let terminals = self.terminals(None)?;
            let matches: Vec<_> = terminals
                .iter()
                .filter(|terminal| {
                    terminal.session().is_some_and(|session| {
                        claim
                            .agent_sessions
                            .iter()
                            .any(|claimed| claimed == session)
                    })
                })
                .collect();
            if matches.len() == 1 {
                return Ok(CallerIdentity {
                    workspace: claim
                        .workspace
                        .clone()
                        .or_else(|| Some(matches[0].workspace().to_owned())),
                    pane: Some(matches[0].address.clone()),
                });
            }
            if !matches.is_empty() {
                return Err("Agent session identity matches multiple live hc terminals. Pass --hc-terminal explicitly.".to_owned().into());
            }
        }
        if claim.inherited_pane.is_some() {
            return Err("HC_TERMINAL_ID is stale or cannot be verified. Run Codex with --no-daemon, or pass --hc-terminal and --hc-space explicitly.".to_owned().into());
        }
        Ok(CallerIdentity {
            workspace: claim.workspace.clone(),
            pane: None,
        })
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) const WORDS: [Option<&str>; 6] = [
        Some("idle"),
        Some("working"),
        Some("done"),
        Some("blocked"),
        Some("error"),
        None,
    ];

    pub(crate) fn pane(word: Option<&str>) -> DiscoveredPane {
        let mut record: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/hc/get-labeled.stdout"))
                .unwrap();
        record["status"] = word
            .map(|state| serde_json::json!({"state":state,"app":"codex"}))
            .unwrap_or_default();
        record["reported"] = serde_json::json!(word.is_some());
        serde_json::from_value::<Terminal>(record).unwrap().pane()
    }

    pub(crate) fn address(word: Option<&str>) -> crate::PaneAddressRecord {
        let pane = pane(word);
        crate::PaneAddressRecord {
            pane_type: crate::PaneType::Agent,
            id: crate::PaneAddressId::new("address"),
            role_id: crate::PaneRoleId::new("role"),
            role_name: "agent".into(),
            role_description: String::new(),
            persona: None,
            alias: "agent".into(),
            lifecycle: crate::PaneAddressLifecycle::Occupied,
            pane_id: Some(pane.pane_id),
            pane_label: None,
            agent_session_id: pane.agent_session_id,
            occupant_label: pane.occupant_label,
            agent_status: pane.agent_status,
            retired: false,
            collaboration_disabled: false,
            remote_commands: false,
        }
    }

    #[test]
    fn golden_argv() {
        #[derive(Default)]
        struct Executor(std::cell::RefCell<Vec<Vec<String>>>);
        impl HerdrCommandExecutor for Executor {
            fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
                self.0
                    .borrow_mut()
                    .push(args.iter().map(|arg| (*arg).to_owned()).collect());
                let stdout = match args {
                    ["ls", "--json"] => include_str!("../../tests/fixtures/hc/ls.stdout"),
                    ["get", ..] => include_str!("../../tests/fixtures/hc/get-labeled.stdout"),
                    ["read", ..] => include_str!("../../tests/fixtures/hc/read.stdout"),
                    ["server", "status", "--json"] => {
                        include_str!("../../tests/fixtures/hc/server-status.stdout")
                    }
                    _ => "{}",
                }
                .to_owned();
                Ok(crate::HerdrCommandOutput { stdout })
            }
        }
        let executor = Executor::default();
        let host = HcHost::new(&executor);
        host.panes(PaneScope::All).unwrap();
        host.read("t1", 40, None).unwrap();
        host.read("t1", 1000, None).unwrap();
        host.prompt("t1", "hello").unwrap();
        host.run_in_pane("t1", "command").unwrap();
        host.interrupt("t1").unwrap();
        host.set_labels("t1", &[("namedrop.session", "next-session")])
            .unwrap();
        assert!(host.socket_id().is_some());
        assert_eq!(
            *executor.0.borrow(),
            vec![
                vec!["ls", "--json"],
                vec!["get", "t1", "--json"],
                vec!["get", "t1", "--json"],
                vec!["read", "t1", "--json"],
                vec!["get", "t1", "--json"],
                vec!["read", "t1", "--lines", "1000", "--json"],
                vec!["send", "t1", "hello", "--json"],
                vec!["send", "t1", "command", "--json"],
                vec!["interrupt", "t1", "--timeout", "2", "--json"],
                vec!["get", "t1", "--json"],
                vec!["label", "t1", "namedrop.session=next-session", "--json"],
                vec!["server", "status", "--json"],
            ]
        );
    }
}
