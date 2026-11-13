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
// Tests are the Rex adapter's only callers until `mux::connect` reaches it,
// so the library build leaves it out.
#[cfg(test)]
mod rex;

pub use effect::*;
pub use event::*;
pub use input::*;
pub use model::*;
pub use ports::*;

use std::sync::{Arc, atomic::AtomicBool};

/// The one way to reach the multiplexer an endpoint names. Each command keeps
/// to `budget` and stops once `cancelled` is set.
pub fn connect(
    endpoint: &Endpoint,
    budget: Budget,
    cancelled: Arc<AtomicBool>,
) -> Result<Arc<dyn Mux>, MuxError> {
    match endpoint.kind {
        MuxKind::Herdr => Ok(herdr::connect(Some(&endpoint.key), budget, cancelled)),
        MuxKind::Rex => Err(MuxError::Unavailable(
            "the Rex adapter is not built yet".into(),
        )),
    }
}

/// `connect` for code that is handed a `MuxConnector`, such as the dashboard.
pub struct Connector;

impl MuxConnector for Connector {
    fn connect(
        &self,
        endpoint: &str,
        budget: Budget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Arc<dyn Mux>, MuxError> {
        connect(&Endpoint::parse(endpoint), budget, cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_herdr_endpoint_connects_to_herdr_and_a_rex_endpoint_is_refused() {
        for connected in [
            connect(
                &Endpoint::parse("/tmp/h.sock"),
                Budget::ADAPTER_DEFAULT,
                Arc::default(),
            ),
            Connector.connect("/tmp/h.sock", Budget::ADAPTER_DEFAULT, Arc::default()),
        ] {
            assert_eq!(
                connected.map(|mux| mux.endpoint().clone()).ok(),
                Some(Endpoint::parse("/tmp/h.sock"))
            );
        }
        for refused in [
            connect(
                &Endpoint::parse("rex:/tmp/r.sock"),
                Budget::ADAPTER_DEFAULT,
                Arc::default(),
            ),
            Connector.connect("rex:/tmp/r.sock", Budget::ADAPTER_DEFAULT, Arc::default()),
        ] {
            assert_eq!(
                refused.map(|_| ()),
                Err(MuxError::Unavailable(
                    "the Rex adapter is not built yet".into()
                ))
            );
        }
    }
}
