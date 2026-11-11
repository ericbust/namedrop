//! The Herdr adapter: the only place meant to hold Herdr arguments, JSON,
//! error codes, and plugin events. Until the callers move to the port, the
//! code it copies also stays where it was.

mod events;
mod wire;

pub use events::{decode_operation_input, decode_plugin_event};

use super::{
    Applied, Depth, Effect, Endpoint, InputKind, InputOutcome, MuxAct, MuxError, MuxInput, MuxKind,
    MuxRead, PaneId, PaneInput, PaneProcess, Retry, Scope, Screen, ScreenRequest, Survey, TabId,
};
use crate::{HerdrCommandExecutor, PromptFailureKind};
use std::path::PathBuf;

mod transport {
    use crate::PromptFailureKind;

    /// Runs one Herdr command and returns its stdout, or its error text.
    /// Public only inside this private module, so nothing outside the
    /// adapter can reach Herdr through it.
    pub trait Transport: Send + Sync {
        fn run(&self, args: &[&str]) -> Result<String, String>;

        fn prompt_failure(&self, error: &str) -> PromptFailureKind {
            super::wire::prompt_failure(error)
        }
    }
}

use transport::Transport;

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
    /// `agent prompt` refuses a busy agent before it types.
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
        let Err(error) = self.transport.run(&args) else {
            return InputOutcome::Delivered;
        };
        match self.transport.prompt_failure(&error) {
            PromptFailureKind::TemporaryPreInvocation => InputOutcome::Declined(Retry::OnEvent),
            PromptFailureKind::TemporaryPreInvocationWithoutEvent => {
                InputOutcome::Declined(Retry::AfterDelay)
            }
            PromptFailureKind::DefinitiveNonRetryable => InputOutcome::Refused(error),
            PromptFailureKind::AmbiguousInvocation => InputOutcome::Ambiguous(error),
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
}

impl<T: Transport> MuxAct for HerdrMux<T> {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        for command in wire::effect_commands(effect, &self.endpoint.key) {
            let args: Vec<_> = command.iter().map(String::as_str).collect();
            wire::run(&self.transport, &args)?;
        }
        Ok(Applied::Done)
    }
}

impl<T: Transport> MuxInput for HerdrMux<T> {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        self.send(input.pane(), input.kind())
    }
}

/// Temporary: every legacy executor, `ProcessHerdr` and the test fakes alike,
/// is a `Mux` through this transport, so callers and fakes can move to the
/// port one commit at a time. Deleted with `HerdrCommandExecutor`.
struct Legacy<'a, E>(&'a E);

impl<E: HerdrCommandExecutor + Sync> Transport for Legacy<'_, E> {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.0.execute(args).map(|output| output.stdout)
    }

    /// The executor's own classification, so a bridged fake keeps its answers.
    fn prompt_failure(&self, error: &str) -> PromptFailureKind {
        self.0.classify_prompt_failure(error)
    }
}

/// A legacy executor does not know its socket. Callers keep reading
/// `LifecycleRuntime::session_socket_id` until the bridge is deleted.
static LEGACY_ENDPOINT: Endpoint = Endpoint {
    kind: MuxKind::Herdr,
    key: String::new(),
};

fn bridge<E: HerdrCommandExecutor + Sync>(executor: &E) -> HerdrMux<Legacy<'_, E>> {
    HerdrMux {
        transport: Legacy(executor),
        endpoint: LEGACY_ENDPOINT.clone(),
    }
}

impl<E: HerdrCommandExecutor + Send + Sync> MuxRead for E {
    fn endpoint(&self) -> &Endpoint {
        &LEGACY_ENDPOINT
    }

    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        bridge(self).survey(scope, depth)
    }

    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        bridge(self).screen(pane, request)
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        bridge(self).process(pane)
    }

    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        bridge(self).tab_label(tab)
    }

    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        bridge(self).integration_root()
    }
}

impl<E: HerdrCommandExecutor + Send + Sync> MuxAct for E {
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        bridge(self).apply(effect)
    }
}

impl<E: HerdrCommandExecutor + Send + Sync> MuxInput for E {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        bridge(self).deliver(input)
    }
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
