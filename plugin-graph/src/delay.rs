//! Delay lines, which line up parallel paths of different latency.

/// A fixed delay of whole frames, carried across blocks.
pub(crate) struct DelayLine {
    frames: Vec<f32>,
    position: usize,
}

impl DelayLine {
    pub fn new(delay: usize) -> Self {
        Self {
            frames: vec![0.; delay],
            position: 0,
        }
    }

    /// Add `input`, delayed, to `output`.
    pub fn add_into(&mut self, input: &[f32], output: &mut [f32]) {
        match self.frames.len() {
            0 => input
                .iter()
                .zip(output)
                .for_each(|(sample, out)| *out += sample),
            length => input.iter().zip(output).for_each(|(sample, out)| {
                *out += std::mem::replace(&mut self.frames[self.position], *sample);
                self.position = (self.position + 1) % length;
            }),
        }
    }
}
