//! Playing a test recording back through the speakers.

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use anyhow::{Context, Result, bail};
    use cpal::SampleFormat;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use plugin_graph::Resampler;

    /// A recording playing on the default output device. Dropping it stops
    /// the sound.
    pub struct Playback {
        _stream: cpal::Stream,
        finished: Arc<AtomicBool>,
        /// Frames the device has taken, at its own rate.
        played: Arc<AtomicUsize>,
        rate: u32,
        offset: Duration,
    }

    impl Playback {
        /// Start playing a mono 16-bit WAV file from the beginning.
        pub fn start(wav: &Path) -> Result<Self> {
            Self::start_at(wav, Duration::ZERO)
        }

        /// Start playing a mono 16-bit WAV file from `offset` into it, as when
        /// switching between two recordings of the same moment.
        pub fn start_at(wav: &Path, offset: Duration) -> Result<Self> {
            let mut reader = hound::WavReader::open(wav)
                .with_context(|| format!("failed to open {}", wav.display()))?;
            let input_rate = reader.spec().sample_rate;
            let skip = (offset.as_secs_f64() * f64::from(input_rate)) as usize;
            let samples = reader
                .samples::<i16>()
                .skip(skip)
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
            let played = Arc::new(AtomicUsize::new(0));
            let counted = played.clone();
            let mut position = 0;
            let stream = device
                .build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        for frame in data.chunks_mut(channels) {
                            frame.fill(resampled.get(position).copied().unwrap_or(0.));
                            position += 1;
                        }
                        counted.store(position.min(resampled.len()), Ordering::Relaxed);
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
                played,
                rate: config.sample_rate,
                offset,
            })
        }

        /// How far into the recording the sound is.
        pub fn position(&self) -> Duration {
            self.offset
                + Duration::from_secs_f64(
                    self.played.load(Ordering::Relaxed) as f64 / f64::from(self.rate),
                )
        }

        /// Whether the recording has played to its end.
        pub fn finished(&self) -> bool {
            self.finished.load(Ordering::Relaxed)
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};

    /// A recording playing through `pw-play`. Dropping it stops the sound.
    pub struct Playback {
        child: Child,
        started: Instant,
        offset: Duration,
        /// The copy of the recording that starts at the offset, if one was
        /// needed, since `pw-play` cannot start partway in.
        trimmed: Option<PathBuf>,
    }

    impl Playback {
        /// Start playing a mono 16-bit WAV file from the beginning.
        pub fn start(wav: &Path) -> Result<Self> {
            Self::start_at(wav, Duration::ZERO)
        }

        /// Start playing a mono 16-bit WAV file from `offset` into it, as when
        /// switching between two recordings of the same moment.
        pub fn start_at(wav: &Path, offset: Duration) -> Result<Self> {
            let trimmed = match offset.is_zero() {
                true => None,
                false => {
                    let mut reader = hound::WavReader::open(wav)
                        .with_context(|| format!("failed to open {}", wav.display()))?;
                    let spec = reader.spec();
                    let skip = (offset.as_secs_f64() * f64::from(spec.sample_rate)) as usize;
                    let samples = reader
                        .samples::<i16>()
                        .skip(skip)
                        .collect::<Result<Vec<_>, _>>()
                        .context("failed to read the recording")?;
                    let path = std::env::temp_dir()
                        .join(format!("limen-playback-{}.wav", std::process::id()));
                    let mut writer = hound::WavWriter::create(&path, spec)
                        .context("failed to write the rest of the recording")?;
                    for sample in samples {
                        writer.write_sample(sample)?;
                    }
                    writer.finalize()?;
                    Some(path)
                }
            };
            let child = Command::new("pw-play")
                .arg(trimmed.as_deref().unwrap_or(wav))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("failed to spawn pw-play")?;
            Ok(Self {
                child,
                started: Instant::now(),
                offset,
                trimmed,
            })
        }

        /// How far into the recording the sound is.
        pub fn position(&self) -> Duration {
            self.offset + self.started.elapsed()
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
            if let Some(trimmed) = &self.trimmed {
                let _ = std::fs::remove_file(trimmed);
            }
        }
    }
}

pub use imp::Playback;
