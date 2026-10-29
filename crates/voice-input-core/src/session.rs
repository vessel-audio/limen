//! The session actor: sole owner of microphone capture and the ASR session.
//!
//! Actor model:
//! - State: at most one [`Active`] take, reachable only from the actor's own
//!   task, so nothing else can observe a half-updated take.
//! - Mailbox: [`SessionCommand`]s sent through a [`SessionHandle`], handled
//!   strictly one at a time in arrival order.
//! - Reports: the actor never answers a sender directly. Every outcome is told
//!   to the dictation loop as a `dictation::Event`, so the pure dictation core
//!   stays the only place that decides what happens next.
//!
//! Serial handling keeps the invariants cheap: a `Teardown` queued before a
//! `Start` has reaped pw-record before the next one is spawned. Slow work
//! (reaping capture, finalizing a take) blocks only this actor, never the
//! dictation loop that answers IPC.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::audio::{CaptureReap, Level, SAMPLE_RATE, capture_loop, reap_capture};
use crate::config::Config;
use crate::dictation::Event;
use crate::provider::{self, AudioSink, EventSource, SessionConfig, TranscriptEvent};

/// Upper bound for stopping capture and handing the take to the provider.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Messages accepted by the session actor's mailbox.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum SessionCommand {
    /// Open a take; answered with `SessionStarted` or `SessionStartFailed`.
    Start,
    /// Finish recording; answered with `FinalizationStarted` or `StopFailed`,
    /// followed by the provider's results.
    Stop,
    /// Release the take, if any. Never answered.
    Teardown,
}

/// Address of the session actor: the only way to reach its state. Dropping it
/// closes the mailbox, which tears the take down and stops the actor.
pub(crate) struct SessionHandle {
    mailbox: mpsc::UnboundedSender<SessionCommand>,
    task: Option<JoinHandle<()>>,
}

impl SessionHandle {
    /// Spawn the actor. `dictation` is the dictation loop's mailbox, where the
    /// actor reports every outcome.
    pub(crate) fn spawn(
        cfg: Config,
        dictation: mpsc::UnboundedSender<Event>,
    ) -> (Self, watch::Receiver<Level>) {
        let (mailbox, commands) = mpsc::unbounded_channel();
        let (level, levels) = watch::channel(Level::SILENCE);
        let task = tokio::spawn(
            SessionActor {
                cfg,
                commands,
                dictation,
                active: None,
                level,
            }
            .run(),
        );
        (
            Self {
                mailbox,
                task: Some(task),
            },
            levels,
        )
    }

    /// Close the actor mailbox and join the capture owner.
    pub(crate) async fn stop(self) {
        let Self { mailbox, task } = self;
        drop(mailbox);
        if let Some(task) = task {
            crate::task::join_task("session", task, crate::task::StopMode::Graceful).await;
        }
    }

    /// Fire-and-forget send. A dead actor is noticed by the dictation loop
    /// when the actor's reports stop, so a failed send only needs a log line.
    pub(crate) fn tell(&self, command: SessionCommand) {
        if let Err(error) = self.mailbox.send(command) {
            tracing::warn!(command = ?error.0, "session actor is gone");
        }
    }
}

#[cfg(test)]
impl SessionHandle {
    /// A handle whose commands land in `mailbox` instead of a running actor.
    pub(crate) fn from_mailbox(mailbox: mpsc::UnboundedSender<SessionCommand>) -> Self {
        Self {
            mailbox,
            task: None,
        }
    }
}

/// The actor's private state, owned by its task for its whole life.
struct SessionActor {
    cfg: Config,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    /// The dictation loop's mailbox; every outcome is reported here.
    dictation: mpsc::UnboundedSender<Event>,
    active: Option<Active>,
    level: watch::Sender<Level>,
}

