//! New channels and new agents from the page. A channel is a Herdr workspace
//! with Namedrop active in it; an agent is a named pane in a new tab, started
//! with the plain `claude` or `codex` command. Herdr is only ever reached on
//! the socket of a space Namedrop already knows, through the multiplexer port.
//! A Rex session refuses both before anything is read, since Namedrop opens
//! no Rex session or tab yet.
use super::super::channel_folders::ChannelFolders;
use super::super::hub::{HubCommand, now_ms};
use super::super::launches::{Launch, run_lifecycle};
use super::super::snapshot::{SpaceKey, StartedAgent};
use super::{Front, Request, read_json_body, respond, respond_status};
use crate::mux::{
    Budget, Closing, Depth, Endpoint, InputOutcome, Mux, MuxError, MuxKind, NewPane, NewSpace,
    NewTab, Scope, SpaceView,
};
use crate::pane_effects::DashboardPane;
use crate::store::AliasClaim;
use crate::store::dashboard::SpaceRow;
use crate::{ActorKind, MutationAttribution, SpaceId, StateNamespace, StateStore, WorkspaceLabel};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Read},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const MAX_CHANNEL_NAME_CHARS: usize = 64;
const MAX_RECENT_FOLDERS: usize = 12;
const UNREACHABLE: &str = "Herdr isn't reachable. Check that Herdr is running, then try again.";
const UNKNOWN_CHANNEL: &str =
    "Namedrop doesn't know this channel. Reload the page, then try again.";
const RESTARTING: &str = "Channels is restarting. Try again in a moment.";
const STATE_UNAVAILABLE: &str = "Namedrop couldn't read its state. Try again.";
const NO_REX_CHANNELS: &str = "Channels can't create a channel in Rex yet. Start a Rex session, activate Namedrop in it, and it shows here as a channel.";
const NO_REX_AGENTS: &str =
    "Channels can't add an agent in Rex yet. Start the agent in a Rex pane, then name it there.";
const NOT_IMPLEMENTED: &str = "501 Not Implemented";

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

    /// The whole command typed into the new pane, never request input:
    /// fixed text, or for Claude Code the program that `claude_command` in
    /// Namedrop's settings names, so a new agent uses the account it selects.
    /// A setting that cannot be used refuses, rather than typing `claude`.
    fn command(self, state: &Path) -> Result<Option<String>, AgentRefusal> {
        Ok(match self {
            Self::Claude => {
                let setting = crate::update_check::claude_command(
                    state,
                    &crate::update_check::ConfigRoots::environment(),
                )
                .map_err(|fault| AgentRefusal::ClaudeCommand(fault.reason))?;
                Some(match setting.command {
                    None => "claude".to_owned(),
                    Some(command) => typed_program(
                        &crate::agent_status_model::locate("claude_command", &command)
                            .map_err(AgentRefusal::ClaudeCommand)?,
                    )
                    .map_err(AgentRefusal::ClaudeCommand)?,
                })
            }
            Self::Codex => Some("codex".to_owned()),
            Self::Terminal => None,
        })
    }
}

/// `program` as a shell line types it: bare when it is an ordinary path,
/// which every shell runs, else single-quoted. A path that is not UTF-8, or
/// that holds a control character, cannot be typed faithfully, so it is
/// refused.
fn typed_program(program: &Path) -> Result<String, String> {
    program
        .to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .map(crate::integration::shell_argument)
        .ok_or_else(|| {
            format!(
                "claude_command leads to {}, which a shell line cannot hold",
                program.display()
            )
        })
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
    InRex,
    HerdrUnreachable,
}

