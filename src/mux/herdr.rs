//! The Herdr adapter: the only place that holds Herdr arguments, JSON, error
//! codes, and plugin events. Namedrop runs Herdr through it, except for the
//! live-peek observer in `dashboard/observer_guard.rs`, which starts `herdr
//! terminal session observe` until step 4 moves it here, and the update
//! check, whose embedded `scripts/check-update.py` runs `herdr plugin list`.

mod events;
mod process;
mod wire;

pub use events::{decode_operation_input, plugin_event_data};

use super::{
    Allowance, Applied, Budget, Depth, Effect, Endpoint, InputKind, InputOutcome, Mux, MuxAct,
    MuxConnector, MuxError, MuxInput, MuxKind, MuxRead, PaneId, PaneInput, PaneProcess, Scope,
    Screen, ScreenRequest, SpaceView, Survey, TabId,
};
use process::Process;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

mod transport {
    use crate::mux::{Allowance, Budget};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// What a command that was cancelled before it started answers, in the
    /// words the process transport uses.
    pub const CANCELLED: &str = "Herdr read cancelled";

    /// Runs one Herdr command and returns its stdout, or its error text.
    /// Public only inside this private module, so nothing outside the
    /// adapter can reach Herdr through it.
    pub trait Transport: Send + Sync {
        fn run(&self, args: &[&str]) -> Result<String, String>;

        /// `run` held to `budget` as well, and cancelled by `cancelled` in
        /// place of the transport's own flag when it is given. A scripted
        /// transport has no clock or output cap to hold, so it only refuses a
        /// command that is cancelled before it starts.
        fn run_within(
            &self,
            args: &[&str],
            _budget: Budget,
            cancelled: Option<&AtomicBool>,
        ) -> Result<String, String> {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err(CANCELLED.to_owned());
            }
            self.run(args)
        }
    }

    impl<T: Transport + ?Sized> Transport for &T {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            (**self).run(args)
        }

        fn run_within(
            &self,
            args: &[&str],
            budget: Budget,
            cancelled: Option<&AtomicBool>,
        ) -> Result<String, String> {
            (**self).run_within(args, budget, cancelled)
        }
    }

    /// A transport whose every command is held to `allowance` as well.
    pub struct Within<'a, T> {
        pub transport: T,
        pub allowance: Allowance<'a>,
    }

    impl<T: Transport> Transport for Within<'_, T> {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.run_within(args, Budget::ADAPTER_DEFAULT, None)
        }

        /// The allowance's budget is asked before anything else, so its error
        /// wins, as it did when the caller's budget was asked first.
        fn run_within(
            &self,
            args: &[&str],
            budget: Budget,
            cancelled: Option<&AtomicBool>,
        ) -> Result<String, String> {
            let budget = tighter(self.allowance.budget()?, budget);
            let cancelled = narrowest(cancelled, self.allowance.cancelled())?;
            self.transport.run_within(args, budget, cancelled)
        }
    }

    /// Each limit of the two that is reached first.
    pub fn tighter(one: Budget, other: Budget) -> Budget {
        Budget {
            timeout: one.timeout.min(other.timeout),
            max_output_bytes: one.max_output_bytes.min(other.max_output_bytes),
        }
    }

    /// The flag a command watches while it runs: the narrower connection's,
    /// or the wider one's when the narrower names none. A command watches
    /// one flag, so a wider flag that is already set refuses it here, before
    /// it starts.
    pub fn narrowest<'f>(
        narrower: Option<&'f AtomicBool>,
        wider: Option<&'f AtomicBool>,
    ) -> Result<Option<&'f AtomicBool>, String> {
        match (narrower, wider) {
            (Some(_), Some(wider)) if wider.load(Ordering::Relaxed) => Err(CANCELLED.to_owned()),
            (Some(narrower), _) => Ok(Some(narrower)),
            (None, wider) => Ok(wider),
        }
    }
}

use transport::{Transport, Within};

pub(crate) struct HerdrMux<T> {
    transport: T,
    endpoint: Endpoint,
}

impl<T: Transport> HerdrMux<T> {
    #[cfg(test)]
    fn new(transport: T, socket: &str) -> Self {
        Self {
            transport,
            endpoint: Endpoint {
                kind: MuxKind::Herdr,
                key: socket.to_owned(),
            },
        }
    }

