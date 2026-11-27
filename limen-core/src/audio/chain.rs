//! The signal path between the microphone and the recognizer when the user
//! routes the voice through plugins: a [`CompiledGraph`] at 48 kHz, which
//! most voice plugins expect, then down to the recognizer's 16 kHz s16.

use plugin_graph::{CompiledGraph, Failure, GraphConfig, Resampler};
use tokio::sync::{mpsc, watch};

use super::{Level, SAMPLE_RATE};

/// The rate the graph runs at.
pub(crate) const GRAPH_RATE: u32 = 48_000;
/// Frames per block of the graph: 10 ms.
pub(crate) const GRAPH_BLOCK: usize = 480;

/// The graph's configuration for a mono microphone and a mono recognizer.
pub(crate) const GRAPH_CONFIG: GraphConfig = GraphConfig {
    input_channels: 1,
    output_channels: 1,
    block: GRAPH_BLOCK,
};

/// Mono samples at [`GRAPH_RATE`] in, 16 kHz s16 chunks out, whatever the
/// chunk size going in.
pub(crate) struct Chain {
    graph: CompiledGraph,
    /// Input not yet a whole block.
    pending: Vec<f32>,
    /// Output still to drop from the start: the graph's latency, so the
    /// take begins where the speech does.
    skip: usize,
    down: Resampler,
    block_out: Vec<f32>,
    converted: Vec<f32>,
}

impl Chain {
    pub fn new(graph: CompiledGraph) -> Self {
        Self {
            skip: graph.latency(),
            graph,
            pending: Vec::new(),
            down: Resampler::new(GRAPH_RATE, SAMPLE_RATE),
            block_out: vec![0.; GRAPH_BLOCK],
            converted: Vec::new(),
        }
    }

    /// Process `samples` and return the 16 kHz audio they complete.
    pub fn feed(&mut self, samples: &[f32]) -> Vec<i16> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / GRAPH_BLOCK * GRAPH_BLOCK;
        self.converted.clear();
        for block in self.pending[..whole].as_chunks::<GRAPH_BLOCK>().0 {
            // The graph's buffers are the ones given to it, so a size
            // mismatch is impossible here.
            let _ = self.graph.process(block, &mut self.block_out);
            let drop = self.skip.min(GRAPH_BLOCK);
            self.skip -= drop;
            self.down.push(&self.block_out[drop..], &mut self.converted);
        }
        self.pending.drain(..whole);
        self.converted
            .iter()
            .map(|sample| (sample.clamp(-1., 1.) * 32767.).round() as i16)
            .collect()
    }

    /// The end of the take: push out what the plugins still hold back, so
    /// the last of the speech is not cut off.
    pub fn finish(&mut self) -> Vec<i16> {
        let to_block = (GRAPH_BLOCK - self.pending.len() % GRAPH_BLOCK) % GRAPH_BLOCK;
        let flush = vec![0.; self.skip.max(self.graph.latency()) + to_block];
        self.feed(&flush)
    }

    /// The nodes that were passed through since they failed.
    pub fn failures(&self) -> Vec<Failure> {
        self.graph.failures()
    }
}

/// A whole recording of mono 48 kHz samples as the recognizer would hear it
/// with no plugins in between.
pub(crate) fn to_speech(samples: &[f32]) -> Vec<i16> {
    let mut converted = Vec::new();
    Resampler::new(GRAPH_RATE, SAMPLE_RATE).push(samples, &mut converted);
    converted
        .iter()
        .map(|sample| (sample.clamp(-1., 1.) * 32767.).round() as i16)
        .collect()
}

