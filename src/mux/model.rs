use crate::{AgentRuntime, AgentSessionId, SpaceId, WorktreeId, domain::string_id};
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

impl Budget {
    /// No limit but the adapter's own: a Herdr command still stops after
    /// `NAMEDROP_HERDR_TIMEOUT_MS`.
    pub const ADAPTER_DEFAULT: Self = Self {
        timeout: Duration::MAX,
        max_output_bytes: usize::MAX,
    };
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

/// Serializes as the multiplexer's word, so JSON output is unchanged.
impl Serialize for AgentState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentKind {
    Known(AgentRuntime),
    Other(String),
    /// The multiplexer reports a session but no agent kind.
    Unreported,
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

/// The session a claim binds to. Only a reported session resolves until a
/// multiplexer that reports none records its own binding.
pub fn resolve_session(claim: &SessionClaim) -> Option<&AgentSessionId> {
    match claim {
        SessionClaim::Reported(session) => Some(session),
        SessionClaim::Unreported { .. } | SessionClaim::None => None,
    }
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
    /// The text the multiplexer shows for this pane while its agent is idle.
    pub idle_label: Option<String>,
    /// The rows of the pane's visible screen, when the multiplexer reports them.
    pub viewport_rows: Option<u64>,
    /// Whether the multiplexer said anything about this pane's agent session.
    /// When it did, a missing session means none runs. When it did not, a
    /// missing session means nothing is known.
    pub reports_session: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkout {
    pub path: std::path::PathBuf,
    pub label: Option<String>,
    pub branch: Option<String>,
    pub repo_key: Option<String>,
    pub repo_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpaceView {
    pub id: SpaceId,
    pub label: Option<String>,
    pub number: Option<u32>,
    /// `None` when the space is not a Git checkout, or when the survey did
    /// not read the checkout (see `Scope`).
    pub checkout: Option<Checkout>,
    /// The key Namedrop stores the space's checkout state under: the checkout
    /// path, or a multiplexer-chosen key for a space that is not a checkout.
    /// `None` when the survey did not resolve it.
    pub worktree: Option<WorktreeId>,
    /// Empty for a `Depth::Spaces` survey.
    pub panes: Vec<PaneFacts>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Survey {
    pub spaces: Vec<SpaceView>,
}

/// Which spaces a survey reads. Each scope reads only what the multiplexer
/// reports in as few requests as today's code makes, so some facts stay unread.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    /// One space, with its checkout and `worktree` key resolved. A
    /// `Depth::Spaces` survey reads only the space's label and stamped checkout.
    Space(&'a SpaceId),
    /// One space's panes alone, as `All` with `Depth::Panes` reads them: no
    /// label, checkout, or `worktree` key. Either depth reads the panes.
    PanesOf(&'a SpaceId),
    /// The one space that holds this checkout. More than one, or none, is an
    /// error.
    CheckoutOf(&'a Path),
    /// Every space. `Depth::Spaces` reads labels, numbers, and stamped
    /// checkouts. `Depth::Panes` reads only panes, grouped by space.
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
    fn only_a_reported_session_resolves() {
        let session = AgentSessionId::new("session-1");
        assert_eq!(
            resolve_session(&SessionClaim::Reported(session.clone())),
            Some(&session)
        );
        let unreported = SessionClaim::Unreported {
            incarnation: Incarnation("pid-1".into()),
        };
        assert_eq!(resolve_session(&unreported), None);
        assert_eq!(resolve_session(&SessionClaim::None), None);
    }

    #[test]
    fn agent_state_serializes_as_the_multiplexer_word() {
        assert_eq!(
            serde_json::to_value([AgentState::Done, AgentState::Unknown("unknown".into())])
                .unwrap(),
            serde_json::json!(["done", "unknown"])
        );
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
