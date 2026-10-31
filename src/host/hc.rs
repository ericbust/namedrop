pub mod events;

use super::*;
use crate::terminal::{process_is_shell, process_start};
use crate::{AgentRuntime, AgentSessionId, HerdrCommandExecutor, WorktreeId};
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
    cwd: Option<String>,
    pid: Option<u32>,
    foreground: Option<Foreground>,
    status: Option<Status>,
    status_ms: Option<u64>,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum StatusWord {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    #[serde(other)]
    Unknown,
}

impl StatusWord {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked | Self::Error => "blocked",
            Self::Unknown => "unknown",
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

fn pre_invocation(error: HostError) -> PromptFailure {
    PromptFailure {
        kind: PromptFailureKind::TemporaryPreInvocation,
        message: error.message,
    }
}

enum PromptTarget {
    Agent(Option<u64>),
    Shell,
}

fn prompt_failure(raw: String, target: PromptTarget) -> PromptFailure {
    let kind = serde_json::from_str::<ErrorEnvelope>(&raw)
        .ok()
        .map(|envelope| match (envelope.error.kind.as_str(), target) {
            ("blocked" | "status_mismatch", PromptTarget::Agent(observed_status_ms)) => {
                PromptFailureKind::DeferredUntilSettle { observed_status_ms }
            }
            ("not_found" | "finished" | "blocked" | "status_mismatch", _) => {
                PromptFailureKind::TemporaryPreInvocation
            }
            ("unavailable", _)
                if envelope.error.hint.contains("nothing was typed")
                    || (envelope
                        .error
                        .message
                        .ends_with(" is lost, waiting for its new shell")
                        && envelope.error.hint == "retry after the restarted event") =>
            {
                PromptFailureKind::TemporaryPreInvocation
            }
            ("usage" | "invalid_params" | "config" | "machine_unknown", _) => {
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

enum Occupancy<'a> {
    Terminal,
    Agent {
        runtime: AgentRuntime,
        session: Option<&'a str>,
        state: AgentState<'a>,
    },
}

enum AgentState<'a> {
    Foreground(Option<&'a StatusWord>),
    Background,
}

fn program_runtime(program: &str) -> Option<AgentRuntime> {
    let name = Path::new(program).file_name()?.to_str()?;
    AgentRuntime::parse(if name == "claude" {
        "claude-code"
    } else {
        name
    })
    .ok()
}

fn process_runtime(pid: u32) -> Option<AgentRuntime> {
    let arguments = crate::terminal::process_arguments(pid)?;
    arguments
        .first()
        .and_then(|program| program_runtime(program))
        .or_else(|| program_runtime(crate::terminal::process_executable(pid)?.to_str()?))
}

fn local_terminal(pane: &str) -> Result<(), HostError> {
    if pane.contains('/') {
        return Err("Namedrop supports local hc terminals only"
            .to_owned()
            .into());
    }
    Ok(())
}

#[derive(Debug)]
pub struct DiscoveryWarning {
    pub terminal: String,
    pub message: String,
}

pub(super) struct TerminalDiscovery {
    pub(super) terminals: Vec<Terminal>,
    pub(super) warnings: Vec<DiscoveryWarning>,
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

    fn session_owner(&self) -> Option<(u32, AgentRuntime)> {
        let pid = self.labels.get("namedrop.session.pid")?.parse().ok()?;
        let runtime = process_runtime(pid)?;
        let start = self.labels.get("namedrop.session.start")?;
        if start.is_empty() || process_start(pid).as_ref() != Some(start) {
            return None;
        }
        Some((pid, runtime))
    }

    fn occupancy(&self) -> Occupancy<'_> {
        let foreground = self.foreground.as_ref().and_then(|foreground| {
            let program = foreground
                .argv
                .first()
                .map(String::as_str)
                .unwrap_or(&foreground.name);
            Some((
                u32::try_from(foreground.pid).ok()?,
                program_runtime(program)?,
            ))
        });
        if let Some((pid, runtime)) = self.session_owner() {
            let session = self
                .labels
                .get("namedrop.session")
                .map(String::as_str)
                .filter(|session| !session.is_empty());
            return Occupancy::Agent {
                runtime,
                session,
                state: if foreground == Some((pid, runtime)) {
                    AgentState::Foreground(self.status.as_ref().map(|status| &status.state))
                } else {
                    AgentState::Background
                },
            };
        }
        if let Some((pid, runtime)) = foreground
            && process_runtime(pid) == Some(runtime)
        {
            return Occupancy::Agent {
                runtime,
                session: None,
                state: AgentState::Foreground(self.status.as_ref().map(|status| &status.state)),
            };
        }
        Occupancy::Terminal
    }

    fn session(&self) -> Option<&str> {
        match self.occupancy() {
            Occupancy::Agent { session, .. } => session,
            Occupancy::Terminal => None,
        }
    }

    fn pane(&self) -> DiscoveredPane {
        let (agent, session, status) = match self.occupancy() {
            Occupancy::Terminal => (None, None, None),
            Occupancy::Agent {
                runtime,
                session,
                state,
            } => (
                Some(
                    match runtime {
                        AgentRuntime::ClaudeCode => "claude",
                        AgentRuntime::Codex => "codex",
                    }
                    .to_owned(),
                ),
                session,
                match state {
                    AgentState::Foreground(status) => status.map(StatusWord::as_str),
                    AgentState::Background => Some("working"),
                },
            ),
        };
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
            agent_session_id: session.map(AgentSessionId::new),
            occupant_label: agent.clone(),
            agent,
            agent_status: status.map(str::to_owned),
            status_reported: Some(self.reported),
            idle_state_label: None,
        }
    }
}

// Naming operations take their lock first. Settlement projection drops this guard
// before running lifecycle work that can acquire the naming lock.
pub(crate) fn prompt_lock(state: &Path, core: &str, pane: &str) -> Result<std::fs::File, String> {
    use sha2::Digest;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let directory = state.join("hc-prompts");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|error| error.to_string())?;
    let key = serde_json::to_vec(&(core, pane)).expect("string pair serializes");
    let filename = format!("{}.lock", crate::lower_hex(&sha2::Sha256::digest(key)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(filename))
        .map_err(|error| error.to_string())?;
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
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

    pub fn core_identity(&self) -> Result<String, HostError> {
        #[derive(Deserialize)]
        struct Core {
            id: String,
        }
        #[derive(Deserialize)]
        struct Server {
            core: Core,
        }
        self.execute::<Server>(&["server", "status", "--json"], None)
            .map(|server| format!("hc:{}", server.core.id))
    }

    pub(crate) fn shell_pid(&self, terminal: &str, budget: &dyn Budget) -> Option<u32> {
        local_terminal(terminal).ok()?;
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
        local_terminal(pane)?;
        self.execute(&["get", pane, "--json"], budget)
    }

    pub(super) fn terminals(
        &self,
        budget: Option<&dyn Budget>,
    ) -> Result<Vec<Terminal>, HostError> {
        Ok(self.discover(budget)?.terminals)
    }

    pub(super) fn discover(
        &self,
        budget: Option<&dyn Budget>,
    ) -> Result<TerminalDiscovery, HostError> {
        let list: TerminalList = self.execute(&["ls", "--json"], budget)?;
        let mut discovery = TerminalDiscovery {
            terminals: Vec::new(),
            warnings: Vec::new(),
        };
        for summary in list
            .terminals
            .into_iter()
            .filter(|summary| summary.state == "live" && !summary.address.contains('/'))
        {
            match self.terminal(&summary.address, budget) {
                Ok(terminal) if terminal.state == "live" => discovery.terminals.push(terminal),
                Ok(_) => (),
                Err(error) => discovery.warnings.push(DiscoveryWarning {
                    terminal: summary.address,
                    message: error.message,
                }),
            }
        }
        Ok(discovery)
    }

    pub fn discovery_warnings(&self) -> Result<Vec<DiscoveryWarning>, HostError> {
        Ok(self.discover(None)?.warnings)
    }

    pub fn set_labels(&self, pane: &str, labels: &[(&str, &str)]) -> Result<(), HostError> {
        let terminal = self.terminal(pane, None)?;
        self.update_labels(&terminal, labels)
    }

    fn update_labels(&self, terminal: &Terminal, labels: &[(&str, &str)]) -> Result<(), HostError> {
        let changed: Vec<String> = labels
            .iter()
            .filter(|(key, value)| terminal.labels.get(*key).map_or("", String::as_str) != *value)
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        if changed.is_empty() {
            return Ok(());
        }
        let mut args = vec!["label", &terminal.address];
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
        let terminal_pid = terminal
            .pid
            .filter(|pid| *pid > 0)
            .ok_or("hc terminal has no live program to own a session".to_owned())?;
        let ancestors: Vec<_> = crate::terminal::ancestor_pids(std::process::id()).collect();
        let boundary = ancestors
            .iter()
            .position(|pid| *pid == terminal_pid)
            .ok_or("SessionStart is outside the hc terminal process tree".to_owned())?;
        let pid = ancestors[..=boundary]
            .iter()
            .copied()
            .find(|pid| process_runtime(*pid).is_some())
            .ok_or("hc terminal has no recognized agent ancestor to own a session".to_owned())?;
        let start = process_start(pid)
            .ok_or("Could not verify the agent process start identity".to_owned())?;
        self.update_labels(
            &terminal,
            &[
                ("namedrop.session", session),
                ("namedrop.session.pid", &pid.to_string()),
                ("namedrop.session.start", &start),
            ],
        )
    }

    pub fn invalidate_session(&self, state: &Path, pane: &str) -> Result<(), HostError> {
        let _lock = label_lock(state)?;
        let terminal = self.terminal(pane, None)?;
        if terminal.state == "live" && terminal.session().is_some() {
            return Ok(());
        }
        self.clear_session(&terminal)
    }

    fn clear_session(&self, terminal: &Terminal) -> Result<(), HostError> {
        self.update_labels(
            terminal,
            &[
                ("namedrop.session", ""),
                ("namedrop.session.pid", ""),
                ("namedrop.session.start", ""),
            ],
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
        let discovery = self.discover(budget)?;
        let terminals = discovery.terminals;
        let mut roots_by_cwd = BTreeMap::<PathBuf, Option<PathBuf>>::new();
        let mut cached_root = |cwd: &Path| {
            roots_by_cwd
                .entry(cwd.to_owned())
                .or_insert_with(|| git_root(cwd))
                .clone()
        };
        let workspace = match at {
            SpaceLocator::Workspace(id) => id.to_owned(),
            SpaceLocator::Checkout(cwd) => {
                if !discovery.warnings.is_empty() {
                    let failures = discovery
                        .warnings
                        .iter()
                        .map(|warning| format!("{}: {}", warning.terminal, warning.message))
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(format!(
                        "hc cannot resolve this checkout because terminal discovery is incomplete ({failures}). Pass --hc-space <space-id>."
                    )
                    .into());
                }
                let root = cached_root(cwd).ok_or_else(|| {
                    "hc cannot resolve this checkout. Pass --hc-space <space-id>.".to_owned()
                })?;
                let spaces: BTreeSet<&str> = terminals
                    .iter()
                    .filter(|terminal| {
                        terminal
                            .cwd()
                            .and_then(|cwd| cached_root(Path::new(cwd)))
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
            .map(|terminal| terminal.cwd().and_then(|cwd| cached_root(Path::new(cwd))))
            .collect();
        let root = roots
            .as_ref()
            .filter(|roots| !roots.is_empty() && roots.iter().all(|root| root == &roots[0]))
            .map(|roots| &roots[0]);
        Ok(DiscoveredWorktree {
            unavailable_panes: discovery
                .warnings
                .into_iter()
                .map(|warning| warning.terminal)
                .collect(),
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
        let shell_pid = terminal
            .pid
            .filter(|pid| *pid > 0 && process_is_shell(*pid))?;
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
        let skip = result
            .text
            .split_inclusive('\n')
            .count()
            .saturating_sub(lines);
        Ok(result.text.split_inclusive('\n').skip(skip).collect())
    }

    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptFailure> {
        let terminal = self.terminal(pane, None).map_err(pre_invocation)?;
        let occupancy = terminal.occupancy();
        if !matches!(
            occupancy,
            Occupancy::Agent {
                session: Some(_),
                ..
            }
        ) {
            self.clear_session(&terminal).map_err(pre_invocation)?;
        }
        let status = match occupancy {
            Occupancy::Agent {
                session: Some(_),
                state: AgentState::Foreground(Some(status @ (StatusWord::Idle | StatusWord::Done))),
                ..
            } if terminal.state == "live" => status,
            _ => {
                return Err(PromptFailure {
                    kind: PromptFailureKind::DeferredUntilSettle {
                        observed_status_ms: terminal.status_ms,
                    },
                    message: "hc terminal has no settled foreground session owner".to_owned(),
                });
            }
        };
        let args = if matches!(status, StatusWord::Idle) {
            vec!["send", pane, text, "--if", "idle", "--json"]
        } else {
            vec!["send", pane, text, "--json"]
        };
        self.transport
            .execute(&args)
            .map(|_| ())
            .map_err(|raw| prompt_failure(raw, PromptTarget::Agent(terminal.status_ms)))
    }

    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), PromptFailure> {
        let terminal = self.terminal(pane, None).map_err(pre_invocation)?;
        let shell_foreground = terminal
            .foreground
            .as_ref()
            .and_then(|foreground| u32::try_from(foreground.pid).ok())
            == terminal.pid;
        if terminal.state != "live"
            || !matches!(terminal.occupancy(), Occupancy::Terminal)
            || !shell_foreground
            || !terminal.pid.is_some_and(process_is_shell)
        {
            return Err(pre_invocation(
                "hc terminal is not an available shell".to_owned().into(),
            ));
        }
        self.transport
            .execute(&["send", pane, command, "--json"])
            .map(|_| ())
            .map_err(|raw| prompt_failure(raw, PromptTarget::Shell))
    }

    fn interrupt(&self, pane: &str) -> Result<(), HostError> {
        local_terminal(pane)?;
        self.transport
            .execute(&["interrupt", pane, "--timeout", "2", "--json"])
            .map(|_| ())
            .map_err(host_error)
    }

    fn report_metadata(&self, pane: &str, metadata: &PaneMetadata) -> Result<(), HostError> {
        match metadata {
            PaneMetadata::Identity { title, .. } => self.set_labels(
                pane,
                &[
                    ("namedrop.name", title.strip_prefix('@').unwrap_or(title)),
                    ("namedrop.name.error", ""),
                ],
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
        self.core_identity().ok()
    }
    fn caller_identity(&self, claim: &CallerClaim) -> Result<CallerIdentity, HostError> {
        if let Some(pane) = &claim.pane {
            local_terminal(pane)?;
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
        use std::os::unix::process::CommandExt;
        let mut agent = Command::new("/bin/sleep")
            .arg0("codex")
            .arg("600")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while process_runtime(agent.id()) != Some(AgentRuntime::Codex) {
            assert!(
                std::time::Instant::now() < deadline,
                "fake agent did not exec"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        record["labels"]["namedrop.session.pid"] = serde_json::json!(agent.id().to_string());
        record["labels"]["namedrop.session.start"] =
            serde_json::json!(process_start(agent.id()).unwrap());
        record["foreground"] =
            serde_json::json!({"pid":agent.id(),"argv":["codex"],"name":"codex"});
        record["status"] = word
            .map(|state| serde_json::json!({"state":state,"app":"codex"}))
            .unwrap_or_default();
        record["reported"] = serde_json::json!(word.is_some());
        let pane = serde_json::from_value::<Terminal>(record).unwrap().pane();
        agent.kill().unwrap();
        agent.wait().unwrap();
        pane
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
        struct Executor(std::cell::RefCell<Vec<Vec<String>>>, u32);
        impl HerdrCommandExecutor for Executor {
            fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
                self.0
                    .borrow_mut()
                    .push(args.iter().map(|arg| (*arg).to_owned()).collect());
                if matches!(args, ["get", ..]) {
                    let mut terminal: serde_json::Value = serde_json::from_str(include_str!(
                        "../../tests/fixtures/hc/get-labeled.stdout"
                    ))
                    .unwrap();
                    terminal["pid"] = serde_json::json!(self.1);
                    terminal["foreground"]["pid"] = serde_json::json!(self.1);
                    return Ok(crate::HerdrCommandOutput {
                        stdout: terminal.to_string(),
                    });
                }
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
        let mut shell = Command::new("/bin/bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "printf 'ready\\n'; read -r _",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(shell.stdout.take().unwrap()),
            &mut ready,
        )
        .unwrap();
        assert_eq!(ready, "ready\n");
        let executor = Executor(std::cell::RefCell::default(), shell.id());
        let host = HcHost::new(&executor);
        host.panes(PaneScope::All).unwrap();
        host.read("t1", 40, None).unwrap();
        host.read("t1", 1000, None).unwrap();
        assert!(host.prompt("t1", "hello").is_err());
        host.run_in_pane("t1", "command").unwrap();
        shell.kill().unwrap();
        shell.wait().unwrap();
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
                vec!["get", "t1", "--json"],
                vec!["label", "t1", "namedrop.session=", "--json"],
                vec!["get", "t1", "--json"],
                vec!["send", "t1", "command", "--json"],
                vec!["interrupt", "t1", "--timeout", "2", "--json"],
                vec!["get", "t1", "--json"],
                vec!["label", "t1", "namedrop.session=next-session", "--json"],
                vec!["server", "status", "--json"],
            ]
        );
    }
}
