//! A plugin as a graph node.

use std::time::Duration;

use vst_audio::{BlockEndpoint, Rendered};

use super::host::LoadOptions;
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
}

impl Vst3Processor {
    /// `main_inputs` of the endpoint's input channels belong to the main bus,
    /// which `buses::choose` puts first.
    pub(super) fn new(blocks: BlockEndpoint, main_inputs: usize, options: LoadOptions) -> Self {
        let geometry = blocks.geometry();
        Self {
            layout: PortLayout {
                main_inputs,
                sidechain_inputs: geometry.input_channels as usize - main_inputs,
                outputs: geometry.output_channels as usize,
            },
            latency: blocks.latency_frames(),
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
        match self.blocks.process_wait(input, output, self.deadline) {
            Ok(Rendered::Delivered) => Ok(()),
            // The pipeline is still filling: that much of the start is
            // silence, which the reported latency accounts for.
            Ok(Rendered::Priming) => {
                output.fill(0.);
                Ok(())
            }
            Err(error) => Err(ProcessError(error.to_string())),
        }
    }
}