impl FoldersRefusal {
    fn reply(&self) -> (&'static str, String) {
        match self {
            Self::UnknownChannel => ("404 Not Found", UNKNOWN_CHANNEL.into()),
            Self::StateUnavailable => ("503 Service Unavailable", STATE_UNAVAILABLE.into()),
            Self::InRex => (NOT_IMPLEMENTED, NO_REX_CHANNELS.into()),
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
    InRex,
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
            Self::InRex => (NOT_IMPLEMENTED, NO_REX_CHANNELS.into()),
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
    InRex,
    HerdrUnreachable,
    TabFailed,
    FolderNotUsed,
    NameFailed,
    NameUnconfirmed(String),
    /// Why the Claude Code program cannot be named.
    ClaudeCommand(String),
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
            Self::InRex => (NOT_IMPLEMENTED, NO_REX_AGENTS.into()),
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
            Self::ClaudeCommand(reason) => (
                CONFLICT,
                format!(
                    "Namedrop can't tell which Claude Code program to start: {reason}. Fix claude_command in Namedrop's settings.json, then add the agent again."
                ),
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

/// Herdr opens in the home folder when it can't use the folder it was given,
/// and says so only through where the pane is.
fn opened_in(pane: &NewPane, folder: &str) -> bool {
    let same = |path: &str| std::fs::canonicalize(path).ok();
    pane.cwd
        .as_deref()
        .and_then(same)
        .is_some_and(|opened| Some(opened) == same(folder))
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
    workspace: &SpaceView,
) -> Option<String> {
    kept.of(namespace).map(str::to_owned).or_else(|| {
        workspace
            .checkout
            .as_ref()
            .and_then(|checkout| checkout.path.to_str())
            .map(str::to_owned)
    })
}

/// Whether the space is in a Rex session, where Namedrop opens nothing yet.
fn in_rex(namespace: &StateNamespace) -> bool {
    Endpoint::parse(&namespace.session_socket_id).kind == MuxKind::Rex
}

/// One Herdr session, reached on the socket of a space Namedrop knows.
struct Herdr(Arc<dyn Mux>);

impl Herdr {
    fn of(front: &Front, namespace: &StateNamespace) -> Result<Self, ()> {
        front
            .mux
            .connect(
                &namespace.session_socket_id,
                Budget::ADAPTER_DEFAULT,
                Arc::default(),
            )
            .map(Self)
            .map_err(|_| ())
    }

    fn workspaces(&self) -> Result<Vec<SpaceView>, ()> {
        self.0
            .survey(Scope::All, Depth::Spaces)
            .map(|survey| survey.spaces)
            .map_err(|_| ())
    }

    fn first_pane_cwd(&self, workspace: &SpaceId) -> Result<Option<String>, ()> {
        let panes = self
            .0
            .survey(Scope::PanesOf(workspace), Depth::Panes)
            .map_err(|_| ())?;
        Ok(panes
            .spaces
            .into_iter()
            .flat_map(|space| space.panes)
            .find(|pane| pane.space == *workspace)
            .and_then(|pane| pane.cwd))
    }

    /// The workspace's folder: the one that stays put, else where its first
    /// pane is now.
    fn folder(
        &self,
        kept: &ChannelFolders,
        socket: &str,
        workspace: &SpaceView,
    ) -> Result<Option<ChannelFolder>, ()> {
        let namespace = StateNamespace::for_space(socket, workspace.id.as_str());
        match fixed_folder(kept, &namespace, workspace) {
            Some(fixed) => Ok(Some(ChannelFolder::Fixed(fixed))),
            None => Ok(self
                .first_pane_cwd(&workspace.id)?
                .map(ChannelFolder::FirstPane)),
        }
    }

    /// The live workspace of a space, which closing it in Herdr takes away.
    fn workspace(&self, namespace: &StateNamespace) -> Result<SpaceView, AgentRefusal> {
        self.workspaces()
            .map_err(|()| AgentRefusal::HerdrUnreachable)?
            .into_iter()
            .find(|workspace| workspace.id == namespace.space_id)
            .ok_or(AgentRefusal::WorkspaceClosed)
    }

    fn create_workspace(&self, folder: &str, label: &str) -> Result<NewSpace, MuxError> {
        self.0.open_space(folder, label)
    }

    fn create_tab(
        &self,
        workspace: &SpaceId,
        folder: &str,
        label: &str,
    ) -> Result<NewTab, MuxError> {
        self.0.open_tab(workspace, folder, label)
    }

    /// Closes what a failed request created, so it leaves nothing behind.
    fn undo(&self, undo: Closing) {
        if let Err(error) = self.0.close(&undo) {
            eprintln!(
                "dashboard: could not close what a failed request created ({undo:?}): {error}"
            );
        }
    }
}

/// Recent folders, newest workspace first: Herdr numbers workspaces as it
/// opens them, so a higher id is newer.
fn recent_folders<'a>(
    workspaces: &'a [SpaceView],
    mut folder: impl FnMut(&'a SpaceView) -> Result<Option<String>, ()>,
) -> Result<Vec<String>, ()> {
    let mut newest: Vec<(usize, &SpaceView)> = workspaces.iter().enumerate().collect();
    newest.sort_by_key(|(index, workspace)| {
        let number = workspace
            .id
            .as_str()
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
        if in_rex(&space.namespace) {
            return Err(FoldersRefusal::InRex);
        }
        let herdr =
            Herdr::of(front, &space.namespace).map_err(|()| FoldersRefusal::HerdrUnreachable)?;
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
        if in_rex(&space.namespace) {
            return Err(AgentRefusal::InRex);
        }
        let herdr =
            Herdr::of(front, &space.namespace).map_err(|()| AgentRefusal::HerdrUnreachable)?;
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
fn keep_folder(front: &Front, namespace: &StateNamespace, folder: &str, live: &[SpaceView]) {
    let mut kept = ChannelFolders::read(front.files.state());
    kept.keep_live(
        &namespace.session_socket_id,
        live.iter()
            .map(|workspace| workspace.id.as_str())
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
    if in_rex(&space.namespace) {
        return Err(ChannelRefusal::InRex);
    }
    let herdr =
        Herdr::of(front, &space.namespace).map_err(|()| ChannelRefusal::HerdrUnreachable)?;
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
    let workspace_id = created.space;
    if !opened_in(&created.pane, &channel.folder) {
        eprintln!(
            "dashboard: Herdr opened {} in {:?}, not in {}",
            workspace_id.as_str(),
            created.pane.cwd,
            channel.folder
        );
        herdr.undo(Closing::Space(workspace_id));
        return Err(ChannelRefusal::FolderNotUsed);
    }
    let namespace = StateNamespace::new(
        space.namespace.session_socket_id.clone(),
        workspace_id.clone(),
    );
    if let Err(error) = run_lifecycle(
        &*front.mux,
        front.files.state(),
        &namespace,
        None,
        "activate",
        json!({"confirmed": true}),
    ) {
        eprintln!(
            "dashboard: could not activate the new workspace {}: {error}",
            workspace_id.as_str()
        );
        undo_activation(front.files.state(), &namespace);
        herdr.undo(Closing::Space(workspace_id));
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
    if in_rex(&namespace) {
        return Err(AgentRefusal::InRex);
    }
    let herdr = Herdr::of(front, &namespace).map_err(|()| AgentRefusal::HerdrUnreachable)?;
    let live = herdr
        .workspaces()
        .map_err(|()| AgentRefusal::HerdrUnreachable)?;
    let workspace = live
        .iter()
        .find(|workspace| workspace.id == namespace.space_id)
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
    let command = agent.kind.command(state)?;
    let tab = herdr
        .create_tab(&workspace.id, folder.path(), &format!("@{}", agent.alias))
        .map_err(|error| {
            eprintln!("dashboard: Herdr could not open a tab: {error}");
            AgentRefusal::TabFailed
        })?;
    if !opened_in(&tab.pane, folder.path()) {
        eprintln!(
            "dashboard: Herdr opened {} in {:?}, not in {}",
            tab.tab.as_str(),
            tab.pane.cwd,
            folder.path()
        );
        herdr.undo(Closing::Tab(tab.tab));
        return Err(AgentRefusal::FolderNotUsed);
    }
    let pane = tab.pane.id.as_str().to_owned();
    if let Err(error) = run_lifecycle(
        &*front.mux,
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
            herdr.undo(Closing::Tab(tab.tab));
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
    if let (Some(command), Some(program)) = (command, agent.kind.started()) {
        // A pane that didn't start still shows as needing the person, who can
        // start the agent by hand in Herdr, so this is not a refusal.
        let started = DashboardPane::acquire_new(state, &namespace.session_socket_id, &pane)
            .map_err(|error| error.to_string())
            .and_then(|effects| match effects.start_agent(&herdr.0, &command) {
                InputOutcome::Delivered => Ok(()),
                InputOutcome::Declined(_, error)
                | InputOutcome::Refused(error)
                | InputOutcome::Ambiguous(error) => Err(error),
            });
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

    /// A program is typed bare when every shell runs it so, quoted when it
    /// must be, and refused when a shell line cannot hold it.
    #[test]
    fn the_claude_program_is_typed_as_a_shell_runs_it_or_refused() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            typed_program(Path::new("/Users/me/.local/bin/claude-personal")),
            Ok("/Users/me/.local/bin/claude-personal".to_owned())
        );
        assert_eq!(
            typed_program(Path::new("/Users/me/my bin/it's claude")),
            Ok("'/Users/me/my bin/it'\"'\"'s claude'".to_owned())
        );
        for refused in [
            Path::new("/Users/me/bin/claude\tpersonal"),
            Path::new("/Users/me/bin/claude\npersonal"),
            Path::new(std::ffi::OsStr::from_bytes(b"/Users/me/bin/claude\xff")),
        ] {
            let reason = typed_program(refused).unwrap_err();
            assert!(
                reason.contains("which a shell line cannot hold"),
                "{reason}"
            );
        }
    }

    fn workspace(id: &str, checkout: Option<&str>) -> SpaceView {
        SpaceView {
            id: SpaceId::new(id),
            label: None,
            number: None,
            checkout: checkout.map(|path| crate::mux::Checkout {
                path: path.into(),
                label: None,
                branch: None,
                repo_key: None,
                repo_name: None,
            }),
            worktree: None,
            panes: Vec::new(),
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
        let pane = |cwd: Option<&Path>| NewPane {
            id: crate::mux::PaneId::new("p"),
            cwd: cwd.map(|cwd| cwd.to_str().unwrap().to_owned()),
        };
        assert!(opened_in(&pane(Some(&here)), here.to_str().unwrap()));
        assert!(!opened_in(
            &pane(Some(Path::new("/"))),
            here.to_str().unwrap()
        ));
        assert!(
            !opened_in(&pane(None), here.to_str().unwrap()),
            "an unreported folder is not confirmed"
        );
        assert!(!opened_in(
            &pane(Some(&here.join("no-such-folder"))),
            here.to_str().unwrap()
        ));
    }

    #[test]
    fn recent_folders_are_newest_first_distinct_capped_and_prefer_the_checkout() {
        let mut workspaces = vec![
            workspace("w2", Some("/repo/two")),
            workspace("w10", None),
            workspace("w3", Some("/repo/two")),
            workspace("w1", Some("/repo/one")),
        ];
        let cwd = |workspace: &SpaceView| -> Result<Option<String>, ()> {
            Ok(match workspace.checkout.as_ref() {
                Some(checkout) => checkout.path.to_str().map(str::to_owned),
                None => Some(format!("/cwd/{}", workspace.id.as_str())),
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
            [AgentKind::Claude, AgentKind::Codex, AgentKind::Terminal].map(AgentKind::pane_type),
            [
                crate::PaneType::Agent,
                crate::PaneType::Agent,
                crate::PaneType::Terminal,
            ]
        );
        // Claude Code's command depends on the settings, so the server test
        // covers it with its own HOME; these two read none.
        let nowhere = Path::new("/nonexistent/state");
        assert_eq!(
            [AgentKind::Codex, AgentKind::Terminal].map(|kind| kind.command(nowhere).unwrap()),
            [Some("codex".to_owned()), None]
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
            ChannelRefusal::InRex,
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
            AgentRefusal::InRex,
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
            FoldersRefusal::InRex,
            FoldersRefusal::HerdrUnreachable,
        ]
        .map(|refusal| refusal.reply());
        for (status, sentence) in channel.iter().chain(&agent).chain(&folders) {
            assert!(status.starts_with(['4', '5']), "{status}");
            assert!(sentence.ends_with('.'), "{sentence}");
            assert!(!sentence.to_lowercase().contains("operator"), "{sentence}");
            assert!(!sentence.contains('\u{2014}'), "{sentence}");
        }
        // A Rex session's refusals name Rex and none of Herdr's words.
        for (status, sentence) in [
            ChannelRefusal::InRex.reply(),
            AgentRefusal::InRex.reply(),
            FoldersRefusal::InRex.reply(),
        ] {
            assert_eq!(status, NOT_IMPLEMENTED);
            assert!(sentence.contains("Rex"), "{sentence}");
            for word in ["Herdr", "workspace", "space ", "tab"] {
                assert!(!sentence.contains(word), "{sentence}");
            }
        }
    }
}
