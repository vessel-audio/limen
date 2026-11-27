//! The serializable description of a routing graph: what the editor shows and
//! what is saved. It names plugins by where they live; loading them is the
//! caller's business.

use serde::{Deserialize, Serialize};

/// A node's stable identity, which saved wires, plugin state, and editor
/// windows keep across edits.
pub type NodeId = u32;

/// One channel of a node: an output when a wire starts at it, an input when
/// a wire ends at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Port {
    pub node: NodeId,
    pub port: u16,
}

/// A wire from an output channel to an input channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: Port,
    pub to: Port,
}

/// Which plugin a node runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    /// The plugin bundle on disk.
    pub path: String,
    /// The class inside the bundle, as lowercase hexadecimal.
    pub class_id: String,
    /// What the editor shows.
    pub name: String,
    /// A disabled plugin passes its main input straight through.
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Whether the plugin's side-chain input is wired up. An active
    /// side-chain nobody feeds is silent, which stops a compressor keyed
    /// from it from ever acting, so it is off unless asked for.
    #[serde(default)]
    pub sidechain: Sidechain,
}

/// Whether a plugin's side-chain bus is active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sidechain {
    #[default]
    Off,
    On,
}

fn enabled() -> bool {
    true
}

/// What a node does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    /// The captured signal, one output port per captured channel.
    Input,
    /// The signal that leaves the graph.
    Output,
    /// A mono level change.
    Gain { db: f32 },
    /// A hosted plugin.
    Plugin(PluginRef),
}

/// A node and where the editor draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSpec {
    pub id: NodeId,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
}

/// A routing graph.
///
/// Several wires into one input channel are summed, and one output channel
/// may feed any number of inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSpec {
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl GraphSpec {
    /// The graph before anything is added: the first input channel goes
    /// straight to the output.
    pub fn passthrough() -> Self {
        let (input, output) = (0, 1);
        Self {
            nodes: vec![
                NodeSpec {
                    id: input,
                    kind: NodeKind::Input,
                    x: 0.,
                    y: 0.,
                },
                NodeSpec {
                    id: output,
                    kind: NodeKind::Output,
                    x: 400.,
                    y: 0.,
                },
            ],
            edges: vec![Edge {
                from: Port {
                    node: input,
                    port: 0,
                },
                to: Port {
                    node: output,
                    port: 0,
                },
            }],
        }
    }

    /// Whether nothing in the graph changes the signal: it has no node but
    /// the input and the output.
    pub fn is_passthrough(&self) -> bool {
        self.nodes.iter().all(|node| match node.kind {
            NodeKind::Input | NodeKind::Output => true,
            NodeKind::Gain { .. } | NodeKind::Plugin(_) => false,
        })
    }
}
