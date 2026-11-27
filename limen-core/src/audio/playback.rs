//! Playing a test recording back through the speakers.

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use anyhow::{Context, Result, bail};
    use cpal::SampleFormat;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use plugin_graph::Resampler;

    /// A recording playing on the default output device. Dropping it stops
    /// the sound.
    pub struct Playback {
        _stream: cpal::Stream,
        finished: Arc<AtomicBool>,
    }

    impl Playback {
        /// Start playing a mono 16-bit WAV file.
        pub fn start(wav: &Path) -> Result<Self> {
            let mut reader = hound::WavReader::open(wav)
                .with_context(|| format!("failed to open {}", wav.display()))?;
            let input_rate = reader.spec().sample_rate;
            let samples = reader
                .samples::<i16>()
                .map(|sample| sample.map(|sample| f32::from(sample) / 32768.))
                .collect::<Result<Vec<_>, _>>()
                .context("failed to read the recording")?;

            let device = cpal::default_host()
                .default_output_device()
                .context("no output device")?;
            let supported = device
                .default_output_config()
                .context("the output device has no default configuration")?;
            if supported.sample_format() != SampleFormat::F32 {
                bail!("the output device does not take f32 samples");
            }
            let config = supported.config();
            let channels = usize::from(config.channels);
            let mut resampled = Vec::with_capacity(samples.len() * 3);
            let mut resampler = Resampler::new(input_rate, config.sample_rate);
            resampler.push(&samples, &mut resampled);
            // The filter's lookahead: silence after the end pushes the last
            // of the recording out.
            resampler.push(&vec![0.; resampler.delay() + 1], &mut resampled);

            let finished = Arc::new(AtomicBool::new(false));
            let done = finished.clone();
            let mut position = 0;
            let stream = device
                .build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        for frame in data.chunks_mut(channels) {
                            frame.fill(resampled.get(position).copied().unwrap_or(0.));
                            position += 1;
                        }
                        if position >= resampled.len() {
                            done.store(true, Ordering::Relaxed);
                        }
                    },
                    |error| tracing::warn!(%error, "playback stream error"),
                    None,
                )
                .context("failed to open the output stream")?;
            stream.play().context("failed to start playback")?;
            Ok(Self {
                _stream: stream,
                finished,
            })
        }

        /// Whether the recording has played to its end.
        pub fn finished(&self) -> bool {
            self.finished.load(Ordering::Relaxed)
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    use anyhow::{Context, Result};

    /// A recording playing through `pw-play`. Dropping it stops the sound.
    pub struct Playback {
        child: Child,
    }

    impl Playback {
        /// Start playing a WAV file.
        pub fn start(wav: &Path) -> Result<Self> {
            let child = Command::new("pw-play")
                .arg(wav)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to spawn pw-play")?;
            Ok(Self { child })
        }

        /// Whether the recording has played to its end.
        pub fn finished(&mut self) -> bool {
            !matches!(self.child.try_wait(), Ok(None))
        }
    }

    impl Drop for Playback {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub use imp::Playback;
