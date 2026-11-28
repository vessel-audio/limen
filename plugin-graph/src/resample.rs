//! Sample-rate conversion with a windowed-sinc filter, so converting down
//! does not fold content above the new Nyquist rate back into the band.

use std::f64::consts::PI;

/// Zero crossings of the sinc kernel on each side, at the lower of the two
/// rates. More is sharper and costs more per output sample.
const CROSSINGS: f64 = 16.;

/// A streaming mono resampler: feed it blocks of any length and it returns
/// whatever output they complete, independent of how the input is cut up.
///
/// Its output trails the input by [`Resampler::delay`] frames of the input
/// rate, the filter's lookahead.
pub struct Resampler {
    /// Input frames per output frame.
    step: f64,
    /// Kernel scale: 1 up to the lower rate's Nyquist, less when converting
    /// down.
    scale: f64,
    /// Half the kernel's width, in input frames.
    half: f64,
    /// Input frames in `buffer`'s place before it, counting the silence the
    /// first frames' kernels read before the signal starts.
    dropped: usize,
    /// The input still needed, from absolute frame `dropped`.
    buffer: Vec<f32>,
    /// Output frames produced so far. Each frame's place is worked out from
    /// this count alone, so no rounding depends on how the input is cut up.
    produced: u64,
    passthrough: bool,
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        let scale = (f64::from(output_rate) / f64::from(input_rate)).min(1.);
        let half = CROSSINGS / scale;
        Self {
            step: f64::from(input_rate) / f64::from(output_rate),
            scale,
            half,
            dropped: 0,
            // The kernel's reach before the first frame reads silence.
            buffer: vec![0.; half.ceil() as usize],
            produced: 0,
            passthrough: input_rate == output_rate,
        }
    }

    /// Frames of the input rate by which the output trails the input.
    pub fn delay(&self) -> usize {
        match self.passthrough {
            true => 0,
            false => self.half.ceil() as usize,
        }
    }

    /// Convert `input`, appending the finished frames to `output`.
    pub fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough {
            output.extend_from_slice(input);
            return;
        }
        self.buffer.extend_from_slice(input);
        let end = (self.dropped + self.buffer.len()) as f64;
        let reach = self.half.ceil();
        loop {
            // The first frame sits at the first input frame, after the
            // silence ahead of it.
            let centre = reach + self.produced as f64 * self.step;
            if centre + self.half >= end {
                break;
            }
            let first = (centre - self.half).ceil() as usize;
            let last = (centre + self.half).floor() as usize;
            output.push(
                (first..=last)
                    .map(|frame| {
                        f64::from(self.buffer[frame - self.dropped])
                            * self.kernel(frame as f64 - centre)
                    })
                    .sum::<f64>() as f32,
            );
            self.produced += 1;
        }
        // Keep what the next frame's kernel still reaches back to.
        let next = reach + self.produced as f64 * self.step;
        let keep_from = ((next - self.half).ceil() as usize).saturating_sub(self.dropped);
        self.buffer.drain(..keep_from);
        self.dropped += keep_from;
    }

    /// The kernel's weight `offset` input frames from the output frame.
    fn kernel(&self, offset: f64) -> f64 {
        let x = offset * self.scale;
        let sinc = match x == 0. {
            true => 1.,
            false => (PI * x).sin() / (PI * x),
        };
        let t = offset / self.half;
        let window = 0.42 + 0.5 * (PI * t).cos() + 0.08 * (2. * PI * t).cos();
        self.scale * sinc * window
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    fn sine(rate: u32, hertz: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                (frame as f64 / f64::from(rate) * hertz * std::f64::consts::TAU).sin() as f32
            })
            .collect()
    }

    fn convert(from: u32, to: u32, input: &[f32], cut: usize) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to);
        let mut output = Vec::new();
        input
            .chunks(cut)
            .for_each(|chunk| resampler.push(chunk, &mut output));
        output
    }

    /// The largest magnitude once the filter has settled.
    fn peak(signal: &[f32]) -> f32 {
        signal[signal.len() / 2..]
            .iter()
            .fold(0., |peak, sample| f32::max(peak, sample.abs()))
    }

    #[test]
    fn equal_rates_pass_the_signal_through_untouched() {
        let input = sine(48_000, 440., 1000);
        assert_eq!(convert(48_000, 48_000, &input, 128), input);
    }

    #[test]
    fn voice_frequencies_keep_their_level_and_high_ones_are_removed() {
        let down = |hertz| peak(&convert(48_000, 16_000, &sine(48_000, hertz, 9600), 480));
        let (voice, above_nyquist) = (down(1000.), down(10_000.));
        assert!(
            (voice - 1.).abs() < 0.01 && above_nyquist < 0.01,
            "1 kHz kept at {voice}, 10 kHz left at {above_nyquist}"
        );
    }

    #[test]
    fn converting_up_keeps_the_level_too() {
        let up = peak(&convert(44_100, 48_000, &sine(44_100, 1000., 8820), 441));
        assert!((up - 1.).abs() < 0.01, "{up}");
    }

    #[test]
    fn the_output_does_not_depend_on_how_the_input_is_cut() {
        let input = sine(44_100, 700., 4410);
        let whole = convert(44_100, 16_000, &input, input.len());
        let pieces = convert(44_100, 16_000, &input, 97);
        assert_eq!(whole, pieces);
    }

    #[test]
    fn the_frame_count_follows_the_rate_ratio() {
        let input = vec![0.5; 48_000];
        let output = convert(48_000, 16_000, &input, 480);
        // One second in is a second out, less the filter's lookahead.
        assert!(
            (15_900..=16_000).contains(&output.len()),
            "{}",
            output.len()
        );
    }
}
