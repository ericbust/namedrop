//! New channels and new agents from the page. A channel is a Herdr workspace
//! with Namedrop active in it; an agent is a named pane in a new tab, started
//! with the plain `claude` or `codex` command. Herdr is only ever reached on
//! the socket of a space Namedrop already knows.
use super::super::hub::{HubCommand, now_ms};
use super::super::launches::{Launch, run_lifecycle};
use super::super::snapshot::SpaceKey;
use super::{Front, Request, read_json_body, respond, respond_status};
use crate::store::AliasClaim;
use crate::store::dashboard::SpaceRow;
use crate::{HerdrCommandExecutor, ProcessHerdr, StateNamespace, StateStore, WorkspaceLabel};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Read},
    net::TcpStream,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_CHANNEL_NAME_CHARS: usize = 64;
const MAX_RECENT_FOLDERS: usize = 12;
const UNREACHABLE: &str = "Herdr isn't reachable. Check that Herdr is running, then try again.";
const UNKNOWN_CHANNEL: &str =
    "Namedrop doesn't know this channel. Reload the page, then try again.";
const RESTARTING: &str = "Channels is restarting. Try again in a moment.";
const STATE_UNAVAILABLE: &str = "Namedrop couldn't read its state. Try again.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChannelBody {
    name: String,
    folder: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentBody {
    kind: String,
    name: String,
    #[serde(default)]
    persona: Option<String>,
}

struct NewChannel {
    /// As the person typed it, trimmed; Herdr shows it as the workspace label.
    name: String,
    /// How Namedrop routes to it, as `#label`.
    label: WorkspaceLabel,
    folder: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentKind {
    Claude,
    Codex,
    Terminal,
}

impl AgentKind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }

    fn pane_type(self) -> crate::PaneType {
        match self {
            Self::Claude | Self::Codex => crate::PaneType::Agent,
            Self::Terminal => crate::PaneType::Terminal,
        }
    }

    /// The whole command typed into the new pane: fixed text, never input.
    fn command(self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Terminal => None,
        }
    }
}

