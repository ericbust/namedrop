use super::{PaneId, TabId};
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
    pub target: PaneId,
    pub state_dir: PathBuf,
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
    OpenPopup(PopupSpec),
    ReloadKeybindings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Applied {
    Done,
    /// The multiplexer cannot show this effect. Callers ignore it.
    Unsupported,
}
