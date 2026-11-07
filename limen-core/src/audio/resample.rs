//! Linear resampling from the device's negotiated rate down to 16 kHz.
//!
//! Deliberately simple: dictation audio goes straight to an ASR model, and
//! linear interpolation at these ratios is inaudible for speech while being
//! cheap enough to run on the forwarder thread for every chunk.

use super::SAMPLE_RATE;

/// Stateful across chunks: interpolation at a chunk boundary needs the last
/// frame of the previous chunk.
pub struct LinearResampler {
    in_rate: u32,
    /// Position of the next output sample, in input frames.
    pos: f64,
    /// Last frame of the previous chunk; interpolation's left endpoint.
    prev: f32,
    primed: bool,
}

impl LinearResampler {
    pub fn new(in_rate: u32) -> Self {
        Self {
            in_rate,
            pos: 0.0,
            prev: 0.0,
            primed: false,
        }
    }

    /// Resample mono f32 input and append 16-bit samples to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<i16>) {
        if input.is_empty() {
            return;
        }
        let ratio = f64::from(self.in_rate) / f64::from(SAMPLE_RATE);
        if (ratio - 1.0).abs() < f64::EPSILON {
            out.extend(input.iter().map(|sample| quantize(*sample)));
            self.prev = *input.last().unwrap();
            self.primed = true;
            return;
        }
        // Output sample i reads input position `pos`; the first read of a
        // chunk interpolates between `prev` (index -1) and input[0].
        let mut index = 0usize;
        while self.pos < (input.len() - 1) as f64 || (index == 0 && !self.primed) {
            let left = self.pos.floor() as i64;
            let frac = (self.pos - left as f64) as f32;
            let a = match left {
                -1 => self.prev,
                n if n >= 0 => input[n as usize],
                _ => break,
            };
            let b = match usize::try_from(left + 1) {
                Ok(n) if n < input.len() => input[n],
                _ => a,
            };
            out.push(quantize(a + (b - a) * frac));
            self.pos += ratio;
            index += 1;
            self.primed = true;
        }
        self.pos -= input.len() as f64;
        self.prev = *input.last().unwrap();
    }
}

fn quantize(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::{LinearResampler, SAMPLE_RATE};

    #[test]
    fn identity_at_16k() {
        let mut resampler = LinearResampler::new(SAMPLE_RATE);
        let input: Vec<f32> = (0..160).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut out = Vec::new();
        resampler.push(&input, &mut out);
        assert_eq!(out.len(), input.len());
        for (sample, expected) in out.iter().zip(input.iter()) {
            let error = (*sample as f32 / 32767.0 - expected).abs();
            assert!(error < 1e-4, "{sample} vs {expected}");
        }
    }

    #[test]
    fn triples_down_from_48k() {
        let mut resampler = LinearResampler::new(48_000);
        let input = vec![0.5_f32; 480];
        let mut out = Vec::new();
        resampler.push(&input, &mut out);
        // 480 input frames at 48k = 160 output frames at 16k, ±1 frame of
        // boundary slack.
        assert!((out.len() as i64 - 160).abs() <= 1, "{}", out.len());
        assert!(out.iter().all(|&s| s > 16_000));
    }

    #[test]
    fn chunk_boundaries_are_continuous() {
        let mut chunked = LinearResampler::new(48_000);
        let mut whole = LinearResampler::new(48_000);
        let input: Vec<f32> = (0..960).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut a = Vec::new();
        let mut b = Vec::new();
        for chunk in input.chunks(100) {
            chunked.push(chunk, &mut a);
        }
        whole.push(&input, &mut b);
        let diff = (a.len() as i64 - b.len() as i64).abs();
        assert!(diff <= 1, "chunked {} vs whole {}", a.len(), b.len());
        let shared = a.len().min(b.len());
        assert_eq!(&a[..shared], &b[..shared]);
    }

    #[test]
    fn silence_stays_silence() {
        let mut resampler = LinearResampler::new(44_100);
        let mut out = Vec::new();
        resampler.push(&[0.0; 441], &mut out);
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn empty_input_produces_nothing() {
        let mut resampler = LinearResampler::new(48_000);
        let mut out = Vec::new();
        resampler.push(&[], &mut out);
        assert!(out.is_empty());
    }
}
