use super::{PaneFacts, PaneId, TabId};
use std::path::PathBuf;

/// Texts a named pane shows for each agent state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateTexts {
    pub ready: String,
    pub working: String,
    pub attention: String,
    pub unknown: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Badge {
    Awaiting(u64),
    Attention(u64),
    Relay { alias: String, summary: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PopupScreen {
    Name,
    Manage,
    Doctor,
    Status,
    Tour,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PopupSpec {
    pub screen: PopupScreen,
    /// The pane the popup acts for, as the caller discovered it.
    pub target: PaneFacts,
    pub state_dir: PathBuf,
    /// The runtime's `session_socket_id`, which the popup's own Namedrop
    /// reconnects to.
    pub socket: String,
}

/// Everything Namedrop does to a multiplexer other than typing into a pane.
/// Each effect is safe to repeat after a crash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Identify {
        pane: PaneId,
        alias: String,
        texts: StateTexts,
    },
    Badges {
        pane: PaneId,
        badges: Vec<Badge>,
    },
    RenameTab {
        tab: TabId,
        label: String,
    },
    Notify {
        title: String,
        body: String,
        sound: bool,
    },
    OpenPopup(Box<PopupSpec>),
    ReloadKeybindings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Applied {
    Done,
    /// The popup opened. Carries the multiplexer's own reply, which
    /// `namedrop ui` and the tour report print unchanged.
    Opened(serde_json::Value),
    /// The multiplexer cannot show this effect. Callers ignore it.
    Unsupported,
}
