use super::{PaneId, TabId};
use crate::SpaceId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneSlot {
    pub space: SpaceId,
    pub pane: PaneId,
    pub tab: Option<TabId>,
}

/// A lifecycle event from any multiplexer. It carries only IDs, because the
/// handlers rediscover the space.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuxEvent {
    PaneAppeared(PaneSlot),
    PaneClosed(PaneSlot),
    PaneMoved {
        from: PaneSlot,
        to: PaneSlot,
    },
    TabClosed {
        space: SpaceId,
        tab: TabId,
    },
    AgentDetected(PaneSlot),
    AgentStatusChanged(PaneSlot),
    /// Events for this space may have been missed, as when the Rex bridge
    /// connects again.
    Resync(SpaceId),
    /// The space and every pane in it are gone for good.
    SpaceClosed(SpaceId),
}
