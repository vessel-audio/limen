//! The resident daemon: one `select!` loop owns the ASR sink, the event
//! stream, and the state machine, so session state never crosses tasks.
//! Injection runs in its own task so a slow injector can never stall the loop.
//!
//! State machine: Idle --toggle--> Recording --toggle--> Finalizing
//! --(provider terminal + output queue drained)--> Idle.
//! A toggle during Finalizing sets `pending_restart`: as soon as the final
//! text and queued output finish, a fresh session starts.

use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio::{Capture, SAMPLE_RATE};
use crate::config::{Config, Paths};
use crate::inject::Injector;
use crate::ipc;
use crate::notify::notify;
use crate::polish::Polisher;
use crate::provider::{self, AudioSink, EventSource, SessionConfig, TranscriptEvent};

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the capture task to die after abort.
const CAPTURE_REAP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Recording,
    Finalizing,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Finalizing => "finalizing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOutcome {
    Success,
    Failure,
}

struct OutputReport {
    injection_succeeded: bool,
}

#[derive(Default)]
struct OutputTracker {
    pending_segments: usize,
    terminal_outcome: Option<TerminalOutcome>,
    dropped_text: bool,
    injection_failed: bool,
}

impl OutputTracker {
    fn segment_enqueued(&mut self) {
        self.pending_segments += 1;
    }

    fn segment_dropped(&mut self) {
        self.dropped_text = true;
    }

    fn segment_finished(&mut self, injection_succeeded: bool) {
        if self.pending_segments == 0 {
            tracing::warn!("ignoring an output report with no pending segment");
            return;
        }
        self.pending_segments -= 1;
        if !injection_succeeded {
            self.injection_failed = true;
        }
    }

    fn mark_terminal(&mut self, outcome: TerminalOutcome) {
        self.terminal_outcome = Some(outcome);
    }

    fn should_finish(&self) -> bool {
        self.terminal_outcome.is_some() && self.pending_segments == 0
    }

