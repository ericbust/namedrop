use super::*;
use crate::{AgentSessionId, HerdrCommandExecutor, HerdrCommandOutput, WorktreeId};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct TuiHost<E: HerdrCommandExecutor> {
    transport: E,
}

impl<E: HerdrCommandExecutor> TuiHost<E> {
    pub fn new(transport: E) -> Self {
        Self { transport }
    }

    fn execute(
        &self,
        args: &[&str],
        budget: Option<&dyn Budget>,
    ) -> Result<HerdrCommandOutput, HostError> {
        let result = match budget {
            Some(budget) => self.transport.execute_bounded(
                args,
                budget.remaining()?,
                budget.cancelled(),
                budget.max_output_bytes(),
            ),
            None => self.transport.execute(args),
        };
        result.map_err(|message| HostError {
            code: herdr_error_code(&message),
            message,
        })
    }
    fn discover_worktree(
        &self,
        cwd: &Path,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        let cwd = cwd
            .to_str()
            .ok_or_else(|| "Worktree path must be valid UTF-8".to_owned())?;
        let worktree_output =
            self.execute(&["worktree", "list", "--cwd", cwd, "--json"], budget)?;
        let worktrees: WorktreeListEnvelope =
            serde_json::from_str(&worktree_output.stdout).map_err(|error| error.to_string())?;
        let WorktreeListResult { source, worktrees } = worktrees.result;
        let worktree = worktrees
            .into_iter()
            .find(|worktree| Path::new(&worktree.path) == Path::new(cwd))
            .ok_or_else(|| format!("Herdr did not resolve {cwd} as a Worktree"))?;
        let workspace_id = worktree
            .open_workspace_id
            .clone()
            .ok_or_else(|| format!("Worktree {} is not open in Herdr", worktree.path))?;
        let repo_key = source
            .as_ref()
            .and_then(|source| source.repo_key.as_deref());
        let (owners, unknown) =
            self.spaces_owning_checkout(&worktree, repo_key, &workspace_id, budget)?;
        if owners.len() > 1 || !unknown.is_empty() {
            let open_in = match owners.as_slice() {
                [owner] => format!("Herdr space {owner}"),
                owners => format!("more than one Herdr space ({})", owners.join(", ")),
            };
            let suspects = if unknown.is_empty() {
                String::new()
            } else {
                format!(
                    " and may be open in {}, whose checkout Herdr does not report",
                    unknown.join(", ")
                )
            };
            return Err(format!(
                "Checkout {} is open in {open_in}{suspects}, so Namedrop can't tell which one \
             this command is for. Pass --herdr-workspace <space-id> (or --herdr-pane \
             <pane-id>).",
                worktree.path
            )
            .into());
        }
        self.discover_panes(
            worktree.path,
            workspace_id,
            true,
            worktree.label,
            worktree.branch,
            budget,
        )
    }

    // `worktree list --cwd` names only the first opener. Returns the spaces proven
    // to own `checkout`, and, for a linked checkout, the spaces in the same
    // repository whose checkout Herdr does not report.
    fn spaces_owning_checkout(
        &self,
        checkout: &WorktreeEntry,
        repo_key: Option<&str>,
        opener: &str,
        budget: Option<&dyn Budget>,
    ) -> Result<(Vec<String>, Vec<String>), HostError> {
        let output = self.execute(&["workspace", "list"], budget)?;
        let workspaces: WorkspaceListEnvelope =
            serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
        let mut owners = vec![opener.to_owned()];
        let mut unknown = Vec::new();
        for workspace in workspaces.result.workspaces {
            if workspace.workspace_id == opener {
                continue;
            }
            // Herdr stamps `worktree` only through `worktree open` and
            // `worktree create`, and then it names the space's own checkout.
            if let Some(stamped) = workspace.worktree {
                if Path::new(&stamped.checkout_path) == Path::new(&checkout.path) {
                    owners.push(workspace.workspace_id);
                }
                continue;
            }
            let listing = match self.worktree_list_for_workspace(&workspace.workspace_id, budget) {
                Ok(listing) => listing,
                Err(error) if error.code.as_deref() == Some("workspace_not_found") => None,
                Err(error) => return Err(error),
            };
            let Some(result) = listing else {
                continue;
            };
            let other_repo = result
                .source
                .as_ref()
                .and_then(|source| source.repo_key.as_deref())
                .zip(repo_key)
                .is_some_and(|(theirs, ours)| theirs != ours);
            if other_repo {
                continue;
            }
            match Self::owned_worktree_index(&result, &workspace.workspace_id) {
                Some(index)
                    if Path::new(&result.worktrees[index].path) == Path::new(&checkout.path) =>
                {
                    owners.push(workspace.workspace_id);
                }
                Some(_) => {}
                // A space on the repository root is always placed, as opener or
                // through `source`, so only a checkout not known to be the root
                // can hide an owner.
                None if checkout.is_linked_worktree != Some(false) => {
                    unknown.push(workspace.workspace_id)
                }
                None => {}
            }
        }
        owners.sort();
        unknown.sort();
        Ok((owners, unknown))
    }

