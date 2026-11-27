use std::collections::HashMap;

use crate::delay::DelayLine;
use crate::{
    CompiledGraph, Edge, Failure, GraphConfig, GraphError, GraphSpec, NodeId, NodeKind, NodeSpec,
    PluginRef, Port, PortLayout, ProcessError, Processor, Sidechain,
};

const BLOCK: usize = 4;

/// A mono processor that delays by `frames`, and fails from block
/// `fail_at` on when it has one.
struct Delay {
    frames: usize,
    line: DelayLine,
    block: usize,
    fail_at: Option<usize>,
}

impl Delay {
    fn of(frames: usize) -> Box<dyn Processor> {
        Box::new(Self {
            frames,
            line: DelayLine::new(frames),
            block: 0,
            fail_at: None,
        })
    }

    fn failing_from(frames: usize, block: usize) -> Box<dyn Processor> {
        Box::new(Self {
            frames,
            line: DelayLine::new(frames),
            block: 0,
            fail_at: Some(block),
        })
    }
}

impl Processor for Delay {
    fn layout(&self) -> PortLayout {
        PortLayout {
            main_inputs: 1,
            sidechain_inputs: 0,
            outputs: 1,
        }
    }

    fn latency(&self) -> usize {
        self.frames
    }

    fn process(&mut self, _: usize, input: &[f32], output: &mut [f32]) -> Result<(), ProcessError> {
        self.block += 1;
        match self.fail_at {
            Some(at) if self.block > at => Err(ProcessError("the worker died".to_string())),
            Some(_) | None => {
                output.fill(0.);
                self.line.add_into(input, output);
                Ok(())
            }
        }
    }
}

/// Multiplies its main input by its side-chain.
struct Keyed;

impl Processor for Keyed {
    fn layout(&self) -> PortLayout {
        PortLayout {
            main_inputs: 1,
            sidechain_inputs: 1,
            outputs: 1,
        }
    }

    fn latency(&self) -> usize {
        0
    }