    /// `MuxInput::deliver` minus the lock token. Herdr gates prompts itself:
    /// `agent prompt` refuses a blocked agent before it types, and types to a
    /// working one.
    pub(crate) fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        let args = match kind {
            InputKind::Prompt(request) => {
                vec![
                    "agent".to_owned(),
                    "prompt".to_owned(),
                    pane.as_str().to_owned(),
                    request.text.to_owned(),
                ]
            }
            InputKind::Line(line) => vec![
                "pane".to_owned(),
                "run".to_owned(),
                pane.as_str().to_owned(),
                line.to_owned(),
            ],
            InputKind::Keys { recipient, keys } => wire::keys_args(pane, recipient, keys),
        };
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        match self.transport.run(&args) {
            Ok(_) => InputOutcome::Delivered,
            Err(error) => wire::input_failure(error),
        }
    }
}

impl<T: Transport> MuxRead for HerdrMux<T> {
    fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Runs exactly the commands today's lookups run for each pair. See
    /// `herdr/tests.rs` for the pinned arguments.
    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        let transport = &self.transport;
        let spaces = match (scope, depth) {
            (Scope::Space(space), Depth::Spaces) => vec![wire::space_summary(transport, space)?],
            (Scope::Space(space), Depth::Panes) => {
                let mut view = wire::space_checkout(transport, space)?;
                view.panes = wire::space_panes(transport, space)?;
                vec![view]
            }
            (Scope::PanesOf(space), _) => vec![SpaceView {
                panes: wire::space_panes(transport, space)?,
                ..wire::bare_space(space.clone())
            }],
            (Scope::CheckoutOf(path), depth) => {
                let mut view = wire::checkout_owner(transport, path)?;
                if depth == Depth::Panes {
                    view.panes = wire::space_panes(transport, &view.id)?;
                }
                vec![view]
            }
            (Scope::All, Depth::Spaces) => wire::all_spaces(transport)?,
            (Scope::All, Depth::Panes) => wire::all_panes(transport)?,
        };
        Ok(Survey { spaces })
    }

    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        let args = wire::screen_args(pane, request);
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let text = wire::run(&self.transport, &args)?;
        Ok(Screen { text })
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        wire::process(&self.transport, pane)
    }

    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        wire::tab_label(&self.transport, tab)
    }

    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        wire::integration_root(&self.transport)
    }

    /// The transport's type is erased, so a budget within a budget is the
    /// same type rather than a new one for each level.
    fn within<'a>(&'a self, allowance: Allowance<'a>) -> Box<dyn MuxRead + 'a> {
        let transport: &dyn Transport = &self.transport;
        Box::new(HerdrMux {
            transport: Within {
                transport,
                allowance,
            },
            endpoint: self.endpoint.clone(),
        })
    }
}

impl<T: Transport> MuxAct for HerdrMux<T> {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        let mut reply = String::new();
        for command in wire::effect_commands(effect) {
            let args: Vec<_> = command.iter().map(String::as_str).collect();
            reply = wire::run(&self.transport, &args)?;
        }
        match effect {
            Effect::OpenPopup(_) => wire::parse(&reply).map(Applied::Opened),
            _ => Ok(Applied::Done),
        }
    }
}

impl<T: Transport> MuxInput for HerdrMux<T> {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        self.send(input.pane(), input.kind())
    }
}

/// Connects to a Herdr server by its socket, the stored `session_socket_id`.
pub struct HerdrConnector;

impl MuxConnector for HerdrConnector {
    fn connect(
        &self,
        socket: &str,
        budget: Budget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Arc<dyn Mux>, MuxError> {
        Ok(connect(Some(socket), budget, cancelled))
    }
}

/// The one way to make a Herdr connection. Each command runs the `herdr`
/// that `HERDR_BIN_PATH` names, within `budget`, until `cancelled` is set.
/// With no `socket`, each command inherits `HERDR_SOCKET_PATH`, or Herdr's
/// own default, and the endpoint's key is empty.
pub fn connect(socket: Option<&str>, budget: Budget, cancelled: Arc<AtomicBool>) -> Arc<dyn Mux> {
    Arc::new(HerdrMux {
        transport: Process::new(socket, budget, cancelled),
        endpoint: Endpoint {
            kind: MuxKind::Herdr,
            key: socket.unwrap_or_default().to_owned(),
        },
    })
}

/// The socket of the Herdr server a command reaches with no socket of its
/// own, as `status server` reports it, or `None` when it reports none.
pub fn server_socket() -> Option<String> {
    wire::server_socket(&Process::new(None, Budget::ADAPTER_DEFAULT, Arc::default()))
}

#[cfg(test)]
pub(crate) mod boundary;
#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