    fn workspace_labels(
        &self,
        budget: Option<&dyn Budget>,
    ) -> Result<Vec<ListedWorkspace>, HostError> {
        let output = self.execute(&["workspace", "list"], budget)?;
        let mut workspaces = serde_json::from_str::<WorkspaceListEnvelope>(&output.stdout)
            .map_err(|error| error.to_string())?
            .result
            .workspaces;
        workspaces.sort_by_key(|workspace| workspace.number);
        Ok(workspaces
            .into_iter()
            .map(|workspace| {
                let label = workspace
                    .label
                    .unwrap_or_else(|| workspace.workspace_id.clone());
                ListedWorkspace {
                    id: workspace.workspace_id,
                    label,
                    repo: workspace.worktree.and_then(|worktree| worktree.repo_name),
                }
            })
            .collect())
    }

    fn worktree_list_for_workspace(
        &self,
        workspace_id: &str,
        budget: Option<&dyn Budget>,
    ) -> Result<Option<WorktreeListResult>, HostError> {
        let output = match self.execute(
            &["worktree", "list", "--workspace", workspace_id, "--json"],
            budget,
        ) {
            Ok(output) => output,
            Err(error) if error.code.as_deref() == Some("not_git_worktree") => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let worktrees: WorktreeListEnvelope =
            serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
        Ok(Some(worktrees.result))
    }

    fn owned_worktree_index(result: &WorktreeListResult, workspace_id: &str) -> Option<usize> {
        result
            .worktrees
            .iter()
            .position(|worktree| worktree.open_workspace_id.as_deref() == Some(workspace_id))
            .or_else(|| {
                // Herdr lists each checkout path once with its first opener as
                // open_workspace_id, so a second workspace on the same path never
                // matches by id. `source` names the requester only when it sits on
                // the repo root, and then source_checkout_path is its checkout.
                let source = result
                    .source
                    .as_ref()
                    .filter(|source| source.source_workspace_id.as_deref() == Some(workspace_id))?;
                let checkout = source.source_checkout_path.as_deref()?;
                result
                    .worktrees
                    .iter()
                    .position(|worktree| Path::new(&worktree.path) == Path::new(checkout))
            })
    }

    fn discover_worktree_for_workspace(
        &self,
        workspace_id: &str,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        let Some(mut result) = self.worktree_list_for_workspace(workspace_id, budget)? else {
            return self.discover_panes(
                format!("herdr-space:{workspace_id}"),
                workspace_id.to_owned(),
                false,
                None,
                None,
                budget,
            );
        };
        let index = Self::owned_worktree_index(&result, workspace_id);
        match index.map(|index| result.worktrees.swap_remove(index)) {
            Some(worktree) => self.discover_panes(
                worktree.path,
                workspace_id.to_owned(),
                true,
                worktree.label,
                worktree.branch,
                budget,
            ),
            None => Err(format!(
                "Herdr returned a Worktree snapshot that does not contain space {workspace_id}"
            )
            .into()),
        }
    }

    fn discover_panes(
        &self,
        worktree_path: String,
        workspace_id: String,
        worktree_attached: bool,
        label: Option<String>,
        branch: Option<String>,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        let pane_output = self.execute(&["pane", "list", "--workspace", &workspace_id], budget)?;
        let panes: PaneListEnvelope =
            serde_json::from_str(&pane_output.stdout).map_err(|error| error.to_string())?;
        let panes = Self::map_panes(panes);
        Ok(DiscoveredWorktree {
            worktree_id: WorktreeId::new(worktree_path),
            workspace_id,
            worktree_attached,
            label,
            branch,
            panes,
        })
    }

    fn map_panes(panes: PaneListEnvelope) -> Vec<DiscoveredPane> {
        panes
            .result
            .panes
            .into_iter()
            .map(|pane| DiscoveredPane {
                pane_id: pane.pane_id,
                terminal_id: pane.terminal_id,
                tab_id: pane.tab_id,
                workspace_id: pane.workspace_id,
                cwd: pane.cwd,
                pane_label: pane.label.or(pane.title).or(pane.terminal_title_stripped),
                agent_session_id: pane
                    .agent_session
                    .map(|session| AgentSessionId::new(session.value)),
                agent: pane.agent.clone(),
                occupant_label: pane.display_agent.or(pane.agent),
                agent_status: pane.agent_status,
                idle_state_label: pane
                    .state_labels
                    .and_then(|mut labels| labels.remove("idle")),
            })
            .collect()
    }
    /// A pane ID inherited from the environment only identifies this process when
    /// the process runs inside that pane, under its shell, or its agent session is
    /// the one Herdr says runs in that pane (Claude Code's Bash can sit outside the
    /// shell's process tree). A daemon keeps the variables of the pane it was first
    /// started from, so the inherited value can name a pane the caller left long
    /// ago. Acting as that pane is a silent identity error, so the check refuses
    /// instead of falling back. When Herdr does not answer, nothing was learned
    /// about the pane, so its own error is the reason given.
    pub fn verify_inherited_pane(
        &self,
        pane_id: &str,
        agent_session_ids: &[&str],
    ) -> Result<(), String> {
        let remedy = "Pass --herdr-pane <pane-id> to say which pane this command acts for.";
        let output = self
            .execute(&["pane", "process-info", "--pane", pane_id], None)
            .map_err(|error| {
                format!("could not verify the inherited HERDR_PANE_ID={pane_id}: {error} {remedy}")
            })?;
        let shell_pid = parse_process_info(&output.stdout)
            .map(|p| p.shell_pid)
            .filter(|pid| *pid > 0)
            .ok_or_else(|| {
                format!(
                    "HERDR_PANE_ID={pane_id} was inherited from the environment, but Herdr reports \
                 no shell for pane {pane_id}, so the pane could not be verified. {remedy}"
                )
            })?;
        let caller = std::process::id();
        if crate::terminal::ancestor_pids(caller).any(|pid| u64::from(pid) == shell_pid) {
            return Ok(());
        }
        if agent_session_ids.iter().any(|session| {
            crate::locate_agent_session_pane(self, session).is_some_and(|(_, pane)| pane == pane_id)
        }) {
            return Ok(());
        }
        let sessions = if agent_session_ids.is_empty() {
            "There is no CODEX_SESSION_ID or CLAUDE_CODE_SESSION_ID in the environment to vouch \
         for it."
                .to_owned()
        } else {
            format!(
                "None of the {} agent session ID(s) in the environment runs in pane {pane_id}.",
                agent_session_ids.len()
            )
        };
        Err(format!(
            "HERDR_PANE_ID={pane_id} was inherited from the environment, but this process (pid \
         {caller}) is not running inside pane {pane_id}: the pane's shell (pid {shell_pid}) is \
         not one of its ancestors. {sessions} A background daemon keeps the pane variables it \
         was started with. {remedy}"
        ))
    }
}
fn parse_process_info(stdout: &str) -> Option<ProcessInfo> {
    serde_json::from_str::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| {
            serde_json::from_value::<ProcessInfo>(v["result"]["process_info"].clone()).ok()
        })
}

