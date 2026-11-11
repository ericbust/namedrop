//! Herdr arguments, replies, and error codes, copied from the callers that
//! still build them inline until they move to the port.

use super::Transport;
use crate::{
    AgentRuntime, AgentSessionId, PromptFailureKind, SpaceId, WorktreeId,
    mux::{
        AgentIdentity, AgentKind, AgentState, Badge, Checkout, Effect, Gone, Key, MuxError,
        PaneFacts, PaneId, PaneProcess, PopupScreen, PopupSpec, Recipient, ScreenRequest,
        SessionClaim, SpaceView, Style, TabId, TerminalId, Wrap,
    },
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub(super) fn error_code(error: &str) -> Option<String> {
    serde_json::from_str::<Value>(error).ok().and_then(|value| {
        value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

pub(super) fn mux_error(error: String) -> MuxError {
    match error_code(&error).as_deref() {
        Some("workspace_not_found") => MuxError::Gone(Gone::Space),
        Some("pane_not_found") => MuxError::Gone(Gone::Pane),
        _ if error.starts_with("Herdr command timed out") => MuxError::Timeout,
        _ if error == "Herdr read exceeded its output limit" => MuxError::TooLarge,
        _ => MuxError::Unavailable(error),
    }
}

/// The same table as `ProcessHerdr::classify_prompt_failure`.
pub(super) fn prompt_failure(error: &str) -> PromptFailureKind {
    match error_code(error).as_deref() {
        // `herdr agent prompt --help` (0.8.2): "If the agent is already
        // blocked, submission is rejected with `agent_blocked` before any
        // input is sent."
        Some("agent_not_found" | "pane_not_found" | "agent_busy" | "agent_blocked") => {
            PromptFailureKind::TemporaryPreInvocation
        }
        Some("agent_not_ready" | "conflict" | "server_busy") => {
            PromptFailureKind::TemporaryPreInvocationWithoutEvent
        }
        Some(
            "invalid_request" | "invalid_args" | "invalid_input" | "unsupported"
            | "unknown_command",
        ) => PromptFailureKind::DefinitiveNonRetryable,
        _ => PromptFailureKind::AmbiguousInvocation,
    }
}

pub(super) fn run(transport: &impl Transport, args: &[&str]) -> Result<String, MuxError> {
    transport.run(args).map_err(mux_error)
}

fn parse<T: DeserializeOwned>(stdout: &str) -> Result<T, MuxError> {
    serde_json::from_str(stdout).map_err(|error| MuxError::Protocol(error.to_string()))
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
struct WorkspaceGetEnvelope {
    result: WorkspaceGetResult,
}

#[derive(Deserialize)]
struct WorkspaceGetResult {
    workspace: WorkspaceGet,
}

#[derive(Deserialize)]
struct WorkspaceGet {
    label: Option<String>,
    number: Option<u64>,
    worktree: Option<WorkspaceWorktree>,
}

#[derive(Deserialize)]
struct WorkspaceWorktree {
    checkout_path: String,
    repo_key: Option<String>,
    repo_name: Option<String>,
}

impl WorkspaceWorktree {
    fn checkout(self) -> Checkout {
        Checkout {
            path: self.checkout_path.into(),
            label: None,
            branch: None,
            repo_key: self.repo_key,
            repo_name: self.repo_name,
        }
    }
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
    state_labels: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Deserialize)]
pub(super) struct AgentSessionEntry {
    pub(super) value: String,
}

/// Herdr's word for an agent.
pub(super) fn agent_kind(agent: &str) -> AgentKind {
    match agent {
        "claude" => AgentKind::Known(AgentRuntime::ClaudeCode),
        "codex" => AgentKind::Known(AgentRuntime::Codex),
        other => AgentKind::Other(other.to_owned()),
    }
}

impl PaneEntry {
    fn facts(self) -> PaneFacts {
        let agent = match (self.agent, self.agent_session) {
            (None, None) => None,
            (agent, session) => Some(AgentIdentity {
                kind: agent.as_deref().map_or(AgentKind::Unreported, agent_kind),
                label: self.display_agent.or(agent),
                session: session.map_or(SessionClaim::None, |session| {
                    SessionClaim::Reported(AgentSessionId::new(session.value))
                }),
            }),
        };
        PaneFacts {
            id: PaneId::new(self.pane_id),
            terminal: TerminalId::new(self.terminal_id),
            tab: TabId::new(self.tab_id),
            space: SpaceId::new(self.workspace_id),
            cwd: self.cwd,
            title: self.label.or(self.title).or(self.terminal_title_stripped),
            agent,
            state: self.agent_status.as_deref().map(AgentState::parse),
            idle_label: self
                .state_labels
                .and_then(|mut labels| labels.remove("idle")),
        }
    }
}

fn number(number: Option<u64>) -> Option<u32> {
    number.and_then(|number| u32::try_from(number).ok())
}

fn bare_space(id: SpaceId) -> SpaceView {
    SpaceView {
        id,
        label: None,
        number: None,
        checkout: None,
        worktree: None,
        panes: Vec::new(),
    }
}

/// `workspace list`: labels, numbers, and stamped checkouts.
pub(super) fn all_spaces(transport: &impl Transport) -> Result<Vec<SpaceView>, MuxError> {
    let listed: WorkspaceListEnvelope = parse(&run(transport, &["workspace", "list"])?)?;
    Ok(listed
        .result
        .workspaces
        .into_iter()
        .map(|workspace| SpaceView {
            label: workspace.label,
            number: number(workspace.number),
            checkout: workspace.worktree.map(WorkspaceWorktree::checkout),
            ..bare_space(SpaceId::new(workspace.workspace_id))
        })
        .collect())
}

/// `workspace get <id>`: one space's label and stamped checkout.
pub(super) fn space_summary(
    transport: &impl Transport,
    space: &SpaceId,
) -> Result<SpaceView, MuxError> {
    let got: WorkspaceGetEnvelope = parse(&run(transport, &["workspace", "get", space.as_str()])?)?;
    let workspace = got.result.workspace;
    Ok(SpaceView {
        label: workspace.label,
        number: number(workspace.number),
        checkout: workspace.worktree.map(WorkspaceWorktree::checkout),
        ..bare_space(space.clone())
    })
}

/// `pane list`: every pane, grouped by space in Herdr's order.
pub(super) fn all_panes(transport: &impl Transport) -> Result<Vec<SpaceView>, MuxError> {
    let listed: PaneListEnvelope = parse(&run(transport, &["pane", "list"])?)?;
    let mut spaces: Vec<SpaceView> = Vec::new();
    for facts in listed.result.panes.into_iter().map(PaneEntry::facts) {
        match spaces.iter_mut().find(|space| space.id == facts.space) {
            Some(space) => space.panes.push(facts),
            None => spaces.push(SpaceView {
                panes: vec![facts.clone()],
                ..bare_space(facts.space)
            }),
        }
    }
    Ok(spaces)
}

/// `pane list --workspace <id>`.
pub(super) fn space_panes(
    transport: &impl Transport,
    space: &SpaceId,
) -> Result<Vec<PaneFacts>, MuxError> {
    let listed: PaneListEnvelope = parse(&run(
        transport,
        &["pane", "list", "--workspace", space.as_str()],
    )?)?;
    Ok(listed
        .result
        .panes
        .into_iter()
        .map(PaneEntry::facts)
        .collect())
}

fn worktree_list_for_workspace(
    transport: &impl Transport,
    workspace_id: &str,
) -> Result<Option<WorktreeListResult>, MuxError> {
    let output = match transport.run(&["worktree", "list", "--workspace", workspace_id, "--json"]) {
        Ok(output) => output,
        Err(error) if error_code(&error).as_deref() == Some("not_git_worktree") => {
            return Ok(None);
        }
        Err(error) => return Err(mux_error(error)),
    };
    let worktrees: WorktreeListEnvelope = parse(&output)?;
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

fn checkout(worktree: WorktreeEntry, repo_key: Option<String>) -> (Checkout, WorktreeId) {
    let id = WorktreeId::new(worktree.path.clone());
    let checkout = Checkout {
        path: worktree.path.into(),
        label: worktree.label,
        branch: worktree.branch,
        repo_key,
        repo_name: None,
    };
    (checkout, id)
}

/// `worktree list --workspace <id> --json`: a space's own checkout, or the
/// synthetic `herdr-space:` key when the space is not a Git checkout.
pub(super) fn space_checkout(
    transport: &impl Transport,
    space: &SpaceId,
) -> Result<SpaceView, MuxError> {
    let workspace_id = space.as_str();
    let Some(mut result) = worktree_list_for_workspace(transport, workspace_id)? else {
        return Ok(SpaceView {
            worktree: Some(WorktreeId::new(format!("herdr-space:{workspace_id}"))),
            ..bare_space(space.clone())
        });
    };
    let index = owned_worktree_index(&result, workspace_id).ok_or_else(|| {
        MuxError::Protocol(format!(
            "Herdr returned a Worktree snapshot that does not contain space {workspace_id}"
        ))
    })?;
    let repo_key = result.source.and_then(|source| source.repo_key);
    let (checkout, worktree) = checkout(result.worktrees.swap_remove(index), repo_key);
    Ok(SpaceView {
        checkout: Some(checkout),
        worktree: Some(worktree),
        ..bare_space(space.clone())
    })
}

/// `worktree list --cwd <path> --json`, then `workspace list` and the
/// listings it needs to prove no other space holds the same checkout.
pub(super) fn checkout_owner(
    transport: &impl Transport,
    cwd: &Path,
) -> Result<SpaceView, MuxError> {
    let cwd = cwd
        .to_str()
        .ok_or_else(|| MuxError::Unavailable("Worktree path must be valid UTF-8".to_owned()))?;
    let listed: WorktreeListEnvelope = parse(&run(
        transport,
        &["worktree", "list", "--cwd", cwd, "--json"],
    )?)?;
    let WorktreeListResult { source, worktrees } = listed.result;
    let worktree = worktrees
        .into_iter()
        .find(|worktree| Path::new(&worktree.path) == Path::new(cwd))
        .ok_or_else(|| {
            MuxError::Unavailable(format!("Herdr did not resolve {cwd} as a Worktree"))
        })?;
    let workspace_id = worktree.open_workspace_id.clone().ok_or_else(|| {
        MuxError::Unavailable(format!("Worktree {} is not open in Herdr", worktree.path))
    })?;
    let repo_key = source.and_then(|source| source.repo_key);
    let (owners, unknown) =
        spaces_owning_checkout(transport, &worktree, repo_key.as_deref(), &workspace_id)?;
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
        return Err(MuxError::Unavailable(format!(
            "Checkout {} is open in {open_in}{suspects}, so Namedrop can't tell which one \
             this command is for. Pass --herdr-workspace <space-id> (or --herdr-pane \
             <pane-id>).",
            worktree.path
        )));
    }
    let (checkout, worktree) = checkout(worktree, repo_key);
    Ok(SpaceView {
        checkout: Some(checkout),
        worktree: Some(worktree),
        ..bare_space(SpaceId::new(workspace_id))
    })
}

// `worktree list --cwd` names only the first opener. Returns the spaces proven
// to own `checkout`, and, for a linked checkout, the spaces in the same
// repository whose checkout Herdr does not report.
fn spaces_owning_checkout(
    transport: &impl Transport,
    checkout: &WorktreeEntry,
    repo_key: Option<&str>,
    opener: &str,
) -> Result<(Vec<String>, Vec<String>), MuxError> {
    let workspaces: WorkspaceListEnvelope = parse(&run(transport, &["workspace", "list"])?)?;
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
        let listing = match worktree_list_for_workspace(transport, &workspace.workspace_id) {
            Ok(listing) => listing,
            Err(MuxError::Gone(Gone::Space)) => None,
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
        match owned_worktree_index(&result, &workspace.workspace_id) {
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

pub(super) fn screen_args(pane: &PaneId, request: ScreenRequest) -> Vec<String> {
    let source = match request.wrap {
        Wrap::AsDrawn => "recent",
        Wrap::Joined => "recent-unwrapped",
    };
    let mut args = vec![
        "pane".to_owned(),
        "read".to_owned(),
        pane.as_str().to_owned(),
        "--source".to_owned(),
        source.to_owned(),
        "--lines".to_owned(),
        request.lines.to_string(),
    ];
    if request.style == Style::Ansi {
        args.extend(["--format".to_owned(), "ansi".to_owned()]);
    }
    args
}

#[derive(Deserialize)]
struct ProcessInfo {
    shell_pid: Option<u32>,
    foreground_process_group_id: Option<u32>,
    #[serde(default)]
    foreground_processes: Vec<ProcessEntry>,
    tty: Option<String>,
}

#[derive(Deserialize)]
struct ProcessEntry {
    pid: u32,
}

pub(super) fn process(transport: &impl Transport, pane: &PaneId) -> Result<PaneProcess, MuxError> {
    let reply: Value = parse(&run(
        transport,
        &["pane", "process-info", "--pane", pane.as_str()],
    )?)?;
    let info: ProcessInfo = serde_json::from_value(reply["result"]["process_info"].clone())
        .map_err(|error| MuxError::Protocol(error.to_string()))?;
    Ok(PaneProcess {
        shell_pid: info.shell_pid,
        foreground_group: info.foreground_process_group_id,
        foreground_pids: info
            .foreground_processes
            .into_iter()
            .map(|process| process.pid)
            .collect(),
        tty: info.tty.map(Into::into),
    })
}

pub(super) fn tab_label(
    transport: &impl Transport,
    tab: &TabId,
) -> Result<Option<String>, MuxError> {
    let value: Value = parse(&run(transport, &["tab", "get", tab.as_str()])?)?;
    Ok(value
        .pointer("/result/label")
        .or_else(|| value.pointer("/result/tab/label"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

pub(super) fn integration_root(
    transport: &impl Transport,
) -> Result<Option<std::path::PathBuf>, MuxError> {
    let list: Value = parse(&run(
        transport,
        &["plugin", "list", "--plugin", "namedrop", "--json"],
    )?)?;
    Ok(list["result"]["plugins"]
        .as_array()
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| plugin["plugin_id"] == "namedrop")
        })
        .and_then(|plugin| plugin["plugin_root"].as_str())
        .map(std::path::PathBuf::from)
        .filter(|root| root.is_absolute()))
}

/// Each command an effect runs, in order. A badge list that mixes the
/// counters and a relay runs one command per metadata source.
pub(super) fn effect_commands(effect: &Effect, socket: &str) -> Vec<Vec<String>> {
    let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    match effect {
        Effect::Identify { pane, alias, texts } => vec![strings(&[
            "pane",
            "report-metadata",
            pane.as_str(),
            "--source",
            "namedrop-identity",
            "--title",
            &format!("@{alias}"),
            "--state-label",
            &format!("idle={}", texts.ready),
            "--state-label",
            &format!("done={}", texts.ready),
            "--state-label",
            &format!("working={}", texts.working),
            "--state-label",
            &format!("blocked={}", texts.attention),
            "--state-label",
            &format!("unknown={}", texts.unknown),
        ])],
        Effect::Badges { pane, badges } => {
            let mut counters = strings(&[
                "pane",
                "report-metadata",
                pane.as_str(),
                "--source",
                "namedrop",
            ]);
            let mut commands = Vec::new();
            for badge in badges {
                match badge {
                    Badge::Awaiting(count) => counters
                        .extend(["--token".to_owned(), format!("namedrop-awaiting={count}")]),
                    Badge::Attention(count) => counters
                        .extend(["--token".to_owned(), format!("namedrop-attention={count}")]),
                    Badge::Relay { alias, summary } => commands.push(strings(&[
                        "pane",
                        "report-metadata",
                        pane.as_str(),
                        "--source",
                        "namedrop-relay",
                        "--token",
                        &format!("namedrop-relay=@{alias}: {summary}"),
                        "--ttl-ms",
                        "15000",
                    ])),
                }
            }
            if counters.len() > 5 {
                commands.insert(0, counters);
            }
            commands
        }
        Effect::RenameTab { tab, label } => vec![strings(&["tab", "rename", tab.as_str(), label])],
        Effect::Notify { title, body, sound } => {
            let mut command = strings(&[
                "notification",
                "show",
                title,
                "--body",
                body,
                "--position",
                "bottom-right",
            ]);
            if *sound {
                command.extend(["--sound".to_owned(), "done".to_owned()]);
            }
            vec![command]
        }
        Effect::OpenPopup(popup) => vec![popup_args(popup, socket)],
        Effect::ReloadKeybindings => vec![strings(&["server", "reload-config"])],
    }
}

fn popup_args(popup: &PopupSpec, socket: &str) -> Vec<String> {
    let screen = popup.screen;
    let (entrypoint, width, height) = match screen {
        PopupScreen::Name => ("naming", "72", "20"),
        PopupScreen::Manage => ("management", "72", "24"),
        PopupScreen::Doctor => ("doctor", "72", "24"),
        PopupScreen::Status => ("agent-status", "84", "24"),
        PopupScreen::Tour => (
            crate::tour::ENTRYPOINT,
            crate::tour::POPUP_WIDTH,
            crate::tour::POPUP_HEIGHT,
        ),
    };
    let target = &popup.target;
    let session = match target.agent.as_ref().map(|agent| &agent.session) {
        Some(SessionClaim::Reported(session)) => session.as_str(),
        _ => "",
    };
    [
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
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([
        "--env".to_owned(),
        format!("NAMEDROP_TARGET_WORKSPACE={}", target.space.as_str()),
        "--env".to_owned(),
        format!("NAMEDROP_TARGET_PANE={}", target.id.as_str()),
        "--env".to_owned(),
        format!("NAMEDROP_TARGET_TERMINAL={}", target.terminal.as_str()),
        "--env".to_owned(),
        format!("NAMEDROP_TARGET_SESSION={session}"),
        "--env".to_owned(),
        format!("HERDR_PLUGIN_STATE_DIR={}", popup.state_dir.display()),
        "--env".to_owned(),
        format!("HERDR_SOCKET_PATH={socket}"),
        "--focus".to_owned(),
    ])
    .collect()
}

pub(super) fn key(key: Key) -> &'static str {
    match key {
        Key::Enter => "enter",
        Key::Escape => "esc",
        Key::CtrlC => "ctrl+c",
        Key::CtrlU => "ctrl+u",
        Key::Up => "up",
        Key::Down => "down",
    }
}

pub(super) fn keys_args(pane: &PaneId, recipient: Recipient, keys: &[Key]) -> Vec<String> {
    let owner = match recipient {
        Recipient::Agent => "agent",
        Recipient::Pane => "pane",
    };
    [owner, "send-keys", pane.as_str()]
        .into_iter()
        .chain(keys.iter().map(|pressed| self::key(*pressed)))
        .map(str::to_owned)
        .collect()
}
