//! The Audio routing page: a canvas of nodes and wires for routing the
//! microphone through plugins, a list of its connections, and a test
//! recording to hear the result.

mod canvas;
mod geometry;
mod page;
mod state;

pub(crate) use page::{Playing, follow, page};
pub use state::RoutingState;
