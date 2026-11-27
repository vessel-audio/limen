//! Runs a real plugin through the graph. Ignored unless a worker and a plugin
//! are given:
//!
//! ```text
//! PLUGIN_GRAPH_TEST_WORKER=…/vst-audio-worker \
//! PLUGIN_GRAPH_TEST_VST3=…/Some\ Effect.vst3 \
//!     cargo test -p plugin-graph --features vst3 -- --ignored
//! ```
#![cfg(feature = "vst3")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use plugin_graph::vst3::{Channels, LoadOptions, Sandbox, Vst3Host};
use plugin_graph::{
    CompiledGraph, Edge, GraphConfig, GraphSpec, NodeKind, NodeSpec, PluginRef, Port, Processor,
    Sidechain,
};

const BLOCK: usize = 480;

fn environment() -> (Vst3Host, PathBuf) {
    let get =
        |name| PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")));
    (
        Vst3Host::new(get("PLUGIN_GRAPH_TEST_WORKER")),
        get("PLUGIN_GRAPH_TEST_VST3"),
    )
}

fn options() -> LoadOptions {
    LoadOptions {
        sample_rate: 48_000,
        block: BLOCK,
        channels: Channels::PreferMono,
        sidechain: Sidechain::Off,
        deadline: Duration::from_millis(50),
    }
}

#[test]
#[ignore = "needs a worker and a plugin"]
fn a_scan_lists_the_plugins_classes() {
    let (host, bundle) = environment();
    let scan = host.scan(&[bundle.parent().unwrap().to_path_buf()], Sandbox::Off);
    let found = scan.plugins.iter().any(|plugin| plugin.path == bundle);
    assert!(found, "{scan:?}");
}

#[test]
#[ignore = "needs a worker and a plugin"]
fn a_sandboxed_scan_lists_the_plugins_classes_too() {
    let (host, bundle) = environment();
    let scan = host.scan(&[bundle.parent().unwrap().to_path_buf()], Sandbox::On);
    let found = scan.plugins.iter().any(|plugin| plugin.path == bundle);
    assert!(found, "{scan:?}");
}

#[test]
#[ignore = "needs a worker and a plugin"]
fn a_plugin_processes_a_signal_in_a_graph_and_keeps_its_state() {
    let (host, bundle) = environment();
    let (mut control, processor) = host.load(&bundle, "", options(), None).unwrap();
    let layout = processor.layout();
    let state = control.save_state().unwrap();

    let spec = GraphSpec {
        nodes: vec![
            NodeSpec {
                id: 0,
                kind: NodeKind::Input,
                x: 0.,
                y: 0.,
            },
            NodeSpec {
                id: 1,
                kind: NodeKind::Output,
                x: 0.,
                y: 0.,
            },
            NodeSpec {
                id: 2,
                kind: NodeKind::Plugin(PluginRef {
                    path: bundle.display().to_string(),
                    class_id: String::new(),
                    name: "plugin".to_string(),
                    enabled: true,
                    sidechain: Sidechain::Off,
                }),
                x: 0.,
                y: 0.,
            },
        ],
        edges: (0..layout.main_inputs as u16)
            .map(|channel| Edge {
                from: Port { node: 0, port: 0 },
                to: Port {
                    node: 2,
                    port: channel,
                },
            })
            .chain([Edge {
                from: Port { node: 2, port: 0 },
                to: Port { node: 1, port: 0 },
            }])
            .collect(),
    };
    let mut graph = CompiledGraph::compile(
        &spec,
        GraphConfig {
            input_channels: 1,
            output_channels: 1,
            block: BLOCK,
        },
        HashMap::from([(2, Box::new(processor) as Box<dyn Processor>)]),
    )
    .unwrap();

    let blocks = 100;
    let mut heard = 0f32;
    for block in 0..blocks {
        let input = (0..BLOCK)
            .map(|frame| ((block * BLOCK + frame) as f32 * 0.05).sin() * 0.3)
            .collect::<Vec<_>>();
        let mut output = vec![0.; BLOCK];
        graph.process(&input, &mut output).unwrap();
        heard = output
            .iter()
            .fold(heard, |peak, sample| peak.max(sample.abs()));
    }
    assert_eq!(
        (graph.failures(), heard > 0.01),
        (vec![], true),
        "peak {heard}"
    );
    assert!(graph.latency() >= BLOCK, "latency {}", graph.latency());

    // The state it saved comes back when the plugin is loaded again.
    drop(control);
    let (mut again, _) = host.load(&bundle, "", options(), Some(&state)).unwrap();
    assert_eq!(again.save_state().unwrap().len(), state.len());
}

/// Noise at 48 kHz from a fixed generator, so every run hears the same.
fn noise(frames: usize) -> Vec<f32> {
    let mut state = 0x2545_f491_u32;
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32 - 0.5) * 0.2
        })
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// A second of noise through the plugin: the take stays as long as it was,
/// and, when the plugin is one that removes noise (such as RNNoise, which
/// `PLUGIN_GRAPH_TEST_REDUCES_NOISE=1` says it is), the noise floor drops.
#[test]
#[ignore = "needs a worker and a plugin"]
fn noise_through_a_plugin_keeps_its_length_and_a_denoiser_lowers_the_floor() {
    let (host, bundle) = environment();
    let (_control, processor) = host.load(&bundle, "", options(), None).unwrap();
    let layout = processor.layout();
    let mut spec = GraphSpec::passthrough();
    spec.nodes.push(NodeSpec {
        id: 2,
        kind: NodeKind::Plugin(PluginRef {
            path: bundle.display().to_string(),
            class_id: String::new(),
            name: "plugin".to_string(),
            enabled: true,
            sidechain: Sidechain::Off,
        }),
        x: 0.,
        y: 0.,
    });
    spec.edges = (0..layout.main_inputs as u16)
        .map(|channel| Edge {
            from: Port { node: 0, port: 0 },
            to: Port {
                node: 2,
                port: channel,
            },
        })
        .chain([Edge {
            from: Port { node: 2, port: 0 },
            to: Port { node: 1, port: 0 },
        }])
        .collect();
    let mut graph = CompiledGraph::compile(
        &spec,
        GraphConfig {
            input_channels: 1,
            output_channels: 1,
            block: BLOCK,
        },
        HashMap::from([(2, Box::new(processor) as Box<dyn Processor>)]),
    )
    .unwrap();

    let input = noise(48_000);
    // As a take does: drop the latency from the start, and flush it at the end.
    let latency = graph.latency();
    let padded = input
        .iter()
        .copied()
        .chain(std::iter::repeat_n(
            0.,
            latency.div_ceil(BLOCK) * BLOCK + BLOCK,
        ))
        .collect::<Vec<_>>();
    let mut output = Vec::new();
    for chunk in padded.as_chunks::<BLOCK>().0 {
        let mut block = vec![0.; BLOCK];
        graph.process(chunk, &mut block).unwrap();
        output.extend(block);
    }
    assert_eq!(graph.failures(), vec![], "the plugin did not process");
    let output = &output[latency..latency + input.len()];
    let (quiet_in, quiet_out) = (rms(&input[24_000..]), rms(&output[24_000..]));
    assert_eq!(output.len(), input.len());
    if std::env::var_os("PLUGIN_GRAPH_TEST_REDUCES_NOISE").is_some() {
        assert!(quiet_out < quiet_in * 0.8, "{quiet_in} in, {quiet_out} out");
    }
}
