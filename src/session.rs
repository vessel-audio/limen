//! Runtime resources for one audio/ASR session.
//!
//! This module owns microphone and provider handles; business transitions live
//! in `dictation`. Keeping the two apart makes the transition core pure.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio::{Capture, SAMPLE_RATE};
use crate::config::Config;
use crate::provider::{self, AudioSink, EventSource, SessionConfig, TranscriptEvent};

/// How long to wait for the capture task to die after abort.
const CAPTURE_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything belonging to one live dictation session.
pub(crate) struct Active {
    sink: Box<dyn AudioSink>,
    events: Box<dyn EventSource>,
    /// `Option` because a JoinHandle panics when polled after completion:
    /// reaping takes it out, so teardown can never poll it twice.
    capture: Option<tokio::task::JoinHandle<()>>,
}

enum CaptureReap {
    Reaped,
    TimedOut,
}

impl Active {
    /// Abort the capture task and wait until pw-record is actually dead
    /// (kill_on_drop runs when the task's `Capture` is dropped).
    async fn reap_capture(&mut self) -> CaptureReap {
        let Some(handle) = self.capture.take() else {
            return CaptureReap::Reaped;
        };
        handle.abort();
        match tokio::time::timeout(CAPTURE_REAP_TIMEOUT, handle).await {
            Ok(_) => CaptureReap::Reaped,
            Err(_) => {
                // Dropping the handle detaches the task, but the abort request
                // still stands. Do not submit audio that may still be appended.
                tracing::warn!("capture task did not exit within the reap timeout");
                CaptureReap::TimedOut
            }
        }
    }
}

/// Open a provider session and start capturing the microphone.
pub(crate) async fn start(
    cfg: &Config,
    active: &mut Option<Active>,
    pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>,
) -> Result<()> {
    let session_cfg = SessionConfig {
        model: cfg.provider.model.clone(),
        language: cfg.provider.language.clone(),
        sample_rate: SAMPLE_RATE,
        api_key: cfg.api_key(),
    };
    let session = provider::from_config(cfg)
        .start_session(&session_cfg)
        .await?;
    let (sink, events) = session.split();
    let (tx, rx) = mpsc::channel(64);
    let device = cfg.audio.device.clone();
    let chunk_ms = cfg.audio.chunk_ms;
    let capture = tokio::spawn(async move {
        if let Err(e) = capture_loop(device, chunk_ms, tx).await {
            tracing::warn!(error = %format!("{e:#}"), "capture loop failed");
        }
    });
    *pcm_rx = Some(rx);
    *active = Some(Active {
        sink,
        events,
        capture: Some(capture),
    });
    Ok(())
}

/// Buffer one captured chunk; a missing session is already stopped.
pub(crate) async fn buffer(active: &mut Option<Active>, pcm: &[i16]) -> Result<()> {
    match active.as_mut() {
        Some(active) => active.sink.send_audio(pcm).await,
        None => Ok(()),
    }
}

/// Read the next provider event from an active session.
pub(crate) async fn next_event(active: &mut Active) -> Option<TranscriptEvent> {
    active.events.next_event().await
}

/// Stop capturing, drain whatever PCM is still queued (the tail of the
/// utterance lives there), then finalize so the provider flushes it.
pub(crate) async fn finalize(
    active: &mut Option<Active>,
    pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>,
) -> Result<()> {
    let Some(a) = active.as_mut() else {
        return Ok(());
    };
    match a.reap_capture().await {
        CaptureReap::Reaped => {}
        CaptureReap::TimedOut => anyhow::bail!("microphone capture did not stop"),
    }
    if let Some(rx) = pcm_rx.as_mut() {
        while let Ok(pcm) = rx.try_recv() {
            a.sink.send_audio(&pcm).await?;
        }
    }
    a.sink.finalize().await
}

/// Drop the session and the PCM channel.
pub(crate) async fn teardown(
    active: &mut Option<Active>,
    pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>,
) {
    if let Some(mut a) = active.take() {
        let _ = a.reap_capture().await;
    }
    pcm_rx.take();
}

async fn capture_loop(
    device: Option<String>,
    chunk_ms: u32,
    tx: mpsc::Sender<Vec<i16>>,
) -> Result<()> {
    let mut capture = Capture::start(device.as_deref())?;
    let chunk_samples = (SAMPLE_RATE * chunk_ms / 1000) as usize;
    loop {
        match capture.read_chunk(chunk_samples).await? {
            Some(pcm) => {
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;

    use super::{Active, finalize};
    use crate::provider::{AudioSink, BoxFuture, EventSource, TranscriptEvent};

    #[derive(Debug, PartialEq, Eq)]
    enum SinkOperation {
        Audio(Vec<i16>),
        Finalized,
    }

    struct FakeSink {
        operations: Arc<Mutex<Vec<SinkOperation>>>,
    }

    impl AudioSink for FakeSink {
        fn send_audio<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async move {
                self.operations
                    .lock()
                    .unwrap()
                    .push(SinkOperation::Audio(pcm.to_vec()));
                Ok(())
            })
        }

        fn finalize(&mut self) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async move {
                self.operations
                    .lock()
                    .unwrap()
                    .push(SinkOperation::Finalized);
                Ok(())
            })
        }
    }

    struct PendingEvents;

    impl EventSource for PendingEvents {
        fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn finalize_drains_queued_audio_before_finalizing() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut active = Some(Active {
            sink: Box::new(FakeSink {
                operations: Arc::clone(&operations),
            }),
            events: Box::new(PendingEvents),
            capture: None,
        });
        let (tx, rx) = mpsc::channel(2);
        tx.send(vec![1, 2]).await.unwrap();
        tx.send(vec![3, 4]).await.unwrap();
        drop(tx);
        let mut pcm_rx = Some(rx);

        finalize(&mut active, &mut pcm_rx).await.unwrap();

        assert_eq!(
            *operations.lock().unwrap(),
            [
                SinkOperation::Audio(vec![1, 2]),
                SinkOperation::Audio(vec![3, 4]),
                SinkOperation::Finalized,
            ]
        );
    }
}