    fn completion_message(&self) -> Option<String> {
        if !self.should_finish() || self.terminal_outcome != Some(TerminalOutcome::Success) {
            return None;
        }
        let message = if self.dropped_text {
            "dictation finished (some text was dropped)"
        } else if self.injection_failed {
            "dictation finished (some text failed)"
        } else {
            "dictation finished"
        };
        Some(message.to_string())
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Everything belonging to one live dictation session.
struct Active {
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

pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ipc::Command>(8);
    ipc::spawn_accept_loop(listener, cmd_tx);

    // Final text is corrected and typed by a dedicated task: slow network or
    // injection work must never stall the event loop.
    let (inject_tx, mut inject_rx) = mpsc::channel::<String>(16);
    let (output_report_tx, mut output_report_rx) = mpsc::unbounded_channel::<OutputReport>();
    let backend = crate::inject::autodetect(cfg.inject.backend);
    let polisher = Polisher::from_config(&cfg.polish)?;
    tracing::info!(?backend, "inject backend");
    tokio::spawn(async move {
        let mut injector = Injector::new(backend);
        while let Some(text) = inject_rx.recv().await {
            let original = text;
            // Cloud work is optional; queued originals take priority.
            let corrected = polisher
                .polish_when_idle(original.clone(), inject_rx.is_empty())
                .await;
            let text = injector.compatible_text(&original, &corrected).to_string();
            let injection_succeeded = match injector.type_segment(&text).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "inject failed");
                    notify("voice-input error", &format!("type text: {error:#}"));
                    false
                }
            };
            if output_report_tx
                .send(OutputReport {
                    injection_succeeded,
                })
                .is_err()
            {
                break;
            }
        }
    });

    let mut state = State::Idle;
    let mut active: Option<Active> = None;
    // Fresh per session; the capture task fills it with 100ms PCM chunks.
    let mut pcm_rx: Option<mpsc::Receiver<Vec<i16>>> = None;
    let mut pending_restart = false;
    let mut output_tracker = OutputTracker::default();
    let mut finalize_deadline: Option<Pin<Box<tokio::time::Sleep>>> = None;

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                let reply = match cmd {
                    ipc::Command::Status(reply) => {
                        let _ = reply.send(state.label().to_string());
                        continue;
                    }
                    ipc::Command::Toggle(reply) => reply,
                };
                let answer = match state {
                    State::Idle => {
                        match start_session(&cfg, &mut active, &mut pcm_rx).await {
                            Ok(()) => {
                                output_tracker.reset();
                                state = State::Recording;
                                state.label().to_string()
                            }
                            Err(e) => {
                                teardown(&mut active, &mut pcm_rx).await;
                                notify("voice-input error", &format!("{e:#}"));
                                format!("error: {e:#}")
                            }
                        }
                    }
                    State::Recording => {
                        state = State::Finalizing;
                        // Acknowledge the stop before doing any I/O. Recognition
                        // and injection finish asynchronously after this reply.
                        let _ = reply.send(state.label().to_string());
                        let stop = finalize_session(&mut active, &mut pcm_rx);
                        match tokio::time::timeout(FINALIZE_TIMEOUT, stop).await {
                            Ok(Ok(())) => {
                                finalize_deadline =
                                    Some(Box::pin(tokio::time::sleep(FINALIZE_TIMEOUT)));
                            }
                            Ok(Err(e)) => {
                                teardown(&mut active, &mut pcm_rx).await;
                                output_tracker.mark_terminal(TerminalOutcome::Failure);
                                notify("voice-input error", &format!("{e:#}"));
                                finish_session_if_drained(
                                    &cfg,
                                    &mut state,
                                    &mut active,
                                    &mut pcm_rx,
                                    &mut pending_restart,
                                    &mut output_tracker,
                                ).await;
                            }
                            Err(_) => {
                                teardown(&mut active, &mut pcm_rx).await;
                                output_tracker.mark_terminal(TerminalOutcome::Failure);
                                notify("voice-input error", "finalize timed out");
                                finish_session_if_drained(
                                    &cfg,
                                    &mut state,
                                    &mut active,
                                    &mut pcm_rx,
                                    &mut pending_restart,
                                    &mut output_tracker,
                                ).await;
                            }
                        }
                        continue;
                    }
                    State::Finalizing => {
                        // Double-toggle = restart after the final output lands.
                        pending_restart = true;
                        "finalizing (restart queued)".to_string()
                    }
                };
                let _ = reply.send(answer);
            }

            report = output_report_rx.recv() => {
                match report {
                    Some(report) => output_tracker.segment_finished(report.injection_succeeded),
                    None => {
                        anyhow::bail!("output worker stopped unexpectedly");
                    }
                }
                finish_session_if_drained(
                    &cfg,
                    &mut state,
                    &mut active,
                    &mut pcm_rx,
                    &mut pending_restart,
                    &mut output_tracker,
                ).await;
            }

            chunk = async { pcm_rx.as_mut().unwrap().recv().await },
                    if matches!(state, State::Recording) && pcm_rx.is_some() => {
                match chunk {
                    Some(pcm) => {
                        if let Some(a) = active.as_mut()
                            && let Err(e) = a.sink.send_audio(&pcm).await
                        {
                            tracing::warn!(error = %format!("{e:#}"), "send_audio failed");
                            teardown(&mut active, &mut pcm_rx).await;
                            state = State::Finalizing;
                            output_tracker.mark_terminal(TerminalOutcome::Failure);
                            notify("voice-input error", &format!("send audio: {e:#}"));
                            finish_session_if_drained(
                                &cfg,
                                &mut state,
                                &mut active,
                                &mut pcm_rx,
                                &mut pending_restart,
                                &mut output_tracker,
                            ).await;
                        }
                    }
                    // Capture ended on its own (device unplugged, pw-record died).
                    None => {
                        tracing::warn!("capture ended unexpectedly");
                        teardown(&mut active, &mut pcm_rx).await;
                        state = State::Finalizing;
                        output_tracker.mark_terminal(TerminalOutcome::Failure);
                        notify("voice-input error", "microphone capture ended");
                        finish_session_if_drained(
                            &cfg,
                            &mut state,
                            &mut active,
                            &mut pcm_rx,
                            &mut pending_restart,
                            &mut output_tracker,
                        ).await;
                    }
                }
            }

            ev = async { active.as_mut().unwrap().events.next_event().await },
                    if matches!(state, State::Finalizing) && active.is_some() => {
                let terminal_outcome = match ev {
                    Some(TranscriptEvent::Stable(text)) => {
                        // Never block the loop on a stuck injector: drop the
                        // final text and say so, instead of stalling events/IPC.
                        tracing::info!(text, "final transcript");
                        match inject_tx.try_send(text) {
                            Ok(()) => output_tracker.segment_enqueued(),
                            Err(error) => {
                                tracing::warn!(error = %error, "inject queue full or dead");
                                output_tracker.segment_dropped();
                                notify("voice-input error", "dropped text: injector is stuck");
                            }
                        }
                        None
                    }
                    Some(TranscriptEvent::Completed) => Some(TerminalOutcome::Success),
                    Some(TranscriptEvent::Failed(e)) => {
                        tracing::warn!(error = %format!("{e:#}"), "provider failed");
                        notify("voice-input error", &format!("{e:#}"));
                        Some(TerminalOutcome::Failure)
                    }
                    None => {
                        tracing::warn!("event stream ended without Completed");
                        Some(TerminalOutcome::Failure)
                    }
                };
                if let Some(outcome) = terminal_outcome {
                    finalize_deadline = None;
                    teardown(&mut active, &mut pcm_rx).await;
                    state = State::Finalizing;
                    output_tracker.mark_terminal(outcome);
                    finish_session_if_drained(
                        &cfg,
                        &mut state,
                        &mut active,
                        &mut pcm_rx,
                        &mut pending_restart,
                        &mut output_tracker,
                    ).await;
                }
            }

            _ = async { finalize_deadline.as_mut().unwrap().await },
                    if finalize_deadline.is_some() => {
                tracing::warn!("finalize timed out");
                finalize_deadline = None;
                teardown(&mut active, &mut pcm_rx).await;
                state = State::Finalizing;
                output_tracker.mark_terminal(TerminalOutcome::Failure);
                notify("voice-input error", "provider did not finish in time");
                finish_session_if_drained(
                    &cfg,
                    &mut state,
                    &mut active,
                    &mut pcm_rx,
                    &mut pending_restart,
                    &mut output_tracker,
                ).await;
            }
        }
    }
}