    fn process(
        &mut self,
        frames: usize,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<(), ProcessError> {
        let (main, key) = input.split_at(frames);
        main.iter()
            .zip(key)
            .zip(output)
            .for_each(|((main, key), out)| *out = main * key);
        Ok(())
    }
}

fn plugin(enabled: bool) -> NodeKind {
    NodeKind::Plugin(PluginRef {
        path: "/plugins/x.vst3".to_string(),
        class_id: "00ff".to_string(),
        name: "X".to_string(),
        enabled,
        sidechain: Sidechain::default(),
    })
}

/// The input is node 0 and the output node 1; `inner` become nodes 2.. . A
/// wire is `(from node, from port, to node, to port)`.
fn spec(inner: Vec<NodeKind>, wires: &[(NodeId, u16, NodeId, u16)]) -> GraphSpec {
    GraphSpec {
        nodes: [NodeKind::Input, NodeKind::Output]
            .into_iter()
            .chain(inner)
            .enumerate()
            .map(|(id, kind)| NodeSpec {
                id: id as NodeId,
                kind,
                x: 0.,
                y: 0.,
            })
            .collect(),
        edges: wires
            .iter()
            .map(|(from, out, to, port)| Edge {
                from: Port {
                    node: *from,
                    port: *out,
                },
                to: Port {
                    node: *to,
                    port: *port,
                },
            })
            .collect(),
    }
}

fn mono() -> GraphConfig {
    GraphConfig {
        input_channels: 1,
        output_channels: 1,
        block: BLOCK,
    }
}

fn compile(
    spec: &GraphSpec,
    processors: Vec<(NodeId, Box<dyn Processor>)>,
) -> Result<CompiledGraph, GraphError> {
    CompiledGraph::compile(
        spec,
        mono(),
        processors.into_iter().collect::<HashMap<_, _>>(),
    )
}

/// Feed a mono signal through block by block.
fn run(graph: &mut CompiledGraph, signal: &[f32]) -> Vec<f32> {
    signal
        .chunks(BLOCK)
        .flat_map(|chunk| {
            let mut output = vec![0.; BLOCK];
            graph.process(chunk, &mut output).unwrap();
            output
        })
        .collect()
}

const RAMP: [f32; 12] = [1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12.];

#[test]
fn the_default_graph_passes_the_signal_through_exactly() {
    let mut graph = compile(&GraphSpec::passthrough(), vec![]).unwrap();
    assert_eq!(
        (run(&mut graph, &RAMP), graph.latency()),
        (RAMP.to_vec(), 0)
    );
}

#[test]
fn wires_into_one_input_are_summed_and_one_output_feeds_many() {
    let two_unity_gains = spec(
        vec![NodeKind::Gain { db: 0. }, NodeKind::Gain { db: 0. }],
        &[(0, 0, 2, 0), (0, 0, 3, 0), (2, 0, 1, 0), (3, 0, 1, 0)],
    );
    let mut graph = compile(&two_unity_gains, vec![]).unwrap();
    assert_eq!(
        run(&mut graph, &RAMP),
        RAMP.iter().map(|sample| sample * 2.).collect::<Vec<_>>()
    );
}

#[test]
fn a_gain_scales_by_its_decibels() {
    let quieter = spec(
        vec![NodeKind::Gain { db: -20. }],
        &[(0, 0, 2, 0), (2, 0, 1, 0)],
    );
    let mut graph = compile(&quieter, vec![]).unwrap();
    let out = run(&mut graph, &RAMP);
    let expected = RAMP.iter().map(|sample| sample * 0.1).collect::<Vec<_>>();
    assert!(
        out.iter().zip(&expected).all(|(a, b)| (a - b).abs() < 1e-5),
        "{out:?}"
    );
}

#[test]
fn parallel_paths_of_different_latency_are_lined_up() {
    // The delayed path and the direct one both reach the output, which hears
    // each three frames late.
    let parallel = spec(
        vec![plugin(true)],
        &[(0, 0, 2, 0), (2, 0, 1, 0), (0, 0, 1, 0)],
    );
    let mut graph = compile(&parallel, vec![(2, Delay::of(3))]).unwrap();
    let delayed = [0., 0., 0., 1., 2., 3., 4., 5., 6., 7., 8., 9.];
    assert_eq!(
        (run(&mut graph, &RAMP), graph.latency()),
        (delayed.iter().map(|sample| sample * 2.).collect(), 3)
    );
}

#[test]
fn a_failing_plugin_is_passed_through_on_time_and_reported() {
    let chain = spec(vec![plugin(true)], &[(0, 0, 2, 0), (2, 0, 1, 0)]);
    let mut graph = compile(&chain, vec![(2, Delay::failing_from(2, 1))]).unwrap();
    // The first block is the plugin's own; from the second, the bypass
    // delays by the same two frames, starting empty.
    assert_eq!(
        (run(&mut graph, &RAMP), graph.failures()),
        (
            vec![0., 0., 1., 2., 0., 0., 5., 6., 7., 8., 9., 10.],
            vec![Failure {
                node: 2,
                reason: "the worker died".to_string()
            }]
        )
    );
}

#[test]
fn a_disabled_plugin_passes_through_without_delay() {
    let chain = spec(vec![plugin(false)], &[(0, 0, 2, 0), (2, 0, 1, 0)]);
    let mut graph = compile(&chain, vec![(2, Delay::of(3))]).unwrap();
    assert_eq!(
        (run(&mut graph, &RAMP), graph.latency(), graph.failures()),
        (RAMP.to_vec(), 0, vec![])
    );
}

#[test]
fn a_sidechain_is_routed_to_its_own_port() {
    let keyed = spec(
        vec![plugin(true)],
        &[(0, 0, 2, 0), (0, 1, 2, 1), (2, 0, 1, 0)],
    );
    let config = GraphConfig {
        input_channels: 2,
        ..mono()
    };
    let mut graph = CompiledGraph::compile(
        &keyed,
        config,
        HashMap::from([(2, Box::new(Keyed) as Box<dyn Processor>)]),
    )
    .unwrap();
    let mut output = vec![0.; BLOCK];
    graph
        .process(&[1., 2., 3., 4., 0., 1., 2., 3.], &mut output)
        .unwrap();
    assert_eq!(output, vec![0., 2., 6., 12.]);
}

#[test]
fn a_graph_that_cannot_be_built_says_why() {
    let gain = || NodeKind::Gain { db: 0. };
    let no_output = GraphSpec {
        nodes: spec(vec![], &[]).nodes[..1].to_vec(),
        edges: vec![],
    };
    let build = |spec: &GraphSpec| compile(spec, vec![]).err();
    assert_eq!(
        [
            build(&spec(
                vec![gain(), gain()],
                &[(0, 0, 2, 0), (2, 0, 3, 0), (3, 0, 2, 0), (3, 0, 1, 0)],
            )),
            build(&spec(vec![gain()], &[(0, 3, 2, 0)])),
            build(&spec(vec![gain()], &[(0, 0, 2, 1)])),
            build(&spec(vec![gain()], &[(0, 0, 9, 0)])),
            build(&spec(vec![gain()], &[(0, 0, 2, 0), (0, 0, 2, 0)])),
            build(&spec(vec![plugin(true)], &[])),
            build(&no_output),
        ],
        [
            Some(GraphError::Cycle),
            Some(GraphError::UnknownOutput { node: 0, port: 3 }),
            Some(GraphError::UnknownInput { node: 2, port: 1 }),
            Some(GraphError::UnknownNode(9)),
            Some(GraphError::DuplicateEdge(Edge {
                from: Port { node: 0, port: 0 },
                to: Port { node: 2, port: 0 },
            })),
            Some(GraphError::MissingProcessor(2)),
            Some(GraphError::Endpoints {
                inputs: 1,
                outputs: 0
            }),
        ]
    );
}

#[test]
fn buffers_of_the_wrong_size_are_refused() {
    let mut graph = compile(&GraphSpec::passthrough(), vec![]).unwrap();
    assert_eq!(
        graph.process(&[0.; 3], &mut [0.; BLOCK]),
        Err(GraphError::BufferSize {
            expected: BLOCK,
            actual: 3
        })
    );
}

#[test]
fn a_graph_survives_saving_as_toml() {
    let mut saved = spec(
        vec![NodeKind::Gain { db: -3. }, plugin(false)],
        &[(0, 0, 2, 0), (2, 0, 3, 0), (3, 0, 1, 0)],
    );
    saved.nodes[2].x = 120.;
    saved.nodes[3].y = -40.;
    let text = toml::to_string(&saved).unwrap();
    assert_eq!(toml::from_str::<GraphSpec>(&text).unwrap(), saved, "{text}");
}
