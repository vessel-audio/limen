//! A graph ready to run: nodes in processing order, wires carrying the
//! delays that line up their paths, and every buffer allocated.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::delay::DelayLine;
use crate::error::GraphError;
use crate::processor::{Gain, PortLayout, Processor};
use crate::spec::{GraphSpec, NodeId, NodeKind};

/// The signal a graph is compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphConfig {
    /// Channels of the signal entering the graph.
    pub input_channels: usize,
    /// Channels of the signal leaving it.
    pub output_channels: usize,
    /// Frames per block.
    pub block: usize,
}

/// A node that stopped processing and is passed through instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub node: NodeId,
    pub reason: String,
}

#[derive(Clone, Copy)]
enum Role {
    Input,
    Output,
    Process,
}

/// A wire into a step: which earlier step and channel it takes, which input
/// channel it feeds, and the delay that aligns it with the node's slowest
/// wire.
struct Wire {
    source: usize,
    source_port: usize,
    port: usize,
    delay: DelayLine,
}

struct Step {
    id: NodeId,
    role: Role,
    /// `None` for a disabled plugin.
    processor: Option<Box<dyn Processor>>,
    input: Vec<f32>,
    wires: Vec<Wire>,
    /// One delay per channel a bypassed step passes on, as long as the
    /// processor's latency, so bypassing keeps the paths aligned.
    bypass: Vec<DelayLine>,
    failure: Option<String>,
}

impl Step {
    fn run(&mut self, output: &mut [f32], frames: usize) {
        let running = self.failure.is_none();
        match self
            .processor
            .as_mut()
            .filter(|_| running)
            .map(|processor| processor.process(frames, &self.input, output))
        {
            Some(Ok(())) => {
                // Keep what a bypass would pass on, so that if this processor
                // fails the signal carries on from where it was.
                self.bypass
                    .iter_mut()
                    .zip(self.input.chunks_exact(frames))
                    .for_each(|(delay, input)| delay.advance(input));
                return;
            }
            Some(Err(error)) => self.failure = Some(error.0),
            None => {}
        }
        output.fill(0.);
        self.bypass
            .iter_mut()
            .zip(
                self.input
                    .chunks_exact(frames)
                    .zip(output.chunks_exact_mut(frames)),
            )
            .for_each(|(delay, (input, output))| delay.add_into(input, output));
    }
}

/// A routing graph compiled for one block size. It allocates nothing while
/// processing.
pub struct CompiledGraph {
    config: GraphConfig,
    steps: Vec<Step>,
    outputs: Vec<Vec<f32>>,
    latency: usize,
}

/// A node as the compiler sees it.
struct Resolved {
    id: NodeId,
    role: Role,
    layout: PortLayout,
    processor: Option<Box<dyn Processor>>,
    latency: usize,
}

impl Resolved {
    /// A node with no processor and no latency.
    fn endpoint(id: NodeId, role: Role, layout: PortLayout) -> Self {
        Self {
            id,
            role,
            layout,
            processor: None,
            latency: 0,
        }
    }
}

