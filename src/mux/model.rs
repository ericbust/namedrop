use crate::{AgentRuntime, AgentSessionId, SpaceId, domain::string_id};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

string_id!(PaneId);
string_id!(TabId);
string_id!(TerminalId);

/// The multiplexer server a port talks to. For Herdr, `key` is the stored
/// `session_socket_id`, byte for byte.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    pub kind: MuxKind,
    pub key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MuxKind {
    Herdr,
    Rex,
}

/// Limits fixed when a connection is made, so no port method takes one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentState {
    Idle,
    Working,
    Done,
    Blocked,
    /// Keeps the multiplexer's word, so status output prints what it printed before.
    Unknown(String),
}

impl AgentState {
    pub fn parse(status: &str) -> Self {
        match status {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Unknown(status) => status,
        }
    }

    pub fn accepts_prompt(&self) -> bool {
        matches!(self, Self::Idle | Self::Done)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentKind {
    Known(AgentRuntime),
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity {
    pub kind: AgentKind,
    pub label: Option<String>,
    pub session: SessionClaim,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionClaim {
    Reported(AgentSessionId),
    /// The multiplexer names no session. `incarnation` changes whenever a new
    /// program starts in the pane, so a binding to an older program is lost.
    Unreported {
        incarnation: Incarnation,
    },
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Incarnation(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneFacts {
    pub id: PaneId,
    pub terminal: TerminalId,
    pub tab: TabId,
    pub space: SpaceId,
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// `None` exactly when the multiplexer reports neither an agent kind nor a
    /// session: a free terminal pane.
    pub agent: Option<AgentIdentity>,
    pub state: Option<AgentState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkout {
    pub path: std::path::PathBuf,
    pub label: Option<String>,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpaceView {
    pub id: SpaceId,
    pub label: Option<String>,
    pub number: Option<u32>,
    /// `None` when the space is not a Git checkout.
    pub checkout: Option<Checkout>,
    /// Empty for a `Depth::Spaces` survey.
    pub panes: Vec<PaneFacts>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Survey {
    pub spaces: Vec<SpaceView>,
}

#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    Space(&'a SpaceId),
    CheckoutOf(&'a Path),
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Depth {
    Spaces,
    Panes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wrap {
    AsDrawn,
    Joined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Style {
    Plain,
    Ansi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenRequest {
    pub lines: usize,
    pub wrap: Wrap,
    pub style: Style,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Screen {
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneProcess {
    pub shell_pid: Option<u32>,
    pub foreground_group: Option<u32>,
    pub foreground_pids: Vec<u32>,
    pub tty: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Enter,
    Escape,
    CtrlC,
    CtrlU,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_reads_the_multiplexer_status_and_prints_it_back() {
        for (status, state) in [
            ("idle", AgentState::Idle),
            ("working", AgentState::Working),
            ("done", AgentState::Done),
            ("blocked", AgentState::Blocked),
            ("busy", AgentState::Unknown("busy".into())),
            ("Idle", AgentState::Unknown("Idle".into())),
            ("", AgentState::Unknown(String::new())),
        ] {
            let parsed = AgentState::parse(status);
            assert_eq!(parsed, state, "{status:?}");
            assert_eq!(parsed.as_str(), status);
        }
    }

    #[test]
    fn only_idle_and_done_accept_a_prompt() {
        let accepting: Vec<_> = ["idle", "working", "done", "blocked", "ready"]
            .into_iter()
            .filter(|status| AgentState::parse(status).accepts_prompt())
            .collect();
        assert_eq!(accepting, ["idle", "done"]);
    }
}
