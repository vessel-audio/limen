//! What a node computes.

/// How many channels a processor reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortLayout {
    /// Input channels of the main bus, which a bypassed processor passes on.
    pub main_inputs: usize,
    /// Input channels after the main ones: side-chain buses, which only
    /// this processor listens to.
    pub sidechain_inputs: usize,
    pub outputs: usize,
}

impl PortLayout {
    pub fn inputs(self) -> usize {
        self.main_inputs + self.sidechain_inputs
    }
}

/// Why a processor could not process a block.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProcessError(pub String);

/// A node's signal processing, run once per block by a compiled graph.
///
/// Buffers are planar: channel `c` of an `input` or `output` slice is
/// `[c * frames..(c + 1) * frames]`, input channels in port order (main bus
/// first, then side-chains). An implementation is built for the block size
/// its graph is compiled with, must not allocate or wait unboundedly inside
/// `process`, and returns an error when it cannot produce a block, after
/// which the graph passes the main input through for it from then on.
pub trait Processor: Send {
    /// The channels this processor has. Constant for its lifetime.
    fn layout(&self) -> PortLayout;

    /// Frames by which its output trails its input, which the graph makes up
    /// for on parallel paths. Constant for its lifetime.
    fn latency(&self) -> usize;

    /// Process one block of `frames` frames.
    fn process(
        &mut self,
        frames: usize,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<(), ProcessError>;
}

/// A mono level change.
pub(crate) struct Gain(pub f32);

impl Gain {
    pub fn from_db(db: f32) -> Self {
        Self(10f32.powf(db / 20.))
    }
}

impl Processor for Gain {
    fn layout(&self) -> PortLayout {
        PortLayout {
            main_inputs: 1,
            sidechain_inputs: 0,
            outputs: 1,
        }
    }

    fn latency(&self) -> usize {
        0
    }

    fn process(&mut self, _: usize, input: &[f32], output: &mut [f32]) -> Result<(), ProcessError> {
        input
            .iter()
            .zip(output)
            .for_each(|(sample, out)| *out = sample * self.0);
        Ok(())
    }
}
