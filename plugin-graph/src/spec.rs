//! The serializable description of a routing graph: what the editor shows and
//! what is saved. It names plugins by where they live; loading them is the
//! caller's business.

use serde::{Deserialize, Serialize};

use crate::error::GraphError;

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

    /// Whether the graph is the default one: the input's first channel
    /// goes straight to the output and nothing else is in the way. A graph
    /// of just those two nodes with the wire taken away is not, since it
    /// delivers silence.
    pub fn is_passthrough(&self) -> bool {
        let only_endpoints = self.nodes.iter().all(|node| match node.kind {
            NodeKind::Input | NodeKind::Output => true,
            NodeKind::Gain { .. } | NodeKind::Plugin(_) => false,
        });
        let of = |wanted: fn(&NodeKind) -> bool| {
            self.nodes
                .iter()
                .find(|node| wanted(&node.kind))
                .map(|node| node.id)
        };
        let (input, output) = (
            of(|kind| matches!(kind, NodeKind::Input)),
            of(|kind| matches!(kind, NodeKind::Output)),
        );
        match (only_endpoints, input, output, self.edges.as_slice()) {
            (true, Some(input), Some(output), [edge]) => {
                edge.from
                    == Port {
                        node: input,
                        port: 0,
                    }
                    && edge.to
                        == Port {
                            node: output,
                            port: 0,
                        }
            }
            (true, _, _, _) | (false, _, _, _) => false,
        }
    }

    /// Check what can be checked without knowing the channels of any
    /// plugin: that ids are unique, there is one input and one output,
    /// wires join nodes that exist, none is repeated, and they form no loop.
    pub fn validate(&self) -> Result<(), GraphError> {
        let mut seen = std::collections::HashSet::new();
        if let Some(duplicate) = self.nodes.iter().find(|node| !seen.insert(node.id)) {
            return Err(GraphError::DuplicateNode(duplicate.id));
        }
        let count = |wanted: fn(&NodeKind) -> bool| {
            self.nodes.iter().filter(|node| wanted(&node.kind)).count()
        };
        let (inputs, outputs) = (
            count(|kind| matches!(kind, NodeKind::Input)),
            count(|kind| matches!(kind, NodeKind::Output)),
        );
        if (inputs, outputs) != (1, 1) {
            return Err(GraphError::Endpoints { inputs, outputs });
        }
        let mut edges = std::collections::HashSet::new();
        for edge in &self.edges {
            for node in [edge.from.node, edge.to.node] {
                if !seen.contains(&node) {
                    return Err(GraphError::UnknownNode(node));
                }
            }
            if !edges.insert(*edge) {
                return Err(GraphError::DuplicateEdge(*edge));
            }
        }
        // Kahn's algorithm: a loop leaves nodes that never become ready.
        let mut waiting = self
            .nodes
            .iter()
            .map(|node| (node.id, 0usize))
            .collect::<std::collections::HashMap<_, _>>();
        self.edges
            .iter()
            .for_each(|edge| *waiting.entry(edge.to.node).or_default() += 1);
        let mut ready = waiting
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(node, _)| *node)
            .collect::<Vec<_>>();
        let mut ordered = 0;
        while let Some(node) = ready.pop() {
            ordered += 1;
            for edge in self.edges.iter().filter(|edge| edge.from.node == node) {
                let count = waiting.entry(edge.to.node).or_default();
                *count -= 1;
                if *count == 0 {
                    ready.push(edge.to.node);
                }
            }
        }
        match ordered == self.nodes.len() {
            true => Ok(()),
            false => Err(GraphError::Cycle),
        }
    }
}
