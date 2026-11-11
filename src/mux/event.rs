use super::{PaneId, TabId};
use crate::SpaceId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSlot {
    pub space: SpaceId,
    pub pane: PaneId,
    pub tab: Option<TabId>,
}

/// A lifecycle event from any multiplexer. It carries only IDs, because the
/// handlers rediscover the space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MuxEvent {
    PaneAppeared(PaneSlot),
    PaneClosed(PaneSlot),
    PaneMoved { from: PaneSlot, to: PaneSlot },
    TabClosed { space: SpaceId, tab: TabId },
    AgentDetected(PaneSlot),
    AgentStatusChanged(PaneSlot),
}
