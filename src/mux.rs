//! The terminal multiplexer port: what Namedrop asks of Herdr or Rex, in
//! panes, spaces, and prompts rather than command arguments.

mod effect;
mod event;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod fake;
pub mod gate;
pub mod herdr;
mod input;
mod model;
mod ports;
mod processes;
pub(crate) mod rex;

pub use effect::*;
pub use event::*;
pub use input::*;
pub use model::*;
pub use ports::*;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

/// The one way to reach the multiplexer an endpoint names. Each command keeps
/// to `budget` and stops once `cancelled` is set. A Rex connection reads its
/// session bindings from the state directory `state`.
pub fn connect(
    state: &Path,
    endpoint: &Endpoint,
    budget: Budget,
    cancelled: Arc<AtomicBool>,
) -> Result<Arc<dyn Mux>, MuxError> {
    match endpoint.kind {
        MuxKind::Herdr => Ok(herdr::connect(Some(&endpoint.key), budget, cancelled)),
        MuxKind::Rex => rex::connect(state, endpoint, budget, cancelled),
    }
}

/// `connect` for code that is handed a `MuxConnector`, such as the dashboard,
/// for the state directory it serves.
pub struct Connector {
    state: PathBuf,
}

impl Connector {
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.to_owned(),
        }
    }
}

impl MuxConnector for Connector {
    fn connect(
        &self,
        endpoint: &str,
        budget: Budget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Arc<dyn Mux>, MuxError> {
        connect(&self.state, &Endpoint::parse(endpoint), budget, cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_endpoint_connects_to_the_multiplexer_its_kind_names() {
        let state = Path::new("/nonexistent-state");
        let connector = Connector::new(state);
        for endpoint in ["/tmp/h.sock", "rex:/tmp/r.sock"] {
            for connected in [
                connect(
                    state,
                    &Endpoint::parse(endpoint),
                    Budget::ADAPTER_DEFAULT,
                    Arc::default(),
                ),
                connector.connect(endpoint, Budget::ADAPTER_DEFAULT, Arc::default()),
            ] {
                assert_eq!(
                    connected.map(|mux| mux.endpoint().clone()).ok(),
                    Some(Endpoint::parse(endpoint))
                );
            }
        }
        assert_eq!(
            connect(
                state,
                &Endpoint::parse("rex:r.sock"),
                Budget::ADAPTER_DEFAULT,
                Arc::default()
            )
            .map(|_| ()),
            Err(MuxError::Unavailable(
                "rex:r.sock names no Rex server's Unix socket".into()
            ))
        );
    }

    /// Nothing is dialed until a port call, and a call to a socket nothing
    /// serves fails without a Herdr command.
    #[test]
    fn a_rex_connection_reaches_rex_and_never_herdr() {
        let state = Path::new("/nonexistent-state");
        let rex = connect(
            state,
            &Endpoint::parse("rex:/nonexistent-namedrop-rex.sock"),
            Budget::ADAPTER_DEFAULT,
            Arc::default(),
        )
        .unwrap();
        assert!(matches!(
            rex.survey(Scope::All, Depth::Spaces),
            Err(MuxError::Unavailable(why)) if why.starts_with("could not reach Rex at /nonexistent-namedrop-rex.sock")
        ));
    }
}
