//! Which multiplexer this process runs in. An explicit endpoint decides by its
//! kind. A process that inherited a Rex claim (`REX_BLOCK` and `REX_SESSION`)
//! is served by the multiplexer whose server is its nearest ancestor, proved
//! by the peer pid of each claimed socket with no request sent. Any doubt
//! about Herdr is resolved as Herdr, so every Herdr process keeps today's
//! path. Everything else reads step 3's set of names.

use super::{
    Endpoint, MuxKind, PaneId,
    processes::{LocalProcesses, ThisMachine},
    rex::Caller,
};
use crate::{
    SpaceId,
    locator::{Locator, Name},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Home {
    /// Today's path. The Herdr runtime builder runs unchanged, with step 3's
    /// Herdr set of names and `verify_inherited_pane`.
    Herdr,
    Rex(Place),
    /// An agent's sandbox denied the Rex socket, and the block's binding
    /// proved the caller. Only `deferred_commands::try_defer` accepts it.
    RexDeferred(Deferral),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Place {
    /// Canonical when it came from a verified claim, as the server reports
    /// its primary listener.
    pub endpoint: Endpoint,
    pub space: Option<SpaceId>,
    pub pane: Option<PaneId>,
    /// What the verifying read showed about the caller's block. Present only
    /// when an inherited `REX_BLOCK` verified, so only then can a hook bind.
    pub(crate) caller: Option<Box<Caller>>,
}

/// Why a hook did not bind its session to its block: the conditions of
/// `Place::bind_session` in order, or a write that failed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindRefused {
    /// The hook's agent is not in the foreground's process group.
    NotInGroup,
    /// Something other than shells lies between the foreground and the
    /// hook's agent, or the foreground runs the agent's own executable.
    Launcher,
    /// The foreground's status record names another runtime.
    Runtime,
    /// The foreground or the hook's agent is not a live process here.
    NotAlive,
    /// The block's binding already names the foreground as its agent.
    AlreadyBound,
    /// A Claude Code in print mode behind the foreground.
    NestedPrintMode,
    /// The place came from flags or names, not a verified block.
    NoCaller,
    Io(String),
}

impl Home {
    pub fn kind(&self) -> MuxKind {
        match self {
            Self::Herdr => MuxKind::Herdr,
            Self::Rex(_) | Self::RexDeferred(_) => MuxKind::Rex,
        }
    }

    /// Where this process records and reads its turn. Herdr: step 3's set of
    /// names, read as the hooks read them before the home existed. Rex: the
    /// place. `None` for `RexDeferred`, which only queues a command.
    pub fn location(&self, inherited: &dyn Inherited) -> Option<Location> {
        match self {
            Self::Herdr => {
                let lookup = |name: &str| inherited.var(name);
                let read = |locator: Locator| {
                    locator
                        .resolve(lookup)
                        .and_then(|(_, value)| value.into_string().ok())
                };
                Some(Location {
                    endpoint: read(Locator::Endpoint).map(|value| Endpoint::parse(&value)),
                    space: read(Locator::Space).map(SpaceId::new),
                    pane: read(Locator::Pane).map(PaneId::new),
                    names: crate::locator::home(lookup),
                })
            }
            Self::Rex(place) => Some(Location {
                endpoint: Some(place.endpoint.clone()),
                space: place.space.clone(),
                pane: place.pane.clone(),
                names: Name::Neutral,
            }),
            Self::RexDeferred(_) => None,
        }
    }
}

/// Where a hook records its turn: endpoint, space, and pane, from either home.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Location {
    pub endpoint: Option<Endpoint>,
    pub space: Option<SpaceId>,
    pub pane: Option<PaneId>,
    /// The set of names the values came from, which a Herdr pane's
    /// verification names in its refusal.
    pub names: Name,
}

/// A place read from a binding file. It can queue a deferred command and
/// nothing else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deferral {
    pub endpoint: Endpoint,
    pub space: SpaceId,
    pub pane: PaneId,
}

/// The location flags a command was given.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags<'a> {
    pub endpoint: Option<&'a str>,
    pub space: Option<&'a str>,
    pub pane: Option<&'a str>,
    /// `--plugin-state`, where a sandboxed command finds its block's binding.
    pub plugin_state: Option<&'a Path>,
}

