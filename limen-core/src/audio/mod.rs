//! Microphone capture. Linux spawns the `pw-record` (PipeWire) binary and
//! reads raw s16le PCM off its stdout; macOS captures through cpal/CoreAudio
//! and resamples in software (`coreaudio`, `resample`). Both deliver the same
//! contract: 16 kHz mono s16le chunks.

mod chain;
#[cfg(target_os = "macos")]
mod coreaudio;
#[cfg(target_os = "linux")]
mod pipewire;
mod playback;
mod plugins;
#[cfg(target_os = "macos")]
mod resample;
mod test_take;

#[cfg(target_os = "macos")]
pub(crate) use coreaudio::{Capture, FloatCapture};
#[cfg(target_os = "linux")]
pub(crate) use pipewire::{Capture, FloatCapture};

pub(crate) use chain::Chain;
pub use playback::Playback;
pub(crate) use plugins::PluginChains;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

pub const SAMPLE_RATE: u32 = 16_000;

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

async fn forward_chunks(
    tx: mpsc::Sender<Vec<i16>>,
    level: watch::Sender<Level>,
    mut read_chunk: impl AsyncFnMut() -> Result<Option<Vec<i16>>>,
) -> Result<()> {
    let result = async {
        loop {
            match read_chunk().await? {
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
        forward_chunks(tx, level.clone(), async move || {
            capture.read_chunk(chunk_samples).await
        })
        .await
    }
    .await;
    level.send_replace(Level::SILENCE);
    result
}

/// Chunks of microphone audio waiting for the chain; past this, new ones are
/// dropped, as when the recognizer lags.
const CHAIN_QUEUE: usize = 64;

/// Capture the microphone through `chain` until `stop` fires (or is dropped)
/// or capture ends. The chain runs on a thread of its own, because a plugin
/// may take milliseconds to answer and no async worker should wait for it.
/// Once the microphone is released the chain is flushed, so what the plugins
/// hold back still reaches `tx`.
pub(crate) async fn chain_capture_loop(
    device: Option<String>,
    chunk_ms: u32,
    tx: mpsc::Sender<Vec<i16>>,
    level: watch::Sender<Level>,
    chain: Chain,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    let (feed, work) = std::sync::mpsc::sync_channel(CHAIN_QUEUE);
    let worker = std::thread::Builder::new()
        .name("limen-chain".into())
        .spawn({
            let level = level.clone();
            move || chain::run_worker(chain, work, tx, level)
        })?;
    let result = async {
        let mut capture = FloatCapture::start(device.as_deref())?;
        let chunk = (chain::GRAPH_RATE * chunk_ms / 1000) as usize;
        loop {
            let read = tokio::select! {
                read = capture.read_chunk(chunk) => read?,
                _ = &mut stop => None,
            };
            let Some(samples) = read else {
                return Ok(());
            };
            match feed.try_send(samples) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    tracing::warn!("chain queue full, dropping a chunk");
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return Ok(()),
            }
        }
    }
    .await;
    // Closing the queue lets the worker flush the chain and finish.
    drop(feed);
    let _ = tokio::task::spawn_blocking(move || worker.join()).await;
    level.send_replace(Level::SILENCE);
    result
}

/// How long a chain capture may take to flush and exit once asked to stop.
const CHAIN_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Ask a chain capture to stop and wait for it to flush, falling back to
/// aborting it if it does not exit in time.
pub(crate) async fn stop_chain_capture(
    mut handle: JoinHandle<()>,
    stop: oneshot::Sender<()>,
    level: watch::Sender<Level>,
) -> CaptureReap {
    let _ = stop.send(());
    match tokio::time::timeout(CHAIN_STOP_TIMEOUT, &mut handle).await {
        Ok(_) => {
            level.send_replace(Level::SILENCE);
            CaptureReap::Reaped
        }
        Err(_) => reap_capture(handle, level).await,
    }
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
        let square = Level::from_s16(&[i16::MIN, i16::MAX]);
        assert!(square.rms > 0.999, "{square:?}");
        assert!(square.dbfs.abs() < 0.001, "{square:?}");
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

    #[tokio::test]
    async fn each_chunk_is_published_before_eof_resets_the_level() {
        use std::collections::VecDeque;

        use tokio::sync::{mpsc, watch};

        let (tx, mut pcm) = mpsc::channel(2);
        let (level, mut levels) = watch::channel(Level::SILENCE);
        let (release_tx, mut release_rx) = mpsc::unbounded_channel();
        let mut chunks = VecDeque::from([vec![i16::MIN], vec![0], vec![]]);
        let task = tokio::spawn(super::forward_chunks(tx, level, async move || {
            let chunk = chunks.pop_front();
            let _ = release_rx.recv().await;
            Ok(chunk.filter(|chunk| !chunk.is_empty()))
        }));

        release_tx.send(()).unwrap();
        levels.changed().await.unwrap();
        assert_eq!(
            *levels.borrow(),
            Level {
                rms: 1.0,
                dbfs: 0.0,
            }
        );
        release_tx.send(()).unwrap();
        levels.changed().await.unwrap();
        assert_eq!(*levels.borrow(), Level::SILENCE);
        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(pcm.recv().await, Some(vec![i16::MIN]));
        assert_eq!(pcm.recv().await, Some(vec![0]));
    }

    #[tokio::test]
    async fn a_read_error_also_resets_the_level() {
        use tokio::sync::{mpsc, watch};

        let (tx, _pcm) = mpsc::channel(1);
        let (level, levels) = watch::channel(Level {
            rms: 0.5,
            dbfs: -6.0,
        });
        let result = super::forward_chunks(tx, level.clone(), || async {
            Err(anyhow::anyhow!("read failed"))
        })
        .await;

        assert!(result.is_err());
        assert_eq!(*levels.borrow(), Level::SILENCE);
    }
}
