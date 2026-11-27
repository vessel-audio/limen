//! What the audio routing requests carry: the plugins a machine has, the
//! routing graph with the state of its nodes, and a test recording.

use plugin_graph::{GraphSpec, NodeId};
use serde::{Deserialize, Serialize};

/// A plugin class found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// The plugin's bundle on disk.
    pub path: String,
    /// The class inside the bundle, as lowercase hexadecimal.
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    /// The plugin's own subcategories, such as `Fx|Dynamics`.
    pub category: String,
    /// Whether it processes audio rather than making it.
    pub effect: bool,
}

/// What a node of the saved graph looks like once its plugin is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node: NodeId,
    pub main_inputs: usize,
    pub sidechain_inputs: usize,
    pub outputs: usize,
    /// Why the node does nothing, when it does not: a plugin that did not
    /// load, or whose worker failed.
    pub problem: Option<String>,
}

/// The saved routing graph and the state of each of its nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioGraphView {
    pub graph: GraphSpec,
    pub nodes: Vec<NodeStatus>,
}

/// A test recording, before and after the graph, as 16 kHz mono WAV files
/// that the daemon keeps until the next test or until it exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestFiles {
    pub dry: String,
    pub wet: String,
}
