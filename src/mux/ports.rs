use super::{
    Allowance, Applied, Budget, Depth, Effect, Endpoint, InputOutcome, PaneId, PaneProcess, Scope,
    Screen, ScreenRequest, Survey, TabId,
};
pub use crate::pane_effects::PaneInput;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gone {
    Space,
    Tab,
    Pane,
}

/// Each variant keeps the multiplexer's own words, which `Display` prints
/// unchanged, so a caller that reports the error says what it said before.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MuxError {
    Gone(Gone, String),
    Timeout(String),
    TooLarge(String),
    Unavailable(String),
    /// The multiplexer answered something the adapter cannot parse.
    Protocol(String),
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Gone(_, text)
        | Self::Timeout(text)
        | Self::TooLarge(text)
        | Self::Unavailable(text)
        | Self::Protocol(text)) = self;
        formatter.write_str(text)
    }
}

/// Reads only, so a function bounded on `MuxRead` cannot type into a pane.
pub trait MuxRead: Send + Sync {
    fn endpoint(&self) -> &Endpoint;
    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError>;
    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError>;
    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError>;
    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError>;
    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError>;
    /// The same connection with each command held to `allowance` as well as
    /// to the connection's own budget, as a connector would make it. A read
    /// that must answer fast, such as health's integration root or the agent
    /// status panel's, reads through it.
    fn within<'a>(&'a self, allowance: Allowance<'a>) -> Box<dyn MuxRead + 'a>;
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

/// A connection a `MuxConnector` made serves wherever a `Mux` does, such as in
/// a `LifecycleRuntime`.
impl MuxRead for Arc<dyn Mux> {
    fn endpoint(&self) -> &Endpoint {
        (**self).endpoint()
    }

    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        (**self).survey(scope, depth)
    }

    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        (**self).screen(pane, request)
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        (**self).process(pane)
    }

    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        (**self).tab_label(tab)
    }

    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        (**self).integration_root()
    }

    fn within<'a>(&'a self, allowance: Allowance<'a>) -> Box<dyn MuxRead + 'a> {
        (**self).within(allowance)
    }
}

impl MuxAct for Arc<dyn Mux> {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        (**self).apply(effect)
    }
}

impl MuxInput for Arc<dyn Mux> {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        (**self).deliver(input)
    }
}

pub trait MuxConnector: Send + Sync {
    /// Every command the connection runs keeps to `budget`, and stops once
    /// `cancelled` is set.
    fn connect(
        &self,
        endpoint: &str,
        budget: Budget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Arc<dyn Mux>, MuxError>;
}