fn herdr_error_code(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error).ok().and_then(|value| {
        value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

#[derive(Deserialize)]
struct WorktreeListEnvelope {
    result: WorktreeListResult,
}

#[derive(Deserialize)]
struct WorktreeListResult {
    source: Option<WorktreeListSource>,
    worktrees: Vec<WorktreeEntry>,
}

#[derive(Deserialize)]
struct WorktreeListSource {
    repo_key: Option<String>,
    source_checkout_path: Option<String>,
    source_workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct WorktreeEntry {
    path: String,
    is_linked_worktree: Option<bool>,
    open_workspace_id: Option<String>,
    label: Option<String>,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceListEnvelope {
    result: WorkspaceListResult,
}

#[derive(Deserialize)]
struct WorkspaceListResult {
    workspaces: Vec<WorkspaceEntry>,
}

#[derive(Deserialize)]
struct WorkspaceEntry {
    workspace_id: String,
    label: Option<String>,
    number: Option<u64>,
    worktree: Option<WorkspaceWorktree>,
}

#[derive(Deserialize)]
struct WorkspaceWorktree {
    checkout_path: String,
    repo_name: Option<String>,
}

#[derive(Deserialize)]
struct PaneListEnvelope {
    result: PaneListResult,
}

#[derive(Deserialize)]
struct PaneListResult {
    panes: Vec<PaneEntry>,
}

#[derive(Deserialize)]
struct PaneEntry {
    pane_id: String,
    terminal_id: String,
    tab_id: String,
    workspace_id: String,
    cwd: Option<String>,
    label: Option<String>,
    title: Option<String>,
    terminal_title_stripped: Option<String>,
    agent: Option<String>,
    display_agent: Option<String>,
    agent_session: Option<AgentSessionEntry>,
    agent_status: Option<String>,
    state_labels: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Deserialize)]
struct AgentSessionEntry {
    value: String,
}
impl<E: HerdrCommandExecutor> Host for TuiHost<E> {
    fn space(
        &self,
        at: SpaceLocator<'_>,
        budget: Option<&dyn Budget>,
    ) -> Result<DiscoveredWorktree, HostError> {
        match at {
            SpaceLocator::Checkout(cwd) => self.discover_worktree(cwd, budget),
            SpaceLocator::Workspace(workspace) => {
                self.discover_worktree_for_workspace(workspace, budget)
            }
        }
    }
    fn spaces(&self) -> Result<Vec<ListedWorkspace>, HostError> {
        self.workspace_labels(None)
    }
    fn panes(&self, scope: PaneScope<'_>) -> Result<Vec<DiscoveredPane>, HostError> {
        let output = match scope {
            PaneScope::All => self.execute(&["pane", "list"], None)?,
            PaneScope::Workspace(workspace) => {
                self.execute(&["pane", "list", "--workspace", workspace], None)?
            }
        };
        let panes = serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
        Ok(Self::map_panes(panes))
    }
    fn process_info(&self, pane: &str) -> Option<ProcessInfo> {
        self.execute(&["pane", "process-info", "--pane", pane], None)
            .ok()
            .and_then(|out| parse_process_info(&out.stdout))
    }
    fn read(
        &self,
        pane: &str,
        lines: usize,
        budget: Option<&dyn Budget>,
    ) -> Result<String, HostError> {
        self.execute(
            &[
                "pane",
                "read",
                pane,
                "--source",
                "recent-unwrapped",
                "--lines",
                &lines.to_string(),
            ],
            budget,
        )
        .map(|out| out.stdout)
    }
    fn prompt(&self, pane: &str, text: &str) -> Result<(), PromptFailure> {
        self.transport
            .execute(&["agent", "prompt", pane, text])
            .map(|_| ())
            .map_err(|message| PromptFailure {
                kind: self.transport.classify_prompt_failure(&message),
                message,
            })
    }
    fn run_in_pane(&self, pane: &str, command: &str) -> Result<(), PromptFailure> {
        self.transport
            .execute(&["pane", "run", pane, command])
            .map(|_| ())
            .map_err(|message| PromptFailure {
                kind: self.transport.classify_prompt_failure(&message),
                message,
            })
    }
    fn interrupt(&self, pane: &str) -> Result<(), HostError> {
        // Herdr key-combo syntax joins a modifier to its key with
        // `+`; it answers `ctrl-c` with `invalid_key`.
        self.execute(&["agent", "send-keys", pane, "ctrl+c"], None)
            .map(|_| ())
    }
    fn report_metadata(&self, pane: &str, metadata: &PaneMetadata) -> Result<(), HostError> {
        match metadata {
            PaneMetadata::Identity {
                title,
                ready,
                working,
                blocked,
                unknown,
            } => self.execute(
                &[
                    "pane",
                    "report-metadata",
                    pane,
                    "--source",
                    "namedrop-identity",
                    "--title",
                    title,
                    "--state-label",
                    &format!("idle={ready}"),
                    "--state-label",
                    &format!("done={ready}"),
                    "--state-label",
                    &format!("working={working}"),
                    "--state-label",
                    &format!("blocked={blocked}"),
                    "--state-label",
                    &format!("unknown={unknown}"),
                ],
                None,
            ),
            PaneMetadata::Counters {
                awaiting,
                attention,
            } => self.execute(
                &[
                    "pane",
                    "report-metadata",
                    pane,
                    "--source",
                    "namedrop",
                    "--token",
                    &format!("namedrop-awaiting={awaiting}"),
                    "--token",
                    &format!("namedrop-attention={attention}"),
                ],
                None,
            ),
            PaneMetadata::Relay { text, ttl_ms } => self.execute(
                &[
                    "pane",
                    "report-metadata",
                    pane,
                    "--source",
                    "namedrop-relay",
                    "--token",
                    &format!("namedrop-relay={text}"),
                    "--ttl-ms",
                    &ttl_ms.to_string(),
                ],
                None,
            ),
        }
        .map(|_| ())
    }
    fn notify(&self, title: &str, body: &str, sound: bool) -> Result<(), HostError> {
        let mut args = vec![
            "notification",
            "show",
            title,
            "--body",
            body,
            "--position",
            "bottom-right",
        ];
        if sound {
            args.extend(["--sound", "done"]);
        }
        self.execute(&args, None).map(|_| ())
    }
    fn tab_label(&self, tab: &str) -> Option<String> {
        let output = self.execute(&["tab", "get", tab], None).ok()?;
        let value: Value = serde_json::from_str(&output.stdout).ok()?;
        value
            .pointer("/result/label")
            .or_else(|| value.pointer("/result/tab/label"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    fn rename_tab(&self, tab: &str, label: &str) -> Result<(), HostError> {
        self.execute(&["tab", "rename", tab, label], None)
            .map(|_| ())
    }
    fn open_popup(&self, popup: &Popup<'_>) -> Result<Value, HostError> {
        let mut args = vec![
            "plugin".to_owned(),
            "pane".to_owned(),
            "open".to_owned(),
            "--plugin".to_owned(),
            "namedrop".to_owned(),
            "--entrypoint".to_owned(),
            popup.entrypoint.to_owned(),
            "--placement".to_owned(),
            "popup".to_owned(),
            "--width".to_owned(),
            popup.width.to_owned(),
            "--height".to_owned(),
            popup.height.to_owned(),
        ];
        for (key, value) in popup.env {
            args.extend(["--env".to_owned(), format!("{key}={value}")]);
        }
        args.push("--focus".to_owned());
        let output = self.execute(&args.iter().map(String::as_str).collect::<Vec<_>>(), None)?;
        serde_json::from_str(&output.stdout)
            .map_err(|error| format!("Herdr returned an unreadable popup result: {error}").into())
    }
    fn reload_config(&self) -> Result<(), HostError> {
        self.execute(&["server", "reload-config"], None).map(|_| ())
    }
    fn plugin_root(&self) -> Option<PathBuf> {
        let output = self
            .transport
            .execute_bounded(
                &["plugin", "list", "--plugin", "namedrop", "--json"],
                Duration::from_secs(2),
                &AtomicBool::new(false),
                64 * 1024,
            )
            .ok()?;
        let list: Value = serde_json::from_str(&output.stdout).ok()?;
        let root = list["result"]["plugins"]
            .as_array()?
            .iter()
            .find(|plugin| plugin["plugin_id"] == "namedrop")?["plugin_root"]
            .as_str()?;
        Some(PathBuf::from(root)).filter(|root| root.is_absolute())
    }
    fn socket_id(&self) -> Option<String> {
        self.execute(&["status", "server"], None)
            .ok()
            .and_then(|output| {
                output
                    .stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("socket: ").map(str::to_owned))
            })
    }
    fn caller_identity(&self, claim: &CallerClaim) -> Result<CallerIdentity, HostError> {
        let mut workspace = claim.workspace.clone();
        // An explicit --herdr-pane is the caller's assertion. An inherited
        // HERDR_PANE_ID can be stale: a daemon keeps the variables of the pane it
        // started in, so it only counts when this process still runs under that
        // pane's shell.
        let mut pane = match claim.pane.clone() {
            Some(pane) => Some(pane),
            None => match claim.inherited_pane.clone() {
                Some(pane) => {
                    let sessions = claim
                        .agent_sessions
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    self.verify_inherited_pane(&pane, &sessions)?;
                    Some(pane)
                }
                None => None,
            },
        };
        if workspace.is_none()
            && let Some(pane) = pane.as_deref()
        {
            workspace = Some(crate::pane_workspace(self, pane)?);
        }
        // An agent can run commands without the pane's Herdr variables (a shared
        // Codex app-server keeps the environment it started with). The agent's
        // own session ID still arrives, and Herdr knows which pane runs it.
        if workspace.is_none()
            && let Some((space, found_pane)) = claim
                .agent_sessions
                .iter()
                .find(|session| !session.is_empty())
                .and_then(|session| crate::locate_agent_session_pane(self, session))
        {
            workspace = Some(space);
            pane = pane.or(Some(found_pane));
        }
        Ok(CallerIdentity { workspace, pane })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingTransport {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl HerdrCommandExecutor for RecordingTransport {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            let stdout = if args == ["status", "server"] {
                "socket: /test/socket\n".to_owned()
            } else {
                serde_json::json!({"result": {
                    "worktrees": [{"path":"/repo","open_workspace_id":"w1"}],
                    "workspaces": [{"workspace_id":"w1","number":1,"label":"Main"}],
                    "panes": [{"pane_id":"w1:p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent_session":{"value":"session1"}}],
                    "process_info": {"shell_pid":std::process::id(),"foreground_process_group_id":1},
                    "label": "Main",
                    "plugins": [{"plugin_id":"namedrop","plugin_root":"/plugin"}]
                }}).to_string()
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }

    fn assert_calls(transport: &RecordingTransport, expected: &[&[&str]]) {
        assert_eq!(*transport.calls.borrow(), expected);
        transport.calls.borrow_mut().clear();
    }

    #[test]
    fn golden_argv() {
        let transport = RecordingTransport::default();
        let host = TuiHost::new(&transport);
        host.space(SpaceLocator::Checkout(Path::new("/repo")), None)
            .unwrap();
        assert_calls(
            &transport,
            &[
                &["worktree", "list", "--cwd", "/repo", "--json"],
                &["workspace", "list"],
                &["pane", "list", "--workspace", "w1"],
            ],
        );
        host.space(SpaceLocator::Workspace("w1"), None).unwrap();
        assert_calls(
            &transport,
            &[
                &["worktree", "list", "--workspace", "w1", "--json"],
                &["pane", "list", "--workspace", "w1"],
            ],
        );
        host.spaces().unwrap();
        assert_calls(&transport, &[&["workspace", "list"]]);
        host.panes(PaneScope::All).unwrap();
        assert_calls(&transport, &[&["pane", "list"]]);
        host.panes(PaneScope::Workspace("w1")).unwrap();
        assert_calls(&transport, &[&["pane", "list", "--workspace", "w1"]]);
        host.process_info("w1:p1").unwrap();
        assert_calls(&transport, &[&["pane", "process-info", "--pane", "w1:p1"]]);
        host.read("w1:p1", 40, None).unwrap();
        assert_calls(
            &transport,
            &[&[
                "pane",
                "read",
                "w1:p1",
                "--source",
                "recent-unwrapped",
                "--lines",
                "40",
            ]],
        );
        host.prompt("w1:p1", "prompt").unwrap();
        assert_calls(&transport, &[&["agent", "prompt", "w1:p1", "prompt"]]);
        host.run_in_pane("w1:p1", "command").unwrap();
        assert_calls(&transport, &[&["pane", "run", "w1:p1", "command"]]);
        host.interrupt("w1:p1").unwrap();
        assert_calls(&transport, &[&["agent", "send-keys", "w1:p1", "ctrl+c"]]);
        host.report_metadata(
            "w1:p1",
            &PaneMetadata::Identity {
                title: "@agent".into(),
                ready: "@agent ready".into(),
                working: "@agent working".into(),
                blocked: "@agent needs attention".into(),
                unknown: "@agent status unknown".into(),
            },
        )
        .unwrap();
        assert_calls(
            &transport,
            &[&[
                "pane",
                "report-metadata",
                "w1:p1",
                "--source",
                "namedrop-identity",
                "--title",
                "@agent",
                "--state-label",
                "idle=@agent ready",
                "--state-label",
                "done=@agent ready",
                "--state-label",
                "working=@agent working",
                "--state-label",
                "blocked=@agent needs attention",
                "--state-label",
                "unknown=@agent status unknown",
            ]],
        );
        host.report_metadata(
            "w1:p1",
            &PaneMetadata::Counters {
                awaiting: 2,
                attention: 3,
            },
        )
        .unwrap();
        assert_calls(
            &transport,
            &[&[
                "pane",
                "report-metadata",
                "w1:p1",
                "--source",
                "namedrop",
                "--token",
                "namedrop-awaiting=2",
                "--token",
                "namedrop-attention=3",
            ]],
        );
        host.report_metadata(
            "w1:p1",
            &PaneMetadata::Relay {
                text: "@agent: reply".into(),
                ttl_ms: 15000,
            },
        )
        .unwrap();
        assert_calls(
            &transport,
            &[&[
                "pane",
                "report-metadata",
                "w1:p1",
                "--source",
                "namedrop-relay",
                "--token",
                "namedrop-relay=@agent: reply",
                "--ttl-ms",
                "15000",
            ]],
        );
        host.notify("@agent replied", "reply", true).unwrap();
        assert_calls(
            &transport,
            &[&[
                "notification",
                "show",
                "@agent replied",
                "--body",
                "reply",
                "--position",
                "bottom-right",
                "--sound",
                "done",
            ]],
        );
        host.notify("Namedrop", "Sent to @agent", false).unwrap();
        assert_calls(
            &transport,
            &[&[
                "notification",
                "show",
                "Namedrop",
                "--body",
                "Sent to @agent",
                "--position",
                "bottom-right",
            ]],
        );
        assert_eq!(host.tab_label("tab1").as_deref(), Some("Main"));
        assert_calls(&transport, &[&["tab", "get", "tab1"]]);
        host.rename_tab("tab1", "@agent").unwrap();
        assert_calls(&transport, &[&["tab", "rename", "tab1", "@agent"]]);
        let env = [
            ("NAMEDROP_TARGET_WORKSPACE", "w1".to_owned()),
            ("NAMEDROP_TARGET_PANE", "w1:p1".to_owned()),
            ("NAMEDROP_TARGET_TERMINAL", "t1".to_owned()),
            ("NAMEDROP_TARGET_SESSION", "session1".to_owned()),
            ("HERDR_PLUGIN_STATE_DIR", "/state".to_owned()),
            ("HERDR_SOCKET_PATH", "/test/socket".to_owned()),
        ];
        for (entrypoint, width, height) in [
            ("naming", "72", "20"),
            ("management", "72", "24"),
            ("doctor", "72", "24"),
            ("agent-status", "84", "24"),
            ("tour", "72", "24"),
        ] {
            host.open_popup(&Popup {
                entrypoint,
                width,
                height,
                env: &env,
            })
            .unwrap();
            assert_calls(
                &transport,
                &[&[
                    "plugin",
                    "pane",
                    "open",
                    "--plugin",
                    "namedrop",
                    "--entrypoint",
                    entrypoint,
                    "--placement",
                    "popup",
                    "--width",
                    width,
                    "--height",
                    height,
                    "--env",
                    "NAMEDROP_TARGET_WORKSPACE=w1",
                    "--env",
                    "NAMEDROP_TARGET_PANE=w1:p1",
                    "--env",
                    "NAMEDROP_TARGET_TERMINAL=t1",
                    "--env",
                    "NAMEDROP_TARGET_SESSION=session1",
                    "--env",
                    "HERDR_PLUGIN_STATE_DIR=/state",
                    "--env",
                    "HERDR_SOCKET_PATH=/test/socket",
                    "--focus",
                ]],
            );
        }
        let target = host.panes(PaneScope::All).unwrap().remove(0);
        transport.calls.borrow_mut().clear();
        crate::tour::open_popup(
            &crate::LifecycleRuntime {
                state_directory: Path::new("/state"),
                home_directory: None,
                agent_config: crate::AgentConfigDirectories::default(),
                cwd: Path::new("/repo"),
                workspace_id: Some("w1"),
                current_pane_id: Some("w1:p1"),
                session_socket_id: "/test/socket",
                host,
                clock: &(),
                ids: &(),
            },
            &target,
        )
        .unwrap();
        assert_calls(
            &transport,
            &[&[
                "plugin",
                "pane",
                "open",
                "--plugin",
                "namedrop",
                "--entrypoint",
                "tour",
                "--placement",
                "popup",
                "--width",
                "72",
                "--height",
                "24",
                "--env",
                "NAMEDROP_TARGET_WORKSPACE=w1",
                "--env",
                "NAMEDROP_TARGET_PANE=w1:p1",
                "--env",
                "NAMEDROP_TARGET_TERMINAL=t1",
                "--env",
                "NAMEDROP_TARGET_SESSION=session1",
                "--env",
                "HERDR_PLUGIN_STATE_DIR=/state",
                "--env",
                "HERDR_SOCKET_PATH=/test/socket",
                "--focus",
            ]],
        );
        host.reload_config().unwrap();
        assert_calls(&transport, &[&["server", "reload-config"]]);
        assert_eq!(host.plugin_root(), Some(PathBuf::from("/plugin")));
        assert_calls(
            &transport,
            &[&["plugin", "list", "--plugin", "namedrop", "--json"]],
        );
        assert_eq!(host.socket_id().as_deref(), Some("/test/socket"));
        assert_calls(&transport, &[&["status", "server"]]);
        let identity = host
            .caller_identity(&CallerClaim {
                workspace: None,
                pane: Some("w1:p1".into()),
                inherited_pane: None,
                agent_sessions: vec![],
            })
            .unwrap();
        assert_eq!(identity.workspace.as_deref(), Some("w1"));
        assert_calls(&transport, &[&["pane", "list"]]);
        let identity = host
            .caller_identity(&CallerClaim {
                workspace: None,
                pane: None,
                inherited_pane: None,
                agent_sessions: vec!["session1".into()],
            })
            .unwrap();
        assert_eq!(identity.pane.as_deref(), Some("w1:p1"));
        assert_calls(&transport, &[&["pane", "list"]]);
        host.caller_identity(&CallerClaim {
            workspace: Some("w1".into()),
            pane: None,
            inherited_pane: Some("w1:p1".into()),
            agent_sessions: vec!["session1".into()],
        })
        .unwrap();
        assert_calls(
            &transport,
            &[
                &["pane", "process-info", "--pane", "w1:p1"],
                &["pane", "list"],
            ],
        );
    }
}