struct NewAgent {
    kind: AgentKind,
    alias: String,
    persona: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
enum FoldersRefusal {
    UnknownChannel,
    StateUnavailable,
    HerdrUnreachable,
}

impl FoldersRefusal {
    fn reply(&self) -> (&'static str, String) {
        match self {
            Self::UnknownChannel => ("404 Not Found", UNKNOWN_CHANNEL.into()),
            Self::StateUnavailable => ("503 Service Unavailable", STATE_UNAVAILABLE.into()),
            Self::HerdrUnreachable => ("503 Service Unavailable", UNREACHABLE.into()),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ChannelRefusal {
    NoName,
    NameTooLong,
    NameHasHiddenCharacters,
    NameNotRoutable,
    FolderNotFullPath,
    FolderMissing,
    FolderIsFile,
    NameTaken(WorkspaceLabel),
    UnknownChannel,
    StateUnavailable,
    Restarting,
    HerdrUnreachable,
    CreateFailed,
    ActivateFailed,
}

impl ChannelRefusal {
    fn reply(&self) -> (&'static str, String) {
        const BAD: &str = "400 Bad Request";
        match self {
            Self::NoName => (BAD, "Give the channel a name.".into()),
            Self::NameTooLong => (BAD, "A channel name can be up to 64 characters.".into()),
            Self::NameHasHiddenCharacters => (
                BAD,
                "A channel name can't contain control or invisible characters.".into(),
            ),
            Self::NameNotRoutable => (
                BAD,
                "A channel name needs a letter or a digit, and it can't be \"all\".".into(),
            ),
            Self::FolderNotFullPath => (
                BAD,
                "Type the folder's full path, starting with / or ~.".into(),
            ),
            Self::FolderMissing => (
                BAD,
                "That folder doesn't exist. Namedrop never creates folders.".into(),
            ),
            Self::FolderIsFile => (BAD, "That path is a file, not a folder.".into()),
            Self::NameTaken(label) => (
                "409 Conflict",
                format!("Another channel is already named #{}. Pick a different name.", label.as_str()),
            ),
            Self::UnknownChannel => ("404 Not Found", UNKNOWN_CHANNEL.into()),
            Self::StateUnavailable => ("503 Service Unavailable", STATE_UNAVAILABLE.into()),
            Self::Restarting => ("503 Service Unavailable", RESTARTING.into()),
            Self::HerdrUnreachable => ("503 Service Unavailable", UNREACHABLE.into()),
            Self::CreateFailed => (
                "500 Internal Server Error",
                "Herdr couldn't create the workspace. Try again.".into(),
            ),
            Self::ActivateFailed => (
                "500 Internal Server Error",
                "Namedrop couldn't activate in the new workspace, so it was closed again. Try again."
                    .into(),
            ),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum AgentRefusal {
    BadKind,
    BadName,
    PersonaTooLong,
    PersonaHasHiddenCharacters,
    NameTaken(String, AliasClaim),
    Paused,
    Inactive,
    FolderGone(String),
    FolderUnknown,
    UnknownChannel,
    WorkspaceClosed,
    StateUnavailable,
    Restarting,
    HerdrUnreachable,
    TabFailed,
    NameFailed,
}

impl AgentRefusal {
    fn reply(&self) -> (&'static str, String) {
        const BAD: &str = "400 Bad Request";
        const CONFLICT: &str = "409 Conflict";
        match self {
            Self::BadKind => (BAD, "Pick Claude Code, Codex or a terminal.".into()),
            Self::BadName => (
                BAD,
                "A name uses lowercase letters, digits and single hyphens, like code-reviewer."
                    .into(),
            ),
            Self::PersonaTooLong => (BAD, "A persona can be up to 1,000 characters.".into()),
            Self::PersonaHasHiddenCharacters => (
                BAD,
                "A persona can't contain control or invisible characters.".into(),
            ),
            Self::NameTaken(alias, AliasClaim::InUse) => (
                CONFLICT,
                format!("@{alias} is already in use in this channel. Pick a different name."),
            ),
            Self::NameTaken(alias, AliasClaim::Retired) => (
                CONFLICT,
                format!(
                    "@{alias} was retired in this channel and can't be used again. Pick a different name."
                ),
            ),
            Self::NameTaken(alias, AliasClaim::Renamed(Some(replacement))) => (
                CONFLICT,
                format!(
                    "@{alias} was renamed to @{replacement} in this channel and can't be used again. Pick a different name."
                ),
            ),
            Self::NameTaken(alias, AliasClaim::Renamed(None)) => (
                CONFLICT,
                format!(
                    "@{alias} was renamed in this channel and can't be used again. Pick a different name."
                ),
            ),
            Self::Paused => (
                CONFLICT,
                "Namedrop is paused in this channel. Resume it, then add the agent.".into(),
            ),
            Self::Inactive => (CONFLICT, "Namedrop isn't active in this channel.".into()),
            Self::FolderGone(folder) => (
                CONFLICT,
                format!("This channel's folder no longer exists: {folder}."),
            ),
            Self::FolderUnknown => (
                CONFLICT,
                "Namedrop couldn't find this channel's folder.".into(),
            ),
            Self::UnknownChannel => ("404 Not Found", UNKNOWN_CHANNEL.into()),
            Self::WorkspaceClosed => (
                "404 Not Found",
                "This channel's workspace is closed.".into(),
            ),
            Self::StateUnavailable => ("503 Service Unavailable", STATE_UNAVAILABLE.into()),
            Self::Restarting => ("503 Service Unavailable", RESTARTING.into()),
            Self::HerdrUnreachable => ("503 Service Unavailable", UNREACHABLE.into()),
            Self::TabFailed => (
                "500 Internal Server Error",
                "Herdr couldn't open a tab for the agent. Try again.".into(),
            ),
            Self::NameFailed => (
                "500 Internal Server Error",
                "Namedrop couldn't name the new pane, so its tab was closed again. Try again."
                    .into(),
            ),
        }
    }
}

impl NewChannel {
    fn parse(body: ChannelBody, home: Option<&Path>) -> Result<Self, ChannelRefusal> {
        let name = body.name.trim();
        if name.is_empty() {
            return Err(ChannelRefusal::NoName);
        }
        if name.chars().count() > MAX_CHANNEL_NAME_CHARS {
            return Err(ChannelRefusal::NameTooLong);
        }
        if name
            .chars()
            .any(|c| c.is_control() || crate::domain::is_invisible_format(c))
        {
            return Err(ChannelRefusal::NameHasHiddenCharacters);
        }
        let label = WorkspaceLabel::from_herdr(name);
        if !label
            .as_str()
            .bytes()
            .any(|byte| byte.is_ascii_alphanumeric())
            || label.as_str() == "all"
        {
            return Err(ChannelRefusal::NameNotRoutable);
        }
        Ok(Self {
            name: name.to_owned(),
            label,
            folder: parse_folder(&body.folder, home)?,
        })
    }
}

/// The canonical path of an existing folder. `~` is the server's home.
fn parse_folder(typed: &str, home: Option<&Path>) -> Result<String, ChannelRefusal> {
    let typed = typed.trim();
    let path = match (typed.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_owned(),
        (Some(rest), Some(home)) if rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(typed),
    };
    if !path.is_absolute() {
        return Err(ChannelRefusal::FolderNotFullPath);
    }
    match std::fs::metadata(&path) {
        Err(_) => return Err(ChannelRefusal::FolderMissing),
        Ok(metadata) if !metadata.is_dir() => return Err(ChannelRefusal::FolderIsFile),
        Ok(_) => {}
    }
    std::fs::canonicalize(&path)
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .ok_or(ChannelRefusal::FolderMissing)
}

impl NewAgent {
    fn parse(body: AgentBody) -> Result<Self, AgentRefusal> {
        let kind = AgentKind::parse(&body.kind).ok_or(AgentRefusal::BadKind)?;
        let name = body.name.trim();
        let alias = name.strip_prefix('@').unwrap_or(name);
        crate::validate_alias(alias).map_err(|_| AgentRefusal::BadName)?;
        let persona = body
            .persona
            .as_deref()
            .and_then(crate::lifecycle::normalize_persona);
        if let Some(persona) = &persona
            && crate::validate_persona(persona).is_err()
        {
            return Err(
                if persona.chars().count() > crate::domain::MAX_PERSONA_CHARS {
                    AgentRefusal::PersonaTooLong
                } else {
                    AgentRefusal::PersonaHasHiddenCharacters
                },
            );
        }
        Ok(Self {
            kind,
            alias: alias.to_owned(),
            persona,
        })
    }
}

#[derive(Deserialize)]
struct Listed<T> {
    result: T,
}

#[derive(Deserialize)]
struct Workspaces {
    workspaces: Vec<Workspace>,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    label: Option<String>,
    worktree: Option<Worktree>,
}

#[derive(Deserialize)]
struct Worktree {
    checkout_path: Option<String>,
}

#[derive(Deserialize)]
struct Panes {
    panes: Vec<Pane>,
}

#[derive(Deserialize)]
struct Pane {
    workspace_id: String,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct CreatedWorkspace {
    workspace: WorkspaceId,
}

#[derive(Deserialize)]
struct WorkspaceId {
    workspace_id: String,
}

#[derive(Deserialize)]
struct CreatedTab {
    tab: TabId,
    root_pane: PaneId,
}

#[derive(Deserialize)]
struct TabId {
    tab_id: String,
}

#[derive(Deserialize)]
struct PaneId {
    pane_id: String,
}

/// What a failed request closes again, so it leaves nothing it created.
#[derive(Debug, Eq, PartialEq)]
enum Undo {
    Workspace(String),
    Tab(String),
}

impl Undo {
    fn args(&self) -> [&str; 3] {
        match self {
            Self::Workspace(id) => ["workspace", "close", id],
            Self::Tab(id) => ["tab", "close", id],
        }
    }
}

/// One Herdr session, reached on the socket of a space Namedrop knows.
struct Herdr(ProcessHerdr);

impl Herdr {
    fn of(namespace: &StateNamespace) -> Self {
        Self(ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone())))
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, ()> {
        let output = self.0.execute(&["workspace", "list"]).map_err(|_| ())?;
        serde_json::from_str::<Listed<Workspaces>>(&output.stdout)
            .map(|listed| listed.result.workspaces)
            .map_err(|_| ())
    }

    fn first_pane_cwd(&self, workspace_id: &str) -> Result<Option<String>, ()> {
        let output = self
            .0
            .execute(&["pane", "list", "--workspace", workspace_id])
            .map_err(|_| ())?;
        let panes = serde_json::from_str::<Listed<Panes>>(&output.stdout).map_err(|_| ())?;
        Ok(panes
            .result
            .panes
            .into_iter()
            .find(|pane| pane.workspace_id == workspace_id)
            .and_then(|pane| pane.cwd))
    }

    /// The workspace's folder: its checkout, else where its first pane is.
    fn folder(&self, workspace: &Workspace) -> Result<Option<String>, ()> {
        match workspace
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.checkout_path.clone())
        {
            Some(checkout) => Ok(Some(checkout)),
            None => self.first_pane_cwd(&workspace.workspace_id),
        }
    }

    fn create_workspace(&self, folder: &str, label: &str) -> Result<String, String> {
        let output = self.0.execute(&[
            "workspace",
            "create",
            "--cwd",
            folder,
            "--label",
            label,
            "--no-focus",
        ])?;
        serde_json::from_str::<Listed<CreatedWorkspace>>(&output.stdout)
            .map(|created| created.result.workspace.workspace_id)
            .map_err(|error| error.to_string())
    }

    fn create_tab(
        &self,
        workspace_id: &str,
        folder: &str,
        label: &str,
    ) -> Result<CreatedTab, String> {
        let output = self.0.execute(&[
            "tab",
            "create",
            "--workspace",
            workspace_id,
            "--cwd",
            folder,
            "--label",
            label,
            "--no-focus",
        ])?;
        serde_json::from_str::<Listed<CreatedTab>>(&output.stdout)
            .map(|created| created.result)
            .map_err(|error| error.to_string())
    }

    fn undo(&self, undo: Undo) {
        if let Err(error) = self.0.execute(&undo.args()) {
            eprintln!(
                "dashboard: could not close what a failed request created ({undo:?}): {error}"
            );
        }
    }
}

/// Recent folders, newest workspace first: Herdr numbers workspaces as it
/// opens them, so a higher id is newer.
fn recent_folders<'a>(
    workspaces: &'a [Workspace],
    mut folder: impl FnMut(&'a Workspace) -> Result<Option<String>, ()>,
) -> Result<Vec<String>, ()> {
    let mut newest: Vec<(usize, &Workspace)> = workspaces.iter().enumerate().collect();
    newest.sort_by_key(|(index, workspace)| {
        let number = workspace
            .workspace_id
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse::<u64>()
            .unwrap_or(0);
        std::cmp::Reverse((number, *index))
    });
    let mut folders = Vec::new();
    for (_, workspace) in newest {
        if folders.len() == MAX_RECENT_FOLDERS {
            break;
        }
        if let Some(path) = folder(workspace)?
            && !folders.contains(&path)
        {
            folders.push(path);
        }
    }
    Ok(folders)
}

fn known_space(state: &Path, key: &SpaceKey) -> Result<Option<SpaceRow>, ()> {
    let Some(mut store) = StateStore::open_read_only(state).map_err(|_| ())? else {
        return Ok(None);
    };
    store
        .set_busy_timeout(Duration::from_millis(250))
        .map_err(|_| ())?;
    Ok(store
        .dashboard_spaces()
        .map_err(|_| ())?
        .into_iter()
        .find(|space| SpaceKey::of(&space.namespace) == *key))
}

fn read_state<T>(
    state: &Path,
    read: impl FnOnce(&mut crate::store::StateTransaction<'_>) -> Result<T, crate::StoreError>,
) -> Result<T, ()> {
    let mut store = StateStore::open_read_only(state)
        .map_err(|_| ())?
        .ok_or(())?;
    store
        .set_busy_timeout(Duration::from_millis(250))
        .map_err(|_| ())?;
    store.transaction(read).map_err(|_| ())
}

fn reply_json(writer: &mut TcpStream, status: &str, body: serde_json::Value) -> io::Result<()> {
    respond(
        writer,
        status,
        "application/json",
        "",
        body.to_string().as_bytes(),
    )
}

fn refuse(writer: &mut TcpStream, (status, sentence): (&'static str, String)) -> io::Result<()> {
    reply_json(writer, status, json!({ "error": sentence }))
}

pub(super) fn folders(writer: &mut TcpStream, front: &Front, key: &SpaceKey) -> io::Result<()> {
    let found = (|| {
        let space = known_space(front.files.state(), key)
            .map_err(|()| FoldersRefusal::StateUnavailable)?
            .ok_or(FoldersRefusal::UnknownChannel)?;
        let herdr = Herdr::of(&space.namespace);
        let workspaces = herdr
            .workspaces()
            .map_err(|()| FoldersRefusal::HerdrUnreachable)?;
        recent_folders(&workspaces, |workspace| herdr.folder(workspace))
            .map_err(|()| FoldersRefusal::HerdrUnreachable)
    })();
    match found {
        Ok(folders) => reply_json(writer, "200 OK", json!({ "folders": folders })),
        Err(refusal) => refuse(writer, refusal.reply()),
    }
}

pub(super) fn create_channel(
    reader: &mut impl Read,
    writer: &mut TcpStream,
    front: &Front,
    request: &Request,
    key: &SpaceKey,
) -> io::Result<()> {
    let body: ChannelBody = match read_json_body(reader, request) {
        Ok(body) => body,
        Err(status) => return respond_status(writer, status),
    };
    let directories = crate::InheritedDirectories::from_environment();
    let created = NewChannel::parse(body, directories.home()).and_then(|channel| {
        let _admitted = front
            .admit_mutation()
            .map_err(|_| ChannelRefusal::Restarting)?;
        create(front, key, &channel)
    });
    match created {
        Ok((namespace, channel)) => reply_json(
            writer,
            "201 Created",
            json!({
                "key": SpaceKey::of(&namespace),
                "workspace_id": namespace.space_id.as_str(),
                "label": channel,
            }),
        ),
        Err(refusal) => refuse(writer, refusal.reply()),
    }
}

fn create(
    front: &Front,
    key: &SpaceKey,
    channel: &NewChannel,
) -> Result<(StateNamespace, String), ChannelRefusal> {
    let space = known_space(front.files.state(), key)
        .map_err(|()| ChannelRefusal::StateUnavailable)?
        .ok_or(ChannelRefusal::UnknownChannel)?;
    let herdr = Herdr::of(&space.namespace);
    let live = herdr
        .workspaces()
        .map_err(|()| ChannelRefusal::HerdrUnreachable)?;
    if live.iter().any(|workspace| {
        workspace.label.as_deref().map(WorkspaceLabel::from_herdr) == Some(channel.label.clone())
    }) {
        return Err(ChannelRefusal::NameTaken(channel.label.clone()));
    }
    let workspace_id = herdr
        .create_workspace(&channel.folder, &channel.name)
        .map_err(|error| {
            eprintln!("dashboard: Herdr could not create a workspace: {error}");
            ChannelRefusal::CreateFailed
        })?;
    let namespace = StateNamespace::for_space(
        space.namespace.session_socket_id.clone(),
        workspace_id.clone(),
    );
    if let Err(error) = run_lifecycle(
        front.files.state(),
        &namespace,
        None,
        "activate",
        json!({"confirmed": true}),
    ) {
        eprintln!("dashboard: could not activate the new workspace {workspace_id}: {error}");
        herdr.undo(Undo::Workspace(workspace_id));
        return Err(ChannelRefusal::ActivateFailed);
    }
    let _ = front.hub.send(HubCommand::DiscoveryReady);
    Ok((namespace, channel.name.clone()))
}

pub(super) fn add_agent(
    reader: &mut impl Read,
    writer: &mut TcpStream,
    front: &Front,
    request: &Request,
    key: &SpaceKey,
) -> io::Result<()> {
    let body: AgentBody = match read_json_body(reader, request) {
        Ok(body) => body,
        Err(status) => return respond_status(writer, status),
    };
    let added = NewAgent::parse(body).and_then(|agent| {
        let _admitted = front
            .admit_mutation()
            .map_err(|_| AgentRefusal::Restarting)?;
        add(front, key, agent)
    });
    match added {
        Ok((address_id, alias)) => reply_json(
            writer,
            "201 Created",
            json!({ "address_id": address_id, "alias": alias }),
        ),
        Err(refusal) => refuse(writer, refusal.reply()),
    }
}

fn add(front: &Front, key: &SpaceKey, agent: NewAgent) -> Result<(String, String), AgentRefusal> {
    let state = front.files.state();
    let space = known_space(state, key)
        .map_err(|()| AgentRefusal::StateUnavailable)?
        .ok_or(AgentRefusal::UnknownChannel)?;
    if !space.state.activated || space.state.deactivated {
        return Err(AgentRefusal::Inactive);
    }
    if space.state.paused {
        return Err(AgentRefusal::Paused);
    }
    let namespace = space.namespace;
    let herdr = Herdr::of(&namespace);
    let workspace = herdr
        .workspaces()
        .map_err(|()| AgentRefusal::HerdrUnreachable)?
        .into_iter()
        .find(|workspace| workspace.workspace_id == namespace.space_id.as_str())
        .ok_or(AgentRefusal::WorkspaceClosed)?;
    let claim = |alias: &str| read_state(state, |tx| tx.alias_claim(&namespace, alias));
    if let Some(claim) = claim(&agent.alias).map_err(|()| AgentRefusal::StateUnavailable)? {
        return Err(AgentRefusal::NameTaken(agent.alias, claim));
    }
    let folder = herdr
        .folder(&workspace)
        .map_err(|()| AgentRefusal::HerdrUnreachable)?
        .ok_or(AgentRefusal::FolderUnknown)?;
    if !Path::new(&folder).is_dir() {
        return Err(AgentRefusal::FolderGone(folder));
    }
    let tab = herdr
        .create_tab(
            &workspace.workspace_id,
            &folder,
            &format!("@{}", agent.alias),
        )
        .map_err(|error| {
            eprintln!("dashboard: Herdr could not open a tab: {error}");
            AgentRefusal::TabFailed
        })?;
    let pane = tab.root_pane.pane_id;
    let named = run_lifecycle(
        state,
        &namespace,
        Some(&pane),
        "name",
        json!({
            "alias": agent.alias,
            "pane_type": agent.kind.pane_type(),
            "persona": agent.persona,
        }),
    )
    .map_err(|error| eprintln!("dashboard: could not name the new pane {pane}: {error}"))
    .and_then(|_| {
        read_state(state, |tx| {
            Ok(tx
                .active_pane_bindings(&namespace)?
                .into_iter()
                .find(|binding| binding.pane_id == pane)
                .map(|binding| binding.address_id.as_str().to_owned()))
        })?
        .ok_or(())
    });
    let Ok(address_id) = named else {
        herdr.undo(Undo::Tab(tab.tab.tab_id));
        return Err(match claim(&agent.alias) {
            Ok(Some(claim)) => AgentRefusal::NameTaken(agent.alias, claim),
            _ => AgentRefusal::NameFailed,
        });
    };
    if let Some(command) = agent.kind.command() {
        // A pane that didn't start still shows as needing the person, who can
        // start it by hand in the pane view, so this is not a refusal.
        if let Err(error) = herdr.0.execute(&["pane", "run", &pane, command]) {
            eprintln!("dashboard: could not start {command} in {pane}: {error}");
        }
        front.launches.record(Launch::new(
            namespace,
            address_id.clone(),
            agent.alias.clone(),
            pane,
            now_ms(),
        ));
    }
    Ok((address_id, agent.alias))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str, checkout: Option<&str>) -> Workspace {
        Workspace {
            workspace_id: id.into(),
            label: None,
            worktree: checkout.map(|path| Worktree {
                checkout_path: Some(path.into()),
            }),
        }
    }

    #[test]
    fn recent_folders_are_newest_first_distinct_capped_and_prefer_the_checkout() {
        let mut workspaces = vec![
            workspace("w2", Some("/repo/two")),
            workspace("w10", None),
            workspace("w3", Some("/repo/two")),
            workspace("w1", Some("/repo/one")),
        ];
        let cwd = |workspace: &Workspace| -> Result<Option<String>, ()> {
            Ok(match workspace.worktree.as_ref() {
                Some(worktree) => worktree.checkout_path.clone(),
                None => Some(format!("/cwd/{}", workspace.workspace_id)),
            })
        };
        assert_eq!(
            recent_folders(&workspaces, cwd).unwrap(),
            vec!["/cwd/w10", "/repo/two", "/repo/one"]
        );
        workspaces = (1..=20)
            .map(|n| workspace(&format!("w{n}"), Some(&format!("/repo/{n}"))))
            .collect();
        let folders = recent_folders(&workspaces, cwd).unwrap();
        assert_eq!(folders.len(), 12);
        assert_eq!(folders.first().unwrap(), "/repo/20");
        assert_eq!(folders.last().unwrap(), "/repo/9");
        assert!(recent_folders(&workspaces, |_| Err(())).is_err());
    }

    #[test]
    fn channel_names_fold_to_a_routable_label_and_folders_must_exist() {
        let home = std::env::temp_dir().join(crate::IdGenerator::next_id(
            &crate::ProcessIds::default(),
            "channel-create-home",
        ));
        std::fs::create_dir_all(home.join("project")).unwrap();
        std::fs::write(home.join("notes.txt"), "").unwrap();
        let parse = |name: &str, folder: &str| {
            NewChannel::parse(
                ChannelBody {
                    name: name.into(),
                    folder: folder.into(),
                },
                Some(&home),
            )
        };
        let canonical = std::fs::canonicalize(home.join("project"))
            .unwrap()
            .display()
            .to_string();
        let channel = parse("  Proto Chan ", "~/project").unwrap();
        assert_eq!(
            (
                channel.name.as_str(),
                channel.label.as_str(),
                channel.folder.as_str()
            ),
            ("Proto Chan", "proto-chan", canonical.as_str())
        );
        assert_eq!(
            parse("x", "~").unwrap().folder,
            std::fs::canonicalize(&home).unwrap().display().to_string()
        );
        assert_eq!(
            parse("x", &format!("{}/project/../project", home.display()))
                .unwrap()
                .folder,
            canonical
        );
        let long = "n".repeat(65);
        for (name, folder, refusal) in [
            ("  ", "/", ChannelRefusal::NoName),
            (long.as_str(), "/", ChannelRefusal::NameTooLong),
            ("a\u{202E}b", "/", ChannelRefusal::NameHasHiddenCharacters),
            ("a\tb", "/", ChannelRefusal::NameHasHiddenCharacters),
            ("All", "/", ChannelRefusal::NameNotRoutable),
            ("!!!", "/", ChannelRefusal::NameNotRoutable),
            ("-_-", "/", ChannelRefusal::NameNotRoutable),
            ("x", "project", ChannelRefusal::FolderNotFullPath),
            ("x", "~other/project", ChannelRefusal::FolderNotFullPath),
            ("x", "", ChannelRefusal::FolderNotFullPath),
            ("x", "~/missing", ChannelRefusal::FolderMissing),
            ("x", "~/notes.txt", ChannelRefusal::FolderIsFile),
        ] {
            assert_eq!(
                parse(name, folder).err(),
                Some(refusal),
                "{name:?} {folder:?}"
            );
        }
        assert!(parse(&"n".repeat(64), "/").is_ok());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn agent_kind_alone_picks_the_pane_type_and_the_fixed_command() {
        let parse = |kind: &str, name: &str, persona: Option<&str>| {
            NewAgent::parse(AgentBody {
                kind: kind.into(),
                name: name.into(),
                persona: persona.map(str::to_owned),
            })
        };
        let agent = parse("claude", " @code-reviewer ", Some("  Reviews diffs. ")).unwrap();
        assert_eq!(
            (agent.kind, agent.alias.as_str(), agent.persona.as_deref()),
            (AgentKind::Claude, "code-reviewer", Some("Reviews diffs."))
        );
        assert_eq!(parse("codex", "x", Some("   ")).unwrap().persona, None);
        assert_eq!(
            [AgentKind::Claude, AgentKind::Codex, AgentKind::Terminal]
                .map(|kind| (kind.pane_type(), kind.command())),
            [
                (crate::PaneType::Agent, Some("claude")),
                (crate::PaneType::Agent, Some("codex")),
                (crate::PaneType::Terminal, None),
            ]
        );
        let long = "p".repeat(1_001);
        for (kind, name, persona, refusal) in [
            ("bash", "x", None, AgentRefusal::BadKind),
            ("Claude", "x", None, AgentRefusal::BadKind),
            ("claude", "Code", None, AgentRefusal::BadName),
            ("claude", "a--b", None, AgentRefusal::BadName),
            ("claude", "@@x", None, AgentRefusal::BadName),
            ("claude", "", None, AgentRefusal::BadName),
            (
                "claude",
                "x",
                Some(long.as_str()),
                AgentRefusal::PersonaTooLong,
            ),
            (
                "claude",
                "x",
                Some("a\u{200B}b"),
                AgentRefusal::PersonaHasHiddenCharacters,
            ),
        ] {
            assert_eq!(parse(kind, name, persona).err(), Some(refusal), "{name:?}");
        }
        assert!(parse("terminal", "x", Some(&"p".repeat(1_000))).is_ok());
    }

    #[test]
    fn undo_closes_exactly_what_was_created() {
        assert_eq!(
            Undo::Workspace("w7".into()).args(),
            ["workspace", "close", "w7"]
        );
        assert_eq!(Undo::Tab("w7:t2".into()).args(), ["tab", "close", "w7:t2"]);
    }

    #[test]
    fn every_refusal_reads_as_a_sentence_without_operator_wording() {
        let channel = [
            ChannelRefusal::NoName,
            ChannelRefusal::NameTooLong,
            ChannelRefusal::NameHasHiddenCharacters,
            ChannelRefusal::NameNotRoutable,
            ChannelRefusal::FolderNotFullPath,
            ChannelRefusal::FolderMissing,
            ChannelRefusal::FolderIsFile,
            ChannelRefusal::NameTaken(WorkspaceLabel::from_herdr("Proto")),
            ChannelRefusal::UnknownChannel,
            ChannelRefusal::StateUnavailable,
            ChannelRefusal::Restarting,
            ChannelRefusal::HerdrUnreachable,
            ChannelRefusal::CreateFailed,
            ChannelRefusal::ActivateFailed,
        ]
        .map(|refusal| refusal.reply());
        let agent = [
            AgentRefusal::BadKind,
            AgentRefusal::BadName,
            AgentRefusal::PersonaTooLong,
            AgentRefusal::PersonaHasHiddenCharacters,
            AgentRefusal::NameTaken("x".into(), AliasClaim::InUse),
            AgentRefusal::NameTaken("x".into(), AliasClaim::Retired),
            AgentRefusal::NameTaken("x".into(), AliasClaim::Renamed(Some("y".into()))),
            AgentRefusal::NameTaken("x".into(), AliasClaim::Renamed(None)),
            AgentRefusal::Paused,
            AgentRefusal::Inactive,
            AgentRefusal::FolderGone("/gone".into()),
            AgentRefusal::FolderUnknown,
            AgentRefusal::UnknownChannel,
            AgentRefusal::WorkspaceClosed,
            AgentRefusal::StateUnavailable,
            AgentRefusal::Restarting,
            AgentRefusal::HerdrUnreachable,
            AgentRefusal::TabFailed,
            AgentRefusal::NameFailed,
        ]
        .map(|refusal| refusal.reply());
        let folders = [
            FoldersRefusal::UnknownChannel,
            FoldersRefusal::StateUnavailable,
            FoldersRefusal::HerdrUnreachable,
        ]
        .map(|refusal| refusal.reply());
        for (status, sentence) in channel.iter().chain(&agent).chain(&folders) {
            assert!(status.starts_with(['4', '5']), "{status}");
            assert!(sentence.ends_with('.'), "{sentence}");
            assert!(!sentence.to_lowercase().contains("operator"), "{sentence}");
            assert!(!sentence.contains('\u{2014}'), "{sentence}");
        }
    }
}
