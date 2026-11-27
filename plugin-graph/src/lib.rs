//! A block-based audio routing graph for hosting plugins.
//!
//! A [`GraphSpec`] describes nodes (an input, an output, gains, plugins) and
//! the wires between their channels. [`CompiledGraph::compile`] checks it,
//! orders it, and lines up parallel paths of different latency with delay
//! lines; the result processes fixed blocks without allocating, and passes a
//! failing node's main input through rather than failing the signal.
//!
//! The crate depends on no application, audio device, or async runtime.

mod compile;
mod delay;
mod error;
mod processor;
mod resample;
mod spec;

#[cfg(test)]
mod tests;

pub use compile::{CompiledGraph, Failure, GraphConfig};
pub use error::GraphError;
pub use processor::{PortLayout, ProcessError, Processor};
pub use resample::Resampler;
pub use spec::{Edge, GraphSpec, NodeId, NodeKind, NodeSpec, PluginRef, Port, Sidechain};

#[cfg(feature = "vst3")]
pub mod vst3;
