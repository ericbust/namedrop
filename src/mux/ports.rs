use super::{
    Applied, Budget, Depth, Effect, Endpoint, InputOutcome, PaneId, PaneProcess, Scope, Screen,
    ScreenRequest, Survey, TabId,
};
pub use crate::pane_effects::PaneInput;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gone {
    Space,
    Tab,
    Pane,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MuxError {
    Gone(Gone),
    Timeout,
    TooLarge,
    Unavailable(String),
    /// The multiplexer answered something the adapter cannot parse.
    Protocol(String),
}

/// Reads only, so a function bounded on `MuxRead` cannot type into a pane.
pub trait MuxRead: Send + Sync {
    fn endpoint(&self) -> &Endpoint;
    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError>;
    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError>;
    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError>;
    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError>;
    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError>;
}

pub trait MuxAct: MuxRead {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError>;
}

/// Every implementation sends a prompt only if the pane is admitted at send
/// time, reports any failure after the first byte as `Ambiguous`, and lets no
/// unclassified error escape. Multiplexers without a native gate implement
/// `deliver` with `gate::emulate_prompt`.
pub trait MuxInput: MuxRead {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome;
}

pub trait Mux: MuxAct + MuxInput {}

impl<T: MuxAct + MuxInput + ?Sized> Mux for T {}

pub trait MuxConnector: Send + Sync {
    fn connect(&self, endpoint: &str, budget: Budget) -> Result<Arc<dyn Mux>, MuxError>;
}
