//! The terminal multiplexer port: what Namedrop asks of Herdr or Rex, in
//! panes, spaces, and prompts rather than command arguments.

mod effect;
mod event;
#[doc(hidden)]
pub mod fake;
pub mod gate;
mod input;
mod model;
mod ports;

pub use effect::*;
pub use event::*;
pub use input::*;
pub use model::*;
pub use ports::*;