async fn finish_session_if_drained(
    cfg: &Config,
    state: &mut State,
    active: &mut Option<Active>,
    pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>,
    pending_restart: &mut bool,
    output_tracker: &mut OutputTracker,
) {
    if !output_tracker.should_finish() {
        return;
    }

    *state = State::Idle;
    if let Some(message) = output_tracker.completion_message() {
        notify("voice-input", &message);
    }
    output_tracker.reset();
    if *pending_restart {
        *pending_restart = false;
        match start_session(cfg, active, pcm_rx).await {
            Ok(()) => {
                *state = State::Recording;
                notify("voice-input", "listening…");
            }
            Err(error) => {
                notify("voice-input error", &format!("{error:#}"));
            }
        }
    }
}

/// Open a provider session and start capturing the microphone.
async fn start_session(
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

/// Stop capturing, drain whatever PCM is still queued (the tail of the
/// utterance lives there), then finalize so the provider flushes it.
async fn finalize_session(
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
async fn teardown(active: &mut Option<Active>, pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>) {
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

    use super::{Active, OutputTracker, TerminalOutcome, finalize_session};
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

        finalize_session(&mut active, &mut pcm_rx).await.unwrap();

        assert_eq!(
            *operations.lock().unwrap(),
            [
                SinkOperation::Audio(vec![1, 2]),
                SinkOperation::Audio(vec![3, 4]),
                SinkOperation::Finalized,
            ]
        );
    }

    #[test]
    fn completion_waits_for_every_queued_segment() {
        let mut tracker = OutputTracker::default();
        tracker.segment_enqueued();
        tracker.segment_enqueued();
        tracker.mark_terminal(TerminalOutcome::Success);
        assert!(!tracker.should_finish());
        tracker.segment_finished(true);
        assert!(!tracker.should_finish());
        tracker.segment_finished(true);
        assert!(tracker.should_finish());
        assert_eq!(
            tracker.completion_message().as_deref(),
            Some("dictation finished")
        );
    }

    #[test]
    fn empty_output_queue_can_complete_immediately() {
        let mut tracker = OutputTracker::default();
        tracker.mark_terminal(TerminalOutcome::Success);
        assert!(tracker.should_finish());
    }

    #[test]
    fn dropped_and_failed_output_is_reported_distinctly() {
        let mut dropped = OutputTracker::default();
        dropped.segment_dropped();
        dropped.mark_terminal(TerminalOutcome::Success);
        assert_eq!(
            dropped.completion_message().as_deref(),
            Some("dictation finished (some text was dropped)")
        );

        let mut failed = OutputTracker::default();
        failed.segment_enqueued();
        failed.mark_terminal(TerminalOutcome::Success);
        failed.segment_finished(false);
        assert_eq!(
            failed.completion_message().as_deref(),
            Some("dictation finished (some text failed)")
        );
    }

    #[test]
    fn provider_failure_does_not_emit_success_or_skip_output_drain() {
        let mut tracker = OutputTracker::default();
        tracker.segment_enqueued();
        tracker.mark_terminal(TerminalOutcome::Failure);
        assert!(!tracker.should_finish());
        assert_eq!(tracker.completion_message(), None);
        tracker.segment_finished(true);
        assert!(tracker.should_finish());
        assert_eq!(tracker.completion_message(), None);
    }
}