/// The chain's thread: process what `work` delivers until its sender is
/// dropped, then flush. Every chunk goes to `tx` and sets `level`, so the
/// meter shows what the recognizer hears.
pub(crate) fn run_worker(
    mut chain: Chain,
    work: std::sync::mpsc::Receiver<Vec<f32>>,
    tx: mpsc::Sender<Vec<i16>>,
    level: watch::Sender<Level>,
) {
    let send = |pcm: Vec<i16>| {
        if pcm.is_empty() {
            return;
        }
        level.send_replace(Level::from_s16(&pcm));
        // A lagging consumer must not stall the microphone: drop the chunk.
        if tx.try_send(pcm).is_err() {
            tracing::warn!("pcm channel full or closed, dropping a chunk");
        }
    };
    while let Ok(samples) = work.recv() {
        send(chain.feed(&samples));
    }
    send(chain.finish());
    for failure in chain.failures() {
        tracing::warn!(node = failure.node, reason = %failure.reason, "a plugin was passed through");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use plugin_graph::{
        CompiledGraph, GraphSpec, NodeKind, NodeSpec, PluginRef, PortLayout, ProcessError,
        Processor, Sidechain,
    };

    use tokio::sync::{mpsc, watch};

    use super::{Chain, GRAPH_CONFIG, run_worker};
    use crate::audio::Level;

    /// Delays its input by `frames`, as a lookahead limiter does.
    struct Lookahead {
        frames: usize,
        held: std::collections::VecDeque<f32>,
    }

    impl Processor for Lookahead {
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

        fn process(
            &mut self,
            _: usize,
            input: &[f32],
            output: &mut [f32],
        ) -> Result<(), ProcessError> {
            self.held.extend(input);
            output
                .iter_mut()
                .for_each(|out| *out = self.held.pop_front().unwrap_or(0.));
            Ok(())
        }
    }

    fn through_lookahead(frames: usize) -> Chain {
        let mut spec = GraphSpec::passthrough();
        spec.nodes.push(NodeSpec {
            id: 2,
            kind: NodeKind::Plugin(PluginRef {
                path: String::new(),
                class_id: String::new(),
                name: "limiter".to_string(),
                enabled: true,
                sidechain: Sidechain::Off,
            }),
            x: 0.,
            y: 0.,
        });
        spec.edges = vec![
            plugin_graph::Edge {
                from: plugin_graph::Port { node: 0, port: 0 },
                to: plugin_graph::Port { node: 2, port: 0 },
            },
            plugin_graph::Edge {
                from: plugin_graph::Port { node: 2, port: 0 },
                to: plugin_graph::Port { node: 1, port: 0 },
            },
        ];
        let processor = Lookahead {
            frames,
            held: std::iter::repeat_n(0., frames).collect(),
        };
        let graph = CompiledGraph::compile(
            &spec,
            GRAPH_CONFIG,
            HashMap::from([(2, Box::new(processor) as Box<dyn Processor>)]),
        )
        .unwrap();
        Chain::new(graph)
    }

    /// Run `seconds` of a constant 0.5 through in odd-sized pieces.
    fn take(chain: &mut Chain, seconds: usize) -> Vec<i16> {
        let signal = vec![0.5; 48_000 * seconds];
        let mut out = signal
            .chunks(1777)
            .flat_map(|chunk| chain.feed(chunk))
            .collect::<Vec<_>>();
        out.extend(chain.finish());
        out
    }

    #[test]
    fn a_lookahead_plugins_delay_is_made_up_so_the_whole_take_arrives() {
        let (with, without) = (
            take(&mut through_lookahead(1200), 1),
            take(&mut through_lookahead(0), 1),
        );
        // A second at 16 kHz, within the resampler's lookahead, whatever the
        // plugin's delay; and the speech is there at the end, not cut off.
        let settled = |take: &[i16]| take[take.len() - 200..take.len() - 100].to_vec();
        assert_eq!(
            (
                with.len().abs_diff(16_000) < 100,
                without.len().abs_diff(16_000) < 100,
                settled(&with),
                settled(&without),
            ),
            (true, true, vec![16384; 100], vec![16384; 100])
        );
    }

    #[test]
    fn the_worker_flushes_when_its_queue_closes_and_leaves_the_level_of_the_last_chunk() {
        let (feed, work) = std::sync::mpsc::sync_channel(8);
        let (tx, mut pcm) = mpsc::channel(8);
        let (level, levels) = watch::channel(Level::SILENCE);
        feed.send(vec![0.5; 4800]).unwrap();
        feed.send(vec![0.5; 4800]).unwrap();
        drop(feed);
        run_worker(through_lookahead(1200), work, tx, level);
        let mut heard = Vec::new();
        while let Ok(chunk) = pcm.try_recv() {
            heard.extend(chunk);
        }
        // 0.2 s in, 0.2 s out, the plugin's 1200 frames of delay included.
        assert_eq!(
            (
                heard.len().abs_diff(3200) < 100,
                *levels.borrow() != Level::SILENCE
            ),
            (true, true)
        );
    }
}