impl SessionActor {
    /// Message loop: wait for a command or for input from the live take, and
    /// handle exactly one of them before waiting again.
    async fn run(mut self) {
        loop {
            let command = match self.active.as_mut() {
                None => self.commands.recv().await,
                Some(active) => tokio::select! {
                    command = self.commands.recv() => command,
                    chunk = active.pcm.recv(), if active.stage == Stage::Recording => {
                        if let Some(event) = active.on_capture(chunk).await {
                            self.report(event);
                        }
                        continue;
                    }
                    event = active.events.next_event(), if active.stage == Stage::Finalizing => {
                        let event = active.on_provider(event);
                        self.report(event);
                        continue;
                    }
                },
            };
            match command {
                Some(SessionCommand::Start) => self.start().await,
                Some(SessionCommand::Stop) => self.stop().await,
                Some(SessionCommand::Teardown) => self.teardown().await,
                // Every handle is gone: the daemon is shutting down.
                None => return self.teardown().await,
            }
        }
    }

    /// Tell the dictation loop what happened. If it is gone, the daemon is
    /// shutting down and nobody is left to act on the report.
    fn report(&self, event: Event) {
        let _ = self.dictation.send(event);
    }

    /// `Start`: open a provider session and start capturing the microphone.
    async fn start(&mut self) {
        // A take never outlives the next `Start`, even if a `Teardown` was missed.
        self.teardown().await;
        let event = match Active::open(&self.cfg, self.level.clone()).await {
            Ok(active) => {
                self.active = Some(active);
                Event::SessionStarted
            }
            Err(error) => Event::SessionStartFailed(format!("{error:#}")),
        };
        self.report(event);
    }

    /// `Stop`: end capture and hand the buffered take to the provider, whose
    /// results are then forwarded as they arrive.
    async fn stop(&mut self) {
        let event = match self.active.as_mut() {
            Some(active) => match active.stage {
                Stage::Recording => {
                    match tokio::time::timeout(STOP_TIMEOUT, active.finalize()).await {
                        Ok(Ok(())) => {
                            active.stage = Stage::Finalizing;
                            Event::FinalizationStarted
                        }
                        Ok(Err(error)) => active.end(Event::StopFailed(format!("{error:#}"))),
                        Err(_) => active.end(Event::StopFailed("finalize timed out".to_string())),
                    }
                }
                Stage::Finalizing => {
                    Event::StopFailed("the take is already finalizing".to_string())
                }
                // Capture failed while this stop was in flight; that report
                // reached the core after it had already left Recording.
                Stage::Ended => {
                    Event::StopFailed("recording failed before it could be stopped".to_string())
                }
            },
            None => Event::StopFailed("no take to stop".to_string()),
        };
        self.report(event);
    }

    /// `Teardown`: release the take, waiting until pw-record is dead.
    async fn teardown(&mut self) {
        if let Some(mut active) = self.active.take() {
            let _ = active.reap_capture().await;
        }
    }
}

/// One take: everything the actor owns between `Start` and `Teardown`.
struct Active {
    sink: Box<dyn AudioSink>,
    events: Box<dyn EventSource>,
    pcm: mpsc::Receiver<Vec<i16>>,
    /// `Option` because a JoinHandle panics when polled after completion:
    /// reaping takes it out, so teardown can never poll it twice.
    capture: Option<JoinHandle<()>>,
    stage: Stage,
    level: watch::Sender<Level>,
}

/// Which input the actor reads from the take between commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Buffer microphone audio into the provider.
    Recording,
    /// Forward provider results to the dictation loop.
    Finalizing,
    /// A terminal report was sent; read nothing until `Teardown`.
    Ended,
}

impl Active {
    /// Open a provider session and start capturing the microphone.
    async fn open(cfg: &Config, level: watch::Sender<Level>) -> Result<Self> {
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
        let (tx, pcm) = mpsc::channel(64);
        level.send_replace(Level::SILENCE);
        let capture_level = level.clone();
        let device = cfg.audio.device.clone();
        let chunk_ms = cfg.audio.chunk_ms;
        let capture = tokio::spawn(async move {
            if let Err(e) = capture_loop(device, chunk_ms, tx, capture_level).await {
                tracing::warn!(error = %format!("{e:#}"), "capture loop failed");
            }
        });
        Ok(Self {
            sink,
            events,
            pcm,
            capture: Some(capture),
            stage: Stage::Recording,
            level,
        })
    }