impl CompiledGraph {
    /// Compile `spec`. `processors` holds the processor of each plugin node;
    /// a disabled plugin's is only asked for its channels.
    pub fn compile(
        spec: &GraphSpec,
        config: GraphConfig,
        mut processors: HashMap<NodeId, Box<dyn Processor>>,
    ) -> Result<Self, GraphError> {
        if config.block == 0 {
            return Err(GraphError::ZeroBlock);
        }
        let mut seen = HashSet::new();
        if let Some(duplicate) = spec.nodes.iter().find(|node| !seen.insert(node.id)) {
            return Err(GraphError::DuplicateNode(duplicate.id));
        }
        let count = |wanted: fn(&NodeKind) -> bool| {
            spec.nodes.iter().filter(|node| wanted(&node.kind)).count()
        };
        let (inputs, outputs) = (
            count(|kind| matches!(kind, NodeKind::Input)),
            count(|kind| matches!(kind, NodeKind::Output)),
        );
        if (inputs, outputs) != (1, 1) {
            return Err(GraphError::Endpoints { inputs, outputs });
        }

        let mut nodes = spec
            .nodes
            .iter()
            .map(|node| {
                let (role, processor, enabled): (_, Box<dyn Processor>, _) = match &node.kind {
                    NodeKind::Input => {
                        return Ok(Resolved::endpoint(
                            node.id,
                            Role::Input,
                            PortLayout {
                                main_inputs: 0,
                                sidechain_inputs: 0,
                                outputs: config.input_channels,
                            },
                        ));
                    }
                    NodeKind::Output => {
                        return Ok(Resolved::endpoint(
                            node.id,
                            Role::Output,
                            PortLayout {
                                main_inputs: config.output_channels,
                                sidechain_inputs: 0,
                                outputs: 0,
                            },
                        ));
                    }
                    NodeKind::Gain { db } => (Role::Process, Box::new(Gain::from_db(*db)), true),
                    NodeKind::Plugin(plugin) => (
                        Role::Process,
                        processors
                            .remove(&node.id)
                            .ok_or(GraphError::MissingProcessor(node.id))?,
                        plugin.enabled,
                    ),
                };
                let (layout, latency) = (processor.layout(), processor.latency());
                Ok(match enabled {
                    true => Resolved {
                        id: node.id,
                        role,
                        layout,
                        processor: Some(processor),
                        latency,
                    },
                    false => Resolved {
                        processor: None,
                        latency: 0,
                        ..Resolved::endpoint(node.id, role, layout)
                    },
                })
            })
            .collect::<Result<Vec<_>, GraphError>>()?;

        let index = spec
            .nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id, position))
            .collect::<HashMap<_, _>>();
        let mut edges = HashSet::new();
        for edge in &spec.edges {
            let from = *index
                .get(&edge.from.node)
                .ok_or(GraphError::UnknownNode(edge.from.node))?;
            let to = *index
                .get(&edge.to.node)
                .ok_or(GraphError::UnknownNode(edge.to.node))?;
            if usize::from(edge.from.port) >= nodes[from].layout.outputs {
                return Err(GraphError::UnknownOutput {
                    node: edge.from.node,
                    port: edge.from.port,
                });
            }
            if usize::from(edge.to.port) >= nodes[to].layout.inputs() {
                return Err(GraphError::UnknownInput {
                    node: edge.to.node,
                    port: edge.to.port,
                });
            }
            if !edges.insert(*edge) {
                return Err(GraphError::DuplicateEdge(*edge));
            }
        }

        // Kahn's algorithm, taking nodes in the order they were listed.
        let mut waiting = vec![0usize; nodes.len()];
        spec.edges
            .iter()
            .for_each(|edge| waiting[index[&edge.to.node]] += 1);
        let mut ready = (0..nodes.len())
            .filter(|node| waiting[*node] == 0)
            .collect::<VecDeque<_>>();
        let mut order = Vec::with_capacity(nodes.len());
        while let Some(node) = ready.pop_front() {
            order.push(node);
            for edge in spec
                .edges
                .iter()
                .filter(|edge| index[&edge.from.node] == node)
            {
                let target = index[&edge.to.node];
                waiting[target] -= 1;
                if waiting[target] == 0 {
                    ready.push_back(target);
                }
            }
        }
        if order.len() != nodes.len() {
            return Err(GraphError::Cycle);
        }
        let position = order
            .iter()
            .enumerate()
            .map(|(step, node)| (*node, step))
            .collect::<HashMap<_, _>>();

        let block = config.block;
        let mut steps = Vec::with_capacity(nodes.len());
        let mut outputs = Vec::with_capacity(nodes.len());
        // Frames by which each finished step's output trails the input.
        let mut trailing = Vec::with_capacity(nodes.len());
        let mut graph_latency = 0;
        for node in &order {
            let resolved = &mut nodes[*node];
            let incoming = spec
                .edges
                .iter()
                .filter(|edge| index[&edge.to.node] == *node)
                .map(|edge| (position[&index[&edge.from.node]], edge))
                .collect::<Vec<_>>();
            let arrival = incoming
                .iter()
                .map(|(source, _)| trailing[*source])
                .max()
                .unwrap_or(0);
            let wires = incoming
                .iter()
                .map(|(source, edge)| Wire {
                    source: *source,
                    source_port: usize::from(edge.from.port),
                    port: usize::from(edge.to.port),
                    delay: DelayLine::new(arrival - trailing[*source]),
                })
                .collect();
            trailing.push(arrival + resolved.latency);
            if matches!(resolved.role, Role::Output) {
                graph_latency = arrival;
            }
            let layout = resolved.layout;
            steps.push(Step {
                id: resolved.id,
                role: resolved.role,
                processor: resolved.processor.take(),
                input: vec![0.; layout.inputs() * block],
                wires,
                bypass: (0..layout.main_inputs.min(layout.outputs))
                    .map(|_| DelayLine::new(resolved.latency))
                    .collect(),
                failure: None,
            });
            outputs.push(vec![0.; layout.outputs * block]);
        }
        Ok(Self {
            config,
            steps,
            outputs,
            latency: graph_latency,
        })
    }

    /// Frames by which the output trails the input.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// The nodes that failed and are passed through since.
    pub fn failures(&self) -> Vec<Failure> {
        self.steps
            .iter()
            .filter_map(|step| {
                step.failure.as_ref().map(|reason| Failure {
                    node: step.id,
                    reason: reason.clone(),
                })
            })
            .collect()
    }

    /// Process one block. Both buffers are planar, `input_channels` and
    /// `output_channels` planes of `block` frames.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<(), GraphError> {
        let GraphConfig {
            input_channels,
            output_channels,
            block,
        } = self.config;
        for (expected, actual) in [
            (input_channels * block, input.len()),
            (output_channels * block, output.len()),
        ] {
            if expected != actual {
                return Err(GraphError::BufferSize { expected, actual });
            }
        }
        for index in 0..self.steps.len() {
            let (before, rest) = self.outputs.split_at_mut(index);
            let own = &mut rest[0];
            let step = &mut self.steps[index];
            step.input.fill(0.);
            for wire in &mut step.wires {
                wire.delay.add_into(
                    &before[wire.source][wire.source_port * block..][..block],
                    &mut step.input[wire.port * block..][..block],
                );
            }
            match step.role {
                Role::Input => own.copy_from_slice(input),
                Role::Output => output.copy_from_slice(&step.input),
                Role::Process => step.run(own, block),
            }
        }
        Ok(())
    }
}
