//! A plugin as a graph node.

use std::time::Duration;

use vst_audio::{BlockEndpoint, Rendered};

use super::host::{Channels, LoadOptions};
use crate::{PortLayout, ProcessError, Processor};

/// The audio side of a loaded plugin.
///
/// Its latency is what the worker reported when the plugin loaded, plus one
/// block of transport; a plugin that changes its latency later is not
/// followed until it is loaded again.
pub struct Vst3Processor {
    blocks: BlockEndpoint,
    block: usize,
    deadline: Duration,
    layout: PortLayout,
    latency: usize,
    /// Set when mono was asked for and the plugin only takes stereo: the
    /// processor then presents one channel in and out, and what it hands the
    /// plugin is the voice on both channels, which it averages back.
    fold: Option<Fold>,
}

/// Scratch buffers for a plugin that takes stereo in place of the mono the
/// graph speaks.
struct Fold {
    input: Vec<f32>,
    output: Vec<f32>,
    /// Channels of the plugin's input beyond the main bus.
    sidechains: usize,
}

impl Vst3Processor {
    /// `main_inputs` of the endpoint's input channels belong to the main bus,
    /// which `buses::choose` puts first.
    pub(super) fn new(blocks: BlockEndpoint, main_inputs: usize, options: LoadOptions) -> Self {
        let geometry = blocks.geometry();
        let (inputs, outputs) = (
            geometry.input_channels as usize,
            geometry.output_channels as usize,
        );
        let sidechains = inputs - main_inputs;
        let folded = options.channels == Channels::PreferMono && main_inputs == 2 && outputs == 2;
        let layout = match folded {
            true => PortLayout {
                main_inputs: 1,
                sidechain_inputs: sidechains,
                outputs: 1,
            },
            false => PortLayout {
                main_inputs,
                sidechain_inputs: sidechains,
                outputs,
            },
        };
        Self {
            layout,
            latency: blocks.latency_frames(),
            fold: folded.then(|| Fold {
                input: vec![0.; inputs * options.block],
                output: vec![0.; outputs * options.block],
                sidechains,
            }),
            blocks,
            block: options.block,
            deadline: options.deadline,
        }
    }
}

impl Vst3Processor {
    /// Start the pipeline over from silence, as before a first block. A
    /// processor kept for another run needs this, or the end of the last run
    /// comes out first.
    pub fn restart(&mut self) -> Result<(), ProcessError> {
        self.blocks
            .restart(self.blocks.depth())
            .map_err(|error| ProcessError(format!("{error:?}")))
    }
}

impl Vst3Processor {
    /// A block's outcome: while the pipeline is still filling, that much of
    /// the start is silence, which the reported latency accounts for.
    fn finish(
        result: Result<Rendered, vst_audio::RenderError>,
        output: &mut [f32],
    ) -> Result<(), ProcessError> {
        match result {
            Ok(Rendered::Delivered) => Ok(()),
            Ok(Rendered::Priming) => {
                output.fill(0.);
                Ok(())
            }
            Err(error) => Err(ProcessError(error.to_string())),
        }
    }
}

impl Processor for Vst3Processor {
    fn layout(&self) -> PortLayout {
        self.layout
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn process(
        &mut self,
        frames: usize,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<(), ProcessError> {
        if frames != self.block {
            return Err(ProcessError(format!(
                "the plugin works in blocks of {}, not {frames}",
                self.block
            )));
        }
        let Some(fold) = self.fold.as_mut() else {
            let result = self.blocks.process_wait(input, output, self.deadline);
            return Self::finish(result, output);
        };
        // The voice on both channels, then the side-chains as they came.
        let (voice, sidechains) = input.split_at(frames);
        fold.input[..frames].copy_from_slice(voice);
        fold.input[frames..2 * frames].copy_from_slice(voice);
        fold.input[2 * frames..2 * frames + fold.sidechains * frames].copy_from_slice(sidechains);
        let result = self
            .blocks
            .process_wait(&fold.input, &mut fold.output, self.deadline);
        let (left, right) = fold.output.split_at(frames);
        // Averaged back to the one channel the graph has.
        output
            .iter_mut()
            .zip(left.iter().zip(right))
            .for_each(|(out, (left, right))| *out = (left + right) / 2.);
        Self::finish(result, output)
    }
}