    /// Buffer what the capture task delivered. The end of capture or a refused
    /// chunk ends the take; the returned event reports why.
    async fn on_capture(&mut self, chunk: Option<Vec<i16>>) -> Option<Event> {
        match chunk {
            Some(pcm) => match self.sink.send_audio(&pcm).await {
                Ok(()) => None,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "send_audio failed");
                    let _ = self.reap_capture().await;
                    Some(self.end(Event::AudioBufferFailed(format!("{error:#}"))))
                }
            },
            None => {
                tracing::warn!("capture ended unexpectedly");
                Some(self.end(Event::CaptureFailed("microphone capture ended".to_string())))
            }
        }
    }

    /// Translate one provider result into a dictation event; terminal results
    /// end the take.
    fn on_provider(&mut self, event: Option<TranscriptEvent>) -> Event {
        match event {
            Some(TranscriptEvent::Stable(text)) => {
                tracing::info!(text, "final transcript");
                Event::Transcript(text)
            }
            Some(TranscriptEvent::Completed) => self.end(Event::ProviderCompleted),
            Some(TranscriptEvent::Failed(error)) => {
                tracing::warn!(error = %format!("{error:#}"), "provider failed");
                self.end(Event::ProviderFailed(format!("{error:#}")))
            }
            None => {
                tracing::warn!("event stream ended without Completed");
                self.end(Event::ProviderEnded)
            }
        }
    }

    /// Stop reading from the take; `event` is its terminal report.
    fn end(&mut self, event: Event) -> Event {
        self.stage = Stage::Ended;
        event
    }

    /// Stop capturing, drain whatever PCM is still queued (the tail of the
    /// utterance lives there), then finalize so the provider flushes it.
    async fn finalize(&mut self) -> Result<()> {
        match self.reap_capture().await {
            CaptureReap::Reaped => {}
            CaptureReap::TimedOut => anyhow::bail!("microphone capture did not stop"),
        }
        while let Ok(pcm) = self.pcm.try_recv() {
            self.sink.send_audio(&pcm).await?;
        }
        self.sink.finalize().await
    }

    /// Abort the capture task and wait until pw-record is actually dead
    /// (kill_on_drop runs when the task's `Capture` is dropped).
    async fn reap_capture(&mut self) -> CaptureReap {
        self.level.send_replace(Level::SILENCE);
        let Some(handle) = self.capture.take() else {
            return CaptureReap::Reaped;
        };
        reap_capture(handle, self.level.clone()).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::{mpsc, watch};

    use super::{Active, SessionActor, SessionCommand, Stage};
    use crate::audio::Level;
    use crate::config::Config;
    use crate::dictation::Event;
    use crate::provider::{AudioSink, BoxFuture, EventSource, TranscriptEvent};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

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

    struct FailingSink;

    impl AudioSink for FailingSink {
        fn send_audio<'a>(&'a mut self, _pcm: &'a [i16]) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { anyhow::bail!("provider back-pressure failed") })
        }

        fn finalize(&mut self) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// Yields the scripted provider results, then reports the end of the stream.
    struct ScriptedEvents(VecDeque<TranscriptEvent>);

    impl EventSource for ScriptedEvents {
        fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>> {
            Box::pin(async move { self.0.pop_front() })
        }
    }

    fn recording_take(
        operations: &Arc<Mutex<Vec<SinkOperation>>>,
        pcm: mpsc::Receiver<Vec<i16>>,
        events: impl IntoIterator<Item = TranscriptEvent>,
    ) -> Active {
        Active {
            sink: Box::new(FakeSink {
                operations: Arc::clone(operations),
            }),
            events: Box::new(ScriptedEvents(events.into_iter().collect())),
            pcm,
            capture: None,
            stage: Stage::Recording,
            level: watch::channel(Level::SILENCE).0,
        }
    }

    /// Run an actor that already owns `active`; returns its mailbox and the
    /// receiving end of its reports.
    fn spawn_actor(
        active: Active,
    ) -> (
        mpsc::UnboundedSender<SessionCommand>,
        mpsc::UnboundedReceiver<Event>,
    ) {
        let cfg: Config = toml::from_str(
            r#"
            [provider]
            url = "http://127.0.0.1:8000/v1/audio/transcriptions"
            model = "test-model"
            "#,
        )
        .unwrap();
        let (mailbox, commands) = mpsc::unbounded_channel();
        let (dictation, reports) = mpsc::unbounded_channel();
        tokio::spawn(
            SessionActor {
                cfg,
                commands,
                dictation,
                active: Some(active),
                level: watch::channel(Level::SILENCE).0,
            }
            .run(),
        );
        (mailbox, reports)
    }

    async fn next_report(reports: &mut mpsc::UnboundedReceiver<Event>) -> Option<Event> {
        tokio::time::timeout(TEST_TIMEOUT, reports.recv())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn finalize_drains_queued_audio_before_finalizing() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (tx, pcm) = mpsc::channel(2);
        tx.send(vec![1, 2]).await.unwrap();
        tx.send(vec![3, 4]).await.unwrap();
        drop(tx);
        let mut active = recording_take(&operations, pcm, []);

        active.finalize().await.unwrap();

        assert_eq!(
            *operations.lock().unwrap(),
            [
                SinkOperation::Audio(vec![1, 2]),
                SinkOperation::Audio(vec![3, 4]),
                SinkOperation::Finalized,
            ]
        );
    }

    #[tokio::test]
    async fn stop_forwards_provider_results_until_the_terminal_one() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (tx, pcm) = mpsc::channel(2);
        tx.send(vec![1, 2]).await.unwrap();
        let (mailbox, mut reports) = spawn_actor(recording_take(
            &operations,
            pcm,
            [
                TranscriptEvent::Stable("hello".to_string()),
                TranscriptEvent::Completed,
            ],
        ));

        mailbox.send(SessionCommand::Stop).unwrap();
        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(next_report(&mut reports).await);
        }
        // Closing the mailbox stops the actor; an exhausted provider must not
        // have been read again after `Completed`.
        drop(mailbox);
        received.push(next_report(&mut reports).await);

        assert_eq!(
            received,
            [
                Some(Event::FinalizationStarted),
                Some(Event::Transcript("hello".to_string())),
                Some(Event::ProviderCompleted),
                None,
            ]
        );
        assert_eq!(
            *operations.lock().unwrap(),
            [SinkOperation::Audio(vec![1, 2]), SinkOperation::Finalized]
        );
    }

    #[tokio::test]
    async fn ended_capture_is_reported_once_and_fails_a_later_stop() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (tx, pcm) = mpsc::channel(1);
        drop(tx);
        let (mailbox, mut reports) = spawn_actor(recording_take(&operations, pcm, []));

        let ended = next_report(&mut reports).await;
        mailbox.send(SessionCommand::Stop).unwrap();
        let stopped = next_report(&mut reports).await;
        drop(mailbox);

        assert_eq!(
            [ended, stopped, next_report(&mut reports).await],
            [
                Some(Event::CaptureFailed("microphone capture ended".to_string())),
                Some(Event::StopFailed(
                    "recording failed before it could be stopped".to_string()
                )),
                None,
            ]
        );
        assert_eq!(*operations.lock().unwrap(), []);
    }

    #[tokio::test]
    async fn reaping_capture_resets_the_level() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let (_tx, pcm) = mpsc::channel(1);
        let mut active = recording_take(&operations, pcm, []);
        let (level, levels) = watch::channel(Level::SILENCE);
        active.level = level;
        active.level.send_replace(Level {
            rms: 0.5,
            dbfs: -6.0,
        });

        active.reap_capture().await;

        assert_eq!(*levels.borrow(), Level::SILENCE);
    }

    #[tokio::test]
    async fn a_provider_audio_error_stops_capture_and_resets_level() {
        let (_tx, pcm) = mpsc::channel(1);
        let (level, levels) = watch::channel(Level {
            rms: 0.5,
            dbfs: -6.0,
        });
        let mut active = Active {
            sink: Box::new(FailingSink),
            events: Box::new(ScriptedEvents(VecDeque::new())),
            pcm,
            capture: None,
            stage: Stage::Recording,
            level,
        };

        let event = active.on_capture(Some(vec![1])).await;

        assert_eq!(
            event,
            Some(Event::AudioBufferFailed(
                "provider back-pressure failed".to_string()
            ))
        );
        assert_eq!(*levels.borrow(), Level::SILENCE);
    }
}
