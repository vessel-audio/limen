//! Microphone capture via the system `pw-record` (PipeWire) binary.
//!
//! Spawns `pw-record --container raw --format s16 --rate 16000 --channels 1 -`
//! and reads raw s16le PCM straight off its stdout. (`--container raw` matters:
//! writing to `-` makes pw-record pick a container we can't parse, while `raw`
//! is header-less.)

use std::process::Stdio;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub const SAMPLE_RATE: u32 = 16_000;

pub struct Capture {
    child: Child,
}

impl Capture {
    /// Start recording from the given pw-record target (`None` = default).
    pub fn start(device: Option<&str>) -> Result<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args([
            "--container",
            "raw",
            "--format",
            "s16",
            "--rate",
            "16000",
            "--channels",
            "1",
        ])
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Reaping is driven by dropping the capture task.
        .kill_on_drop(true);
        if let Some(dev) = device {
            cmd.args(["--target", dev]);
        }
        let child = cmd.spawn().context("failed to spawn pw-record")?;
        Ok(Self { child })
    }

    /// Read exactly `samples` s16le samples; `Ok(None)` on EOF.
    pub async fn read_chunk(&mut self, samples: usize) -> Result<Option<Vec<i16>>> {
        let stdout = self.child.stdout.as_mut().context("pw-record stdout")?;
        let mut buf = vec![0u8; samples * 2];
        match stdout.read_exact(&mut buf).await {
            Ok(_) => Ok(Some(
                buf.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| i16::from_le_bytes(*bytes))
                    .collect(),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Recording amplitude at one captured PCM chunk.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Root-mean-square amplitude normalized to 0.0–1.0.
    pub rms: f32,
    /// Amplitude in dBFS, floored at −96 dBFS.
    pub dbfs: f32,
}

impl Level {
    /// The level published whenever capture is not running.
    pub const SILENCE: Self = Self {
        rms: 0.0,
        dbfs: -96.0,
    };

    /// Measure one signed 16-bit PCM chunk.
    pub(crate) fn from_s16(samples: &[i16]) -> Self {
        let rms = match samples.is_empty() {
            true => 0.0,
            false => {
                let mean = samples
                    .iter()
                    .map(|sample| {
                        let sample = f64::from(*sample) / 32768.0;
                        sample * sample
                    })
                    .sum::<f64>()
                    / samples.len() as f64;
                mean.sqrt() as f32
            }
        };
        Self {
            rms,
            dbfs: (20.0 * rms.log10()).max(-96.0),
        }
    }
}

const CAPTURE_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The result of stopping and joining a capture task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureReap {
    Reaped,
    TimedOut,
}

/// Abort a capture task, reset its level, and bound the join wait.
pub(crate) async fn reap_capture(
    handle: JoinHandle<()>,
    level: watch::Sender<Level>,
) -> CaptureReap {
    level.send_replace(Level::SILENCE);
    handle.abort();
    match tokio::time::timeout(CAPTURE_REAP_TIMEOUT, handle).await {
        Ok(_) => CaptureReap::Reaped,
        Err(_) => {
            tracing::warn!("capture task did not exit within the reap timeout");
            CaptureReap::TimedOut
        }
    }
}

/// Capture PCM and measure it before a back-pressured provider can delay it.
pub(crate) async fn capture_loop(
    device: Option<String>,
    chunk_ms: u32,
    tx: mpsc::Sender<Vec<i16>>,
    level: watch::Sender<Level>,
) -> Result<()> {
    let result = async {
        let mut capture = Capture::start(device.as_deref())?;
        let chunk_samples = (SAMPLE_RATE * chunk_ms / 1000) as usize;
        loop {
            match capture.read_chunk(chunk_samples).await? {
                Some(pcm) => {
                    level.send_replace(Level::from_s16(&pcm));
                    // A lagging consumer must not stall the microphone: drop frames.
                    match tx.try_send(pcm) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::warn!("pcm channel full, dropping a chunk");
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                    }
                }
                None => return Ok(()),
            }
        }
    }
    .await;
    level.send_replace(Level::SILENCE);
    result
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn silence_and_full_scale_have_exact_levels() {
        assert_eq!(Level::from_s16(&[]), Level::SILENCE);
        assert_eq!(Level::from_s16(&[0; 128]), Level::SILENCE);
        assert_eq!(
            Level::from_s16(&[i16::MIN; 2]),
            Level {
                rms: 1.0,
                dbfs: 0.0,
            }
        );
    }

    #[test]
    fn a_full_scale_sine_is_about_minus_three_dbfs() {
        let samples = (0..1024)
            .map(|index| {
                let angle = index as f64 * std::f64::consts::TAU / 128.0;
                (angle.sin() * f64::from(i16::MAX)) as i16
            })
            .collect::<Vec<_>>();
        let level = Level::from_s16(&samples);
        assert!((level.rms - 0.707).abs() < 0.01, "{level:?}");
        assert!((level.dbfs + 3.01).abs() < 0.01, "{level:?}");
    }
}