/// What a Rex server said about a claimed block over one connection that
/// proved it local.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claimed {
    /// `rex:` and the server's primary Unix socket.
    pub endpoint: Endpoint,
    /// The server's pid, which its own status names and the connection's peer
    /// pid matches.
    pub server: u32,
    /// The block's child process.
    pub child: u32,
    /// The block's foreground and its status record, read on the same
    /// connection.
    pub(crate) caller: Caller,
}

/// What this process inherited: its environment, its ancestors, and what a
/// connection to a socket shows. `ThisProcess` reads the real ones, and tests
/// script them.
pub trait Inherited {
    fn var(&self, name: &str) -> Option<OsString>;
    /// This process's pid.
    fn id(&self) -> u32;
    /// This process's ancestors, its parent first.
    fn ancestors(&self) -> Vec<u32>;
    /// One non-blocking connect to `socket`, bounded at 20 ms, and the pid of
    /// the process at its other end. Nothing is sent.
    fn peer(&self, socket: &Path) -> io::Result<u32>;
    /// One connection to the Rex server at `socket` that proves it local,
    /// attaches `session`, and reads `block`'s process.
    fn claimed(&self, socket: &Path, session: &str, block: &str) -> Result<Claimed, String>;
    /// `<pid>@<start>` of a live process, or `None` when it is gone or
    /// cannot be read.
    fn incarnation(&self, pid: u32) -> Option<String>;
}

/// This process, read from the system. Built only in `fn main`, so nothing
/// else reads the real environment or sockets to choose a home.
pub struct ThisProcess;

impl Inherited for ThisProcess {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn id(&self) -> u32 {
        std::process::id()
    }

    fn ancestors(&self) -> Vec<u32> {
        super::processes::ancestors(&ThisMachine, std::process::id())
    }

    fn peer(&self, socket: &Path) -> io::Result<u32> {
        super::rex::peer_within(socket, std::time::Duration::from_millis(20))
    }

    fn claimed(&self, socket: &Path, session: &str, block: &str) -> Result<Claimed, String> {
        super::rex::claimed(socket, session, block)
    }

    fn incarnation(&self, pid: u32) -> Option<String> {
        match ThisMachine.read(pid) {
            super::processes::LocalRead::Alive(process) => Some(process.incarnation.0),
            _ => None,
        }
    }
}

/// What one peer probe showed about a claimed socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Probe {
    /// The server is this many parents up: 0 is the parent.
    Ancestor(usize),
    /// A live server that is not an ancestor, on a path a variable named.
    Stranger,
    /// `ENOENT` on a path a variable named.
    NoServer,
    /// Refused, would block, `ENOENT` or a stranger on a guessed default
    /// path, a path too long for a socket, no answer within 20 ms, or any
    /// other error. A full listener refuses at once, so a refusal proves
    /// nothing.
    Doubt,
    /// `EPERM` or `EACCES`, as an agent's sandbox answers.
    Denied,
}

/// A claimed socket and whether a variable named it or it is the default.
struct Claim {
    socket: PathBuf,
    named: bool,
}

fn probe(inherited: &dyn Inherited, claim: &Claim, ancestors: &[u32]) -> Probe {
    match inherited.peer(&claim.socket) {
        Ok(peer) => match ancestors.iter().position(|pid| *pid == peer) {
            Some(depth) => Probe::Ancestor(depth),
            None if claim.named => Probe::Stranger,
            None => Probe::Doubt,
        },
        Err(error) => match error.raw_os_error() {
            Some(libc::EPERM | libc::EACCES) => Probe::Denied,
            Some(libc::ENOENT) if claim.named => Probe::NoServer,
            _ => Probe::Doubt,
        },
    }
}

/// The answer of the home table for a Rex claim, before the block is verified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Choice {
    Rex,
    Herdr,
    /// No Rex server is an ancestor, and no Herdr name is set.
    Lost,
    /// The Rex socket is denied, and no Herdr name is set.
    Denied,
}

/// The home table. `herdr` is `None` when no Herdr location name is set, so
/// Herdr is never probed.
pub(crate) fn choose(rex: Probe, herdr: Option<Probe>) -> Choice {
    match (rex, herdr) {
        (Probe::Ancestor(_), None) => Choice::Rex,
        (Probe::Ancestor(r), Some(Probe::Ancestor(h))) if r < h => Choice::Rex,
        (Probe::Ancestor(_), Some(Probe::Ancestor(_))) => Choice::Herdr,
        (Probe::Ancestor(_), Some(Probe::Stranger | Probe::NoServer)) => Choice::Rex,
        (Probe::Ancestor(_), Some(Probe::Doubt | Probe::Denied)) => Choice::Herdr,
        (Probe::Stranger | Probe::NoServer | Probe::Doubt, Some(_)) => Choice::Herdr,
        (Probe::Stranger | Probe::NoServer | Probe::Doubt, None) => Choice::Lost,
        (Probe::Denied, Some(_)) => Choice::Herdr,
        (Probe::Denied, None) => Choice::Denied,
    }
}

