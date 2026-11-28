//! What the audio routing requests carry: the plugin library, the routing
//! graph with the state of its nodes, and a test recording.

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

/// A plugin the user took out of the library. A scan leaves it out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedPlugin {
    pub path: String,
    pub class_id: String,
}

/// A bundle the last scan could not read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanFailure {
    pub path: String,
    pub error: String,
}

/// The plugins the user has set up for routing: where to look for them, the
/// bundles picked by hand, and what was found. Nothing in it is read from
/// the disk again until a scan is asked for.
///
/// Whatever a hand-written file leaves out is the default: the usual VST3
/// folders, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginLibrary {
    /// Folders a scan looks in, below which plugin bundles may be nested.
    pub folders: Vec<String>,
    /// Bundles added one by one, which a scan reads as well.
    pub added: Vec<String>,
    /// Plugins taken out, which a scan does not bring back.
    pub removed: Vec<RemovedPlugin>,
    /// The plugins the user can put in the graph.
    pub plugins: Vec<PluginEntry>,
    /// Bundles the last scan could not read.
    pub failures: Vec<ScanFailure>,
}

impl Default for PluginLibrary {
    fn default() -> Self {
        Self {
            folders: plugin_graph::vst3::default_folders()
                .iter()
                .map(|folder| folder.display().to_string())
                .collect(),
            added: Vec::new(),
            removed: Vec::new(),
            plugins: Vec::new(),
            failures: Vec::new(),
        }
    }
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
