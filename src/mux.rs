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
