//! New channels and new agents from the page. A channel is a Herdr workspace
//! with Namedrop active in it; an agent is a named pane in a new tab, started
//! with the plain `claude` or `codex` command. Herdr is only ever reached on
//! the socket of a space Namedrop already knows.
use super::super::channel_folders::ChannelFolders;
use super::super::hub::{HubCommand, now_ms};
use super::super::launches::{Launch, run_lifecycle};
use super::super::snapshot::{SpaceKey, StartedAgent};
use super::{Front, Request, read_json_body, respond, respond_status};
use crate::store::AliasClaim;
use crate::store::dashboard::SpaceRow;
use crate::{
    ActorKind, HerdrCommandExecutor, MutationAttribution, ProcessHerdr, StateNamespace, StateStore,
    WorkspaceLabel,
};
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

    /// The agent program this kind starts; a terminal starts none.
    fn started(self) -> Option<StartedAgent> {
        match self {
            Self::Claude => Some(StartedAgent::Claude),
            Self::Codex => Some(StartedAgent::Codex),
            Self::Terminal => None,
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
    FolderNotUsed,
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
            Self::FolderNotUsed => (
                "500 Internal Server Error",
                "Herdr didn't open the workspace in that folder, so it was closed again. Check the folder, then try again."
                    .into(),
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
    FolderNotUsed,
    NameFailed,
    NameUnconfirmed(String),
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
            Self::FolderNotUsed => (
                "500 Internal Server Error",
                "Herdr didn't open the tab in this channel's folder, so it was closed again. Try again."
                    .into(),
            ),
            Self::NameFailed => (
                "500 Internal Server Error",
                "Namedrop couldn't name the new pane, so its tab was closed again. Try again."
                    .into(),
            ),
            Self::NameUnconfirmed(alias) => (
                "500 Internal Server Error",
                format!(
                    "Namedrop opened a tab for @{alias} but couldn't confirm its name. Check the channel before you try again."
                ),
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
    root_pane: RootPane,
}

#[derive(Deserialize)]
struct WorkspaceId {
    workspace_id: String,
}

#[derive(Deserialize)]
struct CreatedTab {
    tab: TabId,
    root_pane: RootPane,
}

#[derive(Deserialize)]
struct TabId {
    tab_id: String,
}

/// The pane Herdr opened with a new workspace or tab, and where it opened.
#[derive(Deserialize)]
struct RootPane {
    pane_id: String,
    #[serde(default)]
    cwd: Option<String>,
}

impl RootPane {
    /// Herdr opens in the home folder when it can't use the folder it was
    /// given, and says so only through where the pane is.
    fn opened_in(&self, folder: &str) -> bool {
        let same = |path: &str| std::fs::canonicalize(path).ok();
        self.cwd
            .as_deref()
            .and_then(same)
            .is_some_and(|opened| Some(opened) == same(folder))
    }
}

/// Where a channel's new agents open.
#[derive(Debug, Eq, PartialEq)]
enum ChannelFolder {
    /// Kept by Namedrop, or the workspace's checkout: it stays put.
    Fixed(String),
    /// Where the first pane is now. Herdr keeps no launch folder and a
    /// pane's folder follows `cd`, so this holds only until an agent is
    /// added and Namedrop keeps it.
    FirstPane(String),
}

impl ChannelFolder {
    fn path(&self) -> &str {
        match self {
            Self::Fixed(path) | Self::FirstPane(path) => path,
        }
    }
}

/// The folder that stays put, if the channel has one: the one Namedrop
/// kept, else the workspace's checkout.
fn fixed_folder(
    kept: &ChannelFolders,
    namespace: &StateNamespace,
    workspace: &Workspace,
) -> Option<String> {
    kept.of(namespace).map(str::to_owned).or_else(|| {
        workspace
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.checkout_path.clone())
    })
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

    /// The workspace's folder: the one that stays put, else where its first
    /// pane is now.
    fn folder(
        &self,
        kept: &ChannelFolders,
        socket: &str,
        workspace: &Workspace,
    ) -> Result<Option<ChannelFolder>, ()> {
        let namespace = StateNamespace::for_space(socket, workspace.workspace_id.clone());
        match fixed_folder(kept, &namespace, workspace) {
            Some(fixed) => Ok(Some(ChannelFolder::Fixed(fixed))),
            None => Ok(self
                .first_pane_cwd(&workspace.workspace_id)?
                .map(ChannelFolder::FirstPane)),
        }
    }

    /// The live workspace of a space, which closing it in Herdr takes away.
    fn workspace(&self, namespace: &StateNamespace) -> Result<Workspace, AgentRefusal> {
        self.workspaces()
            .map_err(|()| AgentRefusal::HerdrUnreachable)?
            .into_iter()
            .find(|workspace| workspace.workspace_id == namespace.space_id.as_str())
            .ok_or(AgentRefusal::WorkspaceClosed)
    }

    fn create_workspace(&self, folder: &str, label: &str) -> Result<CreatedWorkspace, String> {
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
            .map(|created| created.result)
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

/// The address the pane is bound to, read a few times because the hub and
/// the name step write to the same database.
fn named_address(
    state: &Path,
    namespace: &StateNamespace,
    pane: &str,
) -> Result<Option<String>, ()> {
    let read = || {
        read_state(state, |tx| {
            Ok(tx
                .active_pane_bindings(namespace)?
                .into_iter()
                .find(|binding| binding.pane_id == pane)
                .map(|binding| binding.address_id.as_str().to_owned()))
        })
    };
    let mut found = read();
    for _ in 0..8 {
        if found.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        found = read();
    }
    found
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
        let kept = ChannelFolders::read(front.files.state());
        let socket = &space.namespace.session_socket_id;
        recent_folders(&workspaces, |workspace| {
            Ok(herdr
                .folder(&kept, socket, workspace)?
                .map(|folder| folder.path().to_owned()))
        })
        .map_err(|()| FoldersRefusal::HerdrUnreachable)
    })();
    match found {
        Ok(folders) => reply_json(writer, "200 OK", json!({ "folders": folders })),
        Err(refusal) => refuse(writer, refusal.reply()),
    }
}

/// The folder the channel's next agent opens in, for the form to show.
pub(super) fn channel_folder(
    writer: &mut TcpStream,
    front: &Front,
    key: &SpaceKey,
) -> io::Result<()> {
    let state = front.files.state();
    let found = (|| {
        let space = known_space(state, key)
            .map_err(|()| AgentRefusal::StateUnavailable)?
            .ok_or(AgentRefusal::UnknownChannel)?;
        let herdr = Herdr::of(&space.namespace);
        let workspace = herdr.workspace(&space.namespace)?;
        herdr
            .folder(
                &ChannelFolders::read(state),
                &space.namespace.session_socket_id,
                &workspace,
            )
            .map_err(|()| AgentRefusal::HerdrUnreachable)
    })();
    match found {
        Ok(folder) => reply_json(
            writer,
            "200 OK",
            json!({ "folder": folder.as_ref().map(ChannelFolder::path) }),
        ),
        Err(refusal) => refuse(writer, refusal.reply()),
    }
}

/// Takes back what a failed activation saved, so the workspace that was
/// closed again isn't left as an active space. Only this space's own row is
/// touched: the `deactivate` command also removes what every space shares.
fn undo_activation(state: &Path, namespace: &StateNamespace) {
    let by = MutationAttribution::new(
        ActorKind::Human,
        "local-human",
        u64::try_from(now_ms()).unwrap_or(0),
    );
    let undone = StateStore::open(state)
        .and_then(|mut store| {
            store.transaction(|transaction| {
                if transaction
                    .worktree_state(namespace)?
                    .is_some_and(|space| space.activated && !space.deactivated)
                {
                    transaction.deactivate_worktree(namespace, &by)?;
                }
                Ok(())
            })
        })
        .map_err(|error| error.to_string());
    if let Err(error) = undone {
        eprintln!(
            "dashboard: could not take back the activation of {}: {error}",
            namespace.space_id.as_str()
        );
    }
}

/// Keeps a channel's folder, and lets go of the folders of this Herdr
/// session's closed workspaces.
fn keep_folder(front: &Front, namespace: &StateNamespace, folder: &str, live: &[Workspace]) {
    let mut kept = ChannelFolders::read(front.files.state());
    kept.keep_live(
        &namespace.session_socket_id,
        live.iter()
            .map(|workspace| workspace.workspace_id.as_str())
            .chain([namespace.space_id.as_str()]),
    );
    kept.record(namespace, folder);
    if let Err(error) = kept.save(&front.files) {
        eprintln!("dashboard: could not keep the channel's folder: {error}");
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
    // The folder was checked before this request took its turn, and can have
    // gone since. Herdr would open the workspace in the home folder.
    if !Path::new(&channel.folder).is_dir() {
        return Err(ChannelRefusal::FolderMissing);
    }
    let created = herdr
        .create_workspace(&channel.folder, &channel.name)
        .map_err(|error| {
            eprintln!("dashboard: Herdr could not create a workspace: {error}");
            ChannelRefusal::CreateFailed
        })?;
    let workspace_id = created.workspace.workspace_id;
    if !created.root_pane.opened_in(&channel.folder) {
        eprintln!(
            "dashboard: Herdr opened {workspace_id} in {:?}, not in {}",
            created.root_pane.cwd, channel.folder
        );
        herdr.undo(Undo::Workspace(workspace_id));
        return Err(ChannelRefusal::FolderNotUsed);
    }
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
        undo_activation(front.files.state(), &namespace);
        herdr.undo(Undo::Workspace(workspace_id));
        return Err(ChannelRefusal::ActivateFailed);
    }
    keep_folder(front, &namespace, &channel.folder, &live);
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
    let live = herdr
        .workspaces()
        .map_err(|()| AgentRefusal::HerdrUnreachable)?;
    let workspace = live
        .iter()
        .find(|workspace| workspace.workspace_id == namespace.space_id.as_str())
        .ok_or(AgentRefusal::WorkspaceClosed)?;
    let claim = |alias: &str| read_state(state, |tx| tx.alias_claim(&namespace, alias));
    if let Some(claim) = claim(&agent.alias).map_err(|()| AgentRefusal::StateUnavailable)? {
        return Err(AgentRefusal::NameTaken(agent.alias, claim));
    }
    let folder = herdr
        .folder(
            &ChannelFolders::read(state),
            &namespace.session_socket_id,
            workspace,
        )
        .map_err(|()| AgentRefusal::HerdrUnreachable)?
        .ok_or(AgentRefusal::FolderUnknown)?;
    if !Path::new(folder.path()).is_dir() {
        return Err(AgentRefusal::FolderGone(folder.path().to_owned()));
    }
    let tab = herdr
        .create_tab(
            &workspace.workspace_id,
            folder.path(),
            &format!("@{}", agent.alias),
        )
        .map_err(|error| {
            eprintln!("dashboard: Herdr could not open a tab: {error}");
            AgentRefusal::TabFailed
        })?;
    if !tab.root_pane.opened_in(folder.path()) {
        eprintln!(
            "dashboard: Herdr opened {} in {:?}, not in {}",
            tab.tab.tab_id,
            tab.root_pane.cwd,
            folder.path()
        );
        herdr.undo(Undo::Tab(tab.tab.tab_id));
        return Err(AgentRefusal::FolderNotUsed);
    }
    let pane = tab.root_pane.pane_id;
    if let Err(error) = run_lifecycle(
        state,
        &namespace,
        Some(&pane),
        "name",
        json!({
            "alias": agent.alias,
            "pane_type": agent.kind.pane_type(),
            "persona": agent.persona,
        }),
    ) {
        eprintln!("dashboard: naming the new pane {pane} failed: {error}");
    }
    // The name step saves the name before its last steps, so the pane's own
    // binding says whether it is named, whatever the step answered. Closing
    // the tab of a named pane would use the name up.
    let address_id = match named_address(state, &namespace, &pane) {
        Ok(Some(address_id)) => address_id,
        Ok(None) => {
            herdr.undo(Undo::Tab(tab.tab.tab_id));
            return Err(match claim(&agent.alias) {
                Ok(Some(claim)) => AgentRefusal::NameTaken(agent.alias, claim),
                _ => AgentRefusal::NameFailed,
            });
        }
        Err(()) => return Err(AgentRefusal::NameUnconfirmed(agent.alias)),
    };
    // The channel now has an agent in that folder, so the folder is the
    // channel's from here on, wherever its first pane goes.
    if let ChannelFolder::FirstPane(path) = &folder {
        keep_folder(front, &namespace, path, &live);
    }
    if let (Some(command), Some(program)) = (agent.kind.command(), agent.kind.started()) {
        // A pane that didn't start still shows as needing the person, who can
        // start the agent by hand in Herdr, so this is not a refusal.
        let started =
            crate::pane_effects::PaneEffects::acquire(state, &namespace.session_socket_id, &pane)
                .map_err(|error| error.to_string())
                .and_then(|effects| effects.start_agent(&herdr.0, command));
        if let Err(error) = started {
            eprintln!("dashboard: could not start {command} in {pane}: {error}");
        }
        front.launches.record(Launch::new(
            namespace,
            address_id.clone(),
            program,
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
    fn a_kept_folder_wins_over_the_checkout_and_a_pane_is_only_confirmed_where_it_opened() {
        let namespace = StateNamespace::for_space("socket", "w1");
        let mut kept = ChannelFolders::default();
        assert_eq!(
            fixed_folder(&kept, &namespace, &workspace("w1", None)),
            None
        );
        assert_eq!(
            fixed_folder(&kept, &namespace, &workspace("w1", Some("/repo"))),
            Some("/repo".to_owned())
        );
        kept.record(&namespace, "/kept");
        assert_eq!(
            fixed_folder(&kept, &namespace, &workspace("w1", Some("/repo"))),
            Some("/kept".to_owned())
        );
        let here = std::env::temp_dir();
        let pane = |cwd: Option<&Path>| RootPane {
            pane_id: "p".into(),
            cwd: cwd.map(|cwd| cwd.to_str().unwrap().to_owned()),
        };
        assert!(pane(Some(&here)).opened_in(here.to_str().unwrap()));
        assert!(!pane(Some(Path::new("/"))).opened_in(here.to_str().unwrap()));
        assert!(
            !pane(None).opened_in(here.to_str().unwrap()),
            "an unreported folder is not confirmed"
        );
        assert!(!pane(Some(&here.join("no-such-folder"))).opened_in(here.to_str().unwrap()));
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
            ChannelRefusal::FolderNotUsed,
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
            AgentRefusal::FolderNotUsed,
            AgentRefusal::NameFailed,
            AgentRefusal::NameUnconfirmed("coder".into()),
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