/// A variable's value, with an empty one read as unset.
fn set(inherited: &dyn Inherited, name: &str) -> Option<String> {
    inherited
        .var(name)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
}

/// Rex's claimed socket: `REX_SERVER` when it names a Unix socket, as `rex`
/// reads it (a percent-encoded `unix://` URL or a bare absolute path, taken
/// as written), else Rex's default socket.
fn rex_claim(inherited: &dyn Inherited) -> Option<Claim> {
    if let Some(server) = set(inherited, "REX_SERVER") {
        let socket = match server.strip_prefix("unix://") {
            Some(path) => percent_decoded(path),
            None if server.starts_with('/') => server,
            None => return None,
        };
        return Some(Claim {
            socket: PathBuf::from(socket),
            named: true,
        });
    }
    #[cfg(target_os = "macos")]
    let default =
        set(inherited, "HOME").map(|home| Path::new(&home).join("Library/Application Support/rex"));
    #[cfg(not(target_os = "macos"))]
    let default = set(inherited, "XDG_RUNTIME_DIR").map(|runtime| Path::new(&runtime).join("rex"));
    default.map(|directory| Claim {
        socket: directory.join("server.sock"),
        named: false,
    })
}

/// Herdr's claimed socket: `HERDR_SOCKET_PATH`, else Herdr's default.
fn herdr_claim(inherited: &dyn Inherited) -> Option<Claim> {
    if let Some(socket) = set(inherited, Locator::Endpoint.herdr_var()) {
        return Some(Claim {
            socket: PathBuf::from(socket),
            named: true,
        });
    }
    set(inherited, "XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| set(inherited, "HOME").map(|home| Path::new(&home).join(".config")))
        .map(|config| Claim {
            socket: config.join("herdr/herdr.sock"),
            named: false,
        })
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |offset: usize| {
            bytes
                .get(at + offset)
                .and_then(|byte| (*byte as char).to_digit(16))
        };
        match (bytes[at], hex(1), hex(2)) {
            (b'%', Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                at += 3;
            }
            (byte, _, _) => {
                decoded.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Where this process runs.
///
/// 1. An explicit endpoint decides by `Endpoint::parse`, with no probe. A
///    Rex place then trusts `--space` and `--pane`, as `--herdr-pane` is
///    trusted today.
/// 2. With `REX_BLOCK` and `REX_SESSION` set, the nearest server ancestor
///    decides, and a Rex home reads its place from the verified claim and
///    ignores every `HERDR_*` and `NAMEDROP_*` location name.
/// 3. Otherwise step 3's set of names: Herdr's when a Herdr location name is
///    set, else the neutral set, whose `NAMEDROP_ENDPOINT` is parsed for its
///    kind.
pub fn resolve(flags: &Flags<'_>, inherited: &dyn Inherited) -> Result<Home, String> {
    let place = |endpoint: Endpoint, space: Option<String>, pane: Option<String>| Place {
        endpoint,
        space: flags.space.map(SpaceId::new).or(space.map(SpaceId::new)),
        pane: flags.pane.map(PaneId::new).or(pane.map(PaneId::new)),
        caller: None,
    };
    if let Some(endpoint) = flags.endpoint {
        let endpoint = Endpoint::parse(endpoint);
        return Ok(match endpoint.kind {
            MuxKind::Herdr => Home::Herdr,
            MuxKind::Rex => Home::Rex(place(endpoint, None, None)),
        });
    }
    if let (Some(block), Some(session)) =
        (set(inherited, "REX_BLOCK"), set(inherited, "REX_SESSION"))
    {
        return nearest(flags, inherited, &session, &block).map(|nearest| match nearest {
            Nearest::Herdr => Home::Herdr,
            Nearest::Rex(endpoint, caller) => Home::Rex(Place {
                caller: Some(Box::new(caller)),
                ..place(endpoint, Some(session), Some(block))
            }),
            Nearest::Deferred(deferral) => Home::RexDeferred(deferral),
        });
    }
    let lookup = |name: &str| inherited.var(name);
    match Locator::Endpoint.resolve(lookup) {
        Some((Name::Neutral, endpoint)) => {
            let endpoint = Endpoint::parse(&endpoint.to_string_lossy());
            if endpoint.kind == MuxKind::Rex {
                let neutral = |locator: Locator| {
                    locator
                        .resolve(lookup)
                        .and_then(|(_, value)| value.into_string().ok())
                };
                return Ok(Home::Rex(place(
                    endpoint,
                    neutral(Locator::Space),
                    neutral(Locator::Pane),
                )));
            }
            Ok(Home::Herdr)
        }
        _ => Ok(Home::Herdr),
    }
}

/// What the nearest server ancestor makes of a Rex claim.
enum Nearest {
    Herdr,
    /// The verified endpoint and what it showed of the caller's block.
    Rex(Endpoint, Caller),
    /// The socket is denied, and the block's binding proved the caller.
    Deferred(Deferral),
}

fn nearest(
    flags: &Flags<'_>,
    inherited: &dyn Inherited,
    session: &str,
    block: &str,
) -> Result<Nearest, String> {
    let ancestors = inherited.ancestors();
    let rex = rex_claim(inherited);
    let rex_probe = rex
        .as_ref()
        .map_or(Probe::Doubt, |claim| probe(inherited, claim, &ancestors));
    let herdr_named = [Locator::Endpoint, Locator::Space, Locator::Pane]
        .iter()
        .any(|locator| inherited.var(locator.herdr_var()).is_some());
    let herdr_probe = herdr_named.then(|| {
        herdr_claim(inherited).map_or(Probe::Doubt, |claim| probe(inherited, &claim, &ancestors))
    });
    let server = || set(inherited, "REX_SERVER").unwrap_or_else(|| "unset".to_owned());
    match choose(rex_probe, herdr_probe) {
        Choice::Herdr => Ok(Nearest::Herdr),
        Choice::Lost => Err(format!(
            "REX_BLOCK={block} is on a Rex server this process cannot find (REX_SERVER={}); start that server with REX_SERVER set",
            server()
        )),
        Choice::Denied => deferral(flags, inherited, block, &ancestors)
            .map(Nearest::Deferred)
            .map_err(|why| {
                format!(
                    "the agent's sandbox denies the Rex socket that REX_SERVER={} names, and {why}",
                    server()
                )
            }),
        Choice::Rex => {
            let (Some(claim), Probe::Ancestor(depth)) = (rex, rex_probe) else {
                unreachable!("Rex is chosen only for a server that is an ancestor")
            };
            let claimed = inherited.claimed(&claim.socket, session, block)?;
            if claimed.server != ancestors[depth] {
                return Err(format!(
                    "the Rex server at {} is process {}, not this process's ancestor {}",
                    claim.socket.display(),
                    claimed.server,
                    ancestors[depth]
                ));
            }
            // A launcher that `exec`s Namedrop makes it the block's child.
            if claimed.child != inherited.id() && !ancestors.contains(&claimed.child) {
                return Err(format!(
                    "REX_BLOCK={block} does not run this process: its child {} is neither this process nor an ancestor",
                    claimed.child
                ));
            }
            Ok(Nearest::Rex(claimed.endpoint, claimed.caller.of(ancestors)))
        }
    }
}

/// The place a sandboxed command defers to: the block's binding, when its
/// agent is an ancestor of this process with the bound start time, as a
/// tool's agent is. The binding is read from `--plugin-state`, else from
/// a Rex home's state directory.
fn deferral(
    flags: &Flags<'_>,
    inherited: &dyn Inherited,
    block: &str,
    ancestors: &[u32],
) -> Result<Deferral, String> {
    let state = flags.plugin_state.map(Path::to_path_buf).or_else(|| {
        crate::locator::state_directory(None, MuxKind::Rex, |name| inherited.var(name))
    });
    let (deferral, agent) = state
        .and_then(|state| super::rex::bound(&state, block))
        .ok_or_else(|| format!("no Namedrop hook has bound REX_BLOCK={block} yet"))?;
    let proves = agent
        .split_once('@')
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
        .is_some_and(|pid| {
            ancestors.contains(&pid) && inherited.incarnation(pid).as_deref() == Some(&agent)
        });
    if !proves {
        return Err(format!(
            "the agent {agent} bound to REX_BLOCK={block} is not an ancestor of this process"
        ));
    }
    Ok(deferral)
}

#[cfg(test)]
mod tests;
