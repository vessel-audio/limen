//! Why a graph cannot be built or run.

use crate::spec::{Edge, NodeId};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GraphError {
    #[error("the block size must be at least one frame")]
    ZeroBlock,
    #[error("node {0} appears twice")]
    DuplicateNode(NodeId),
    #[error("a graph needs one input and one output node, not {inputs} and {outputs}")]
    Endpoints { inputs: usize, outputs: usize },
    #[error("plugin node {0} has no processor")]
    MissingProcessor(NodeId),
    #[error("a wire names node {0}, which is not in the graph")]
    UnknownNode(NodeId),
    #[error("node {node} has no output port {port}")]
    UnknownOutput { node: NodeId, port: u16 },
    #[error("node {node} has no input port {port}")]
    UnknownInput { node: NodeId, port: u16 },
    #[error("the wire {0:?} appears twice")]
    DuplicateEdge(Edge),
    #[error("the wires form a loop")]
    Cycle,
    #[error("expected {expected} samples, got {actual}")]
    BufferSize { expected: usize, actual: usize },
}
