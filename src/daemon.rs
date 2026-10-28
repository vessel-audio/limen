//! The resident daemon: translate I/O into events, apply pure dictation
//! transitions, and interpret the resulting effects.
//!
//! One `select!` loop owns all runtime resources. Text input runs in its own
//! task so a slow input method cannot stall the event loop.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::config::{Config, Paths};
use crate::dictation::{self, Dictation, Effect, Event, InputOutcome, Notification};
use crate::input_method::InputMethod;
use crate::ipc;
use crate::notify::notify;
use crate::polish::Polisher;
use crate::provider::TranscriptEvent;
use crate::session::{self, Active};

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type Deadline = Pin<Box<tokio::time::Sleep>>;

/// Runtime resources used to interpret effects from the pure core.
struct Effects {
    cfg: Config,
    dictation: Dictation,
    active: Option<Active>,
    pcm_rx: Option<mpsc::Receiver<Vec<i16>>>,
    input_tx: mpsc::Sender<String>,
    finalize_deadline: Option<Deadline>,
}

/// A reply is available only while handling an IPC-originated event chain.
enum ReplyTarget {
    NotRequested,
    Requested(oneshot::Sender<String>),
}

impl ReplyTarget {
    fn respond(&mut self, reply: Option<String>) {
        let Some(message) = reply else {
            return;
        };
        match std::mem::replace(self, ReplyTarget::NotRequested) {
            ReplyTarget::Requested(reply) => {
                let _ = reply.send(message);
            }
            ReplyTarget::NotRequested => {
                tracing::warn!(message, "transition reply had no IPC target");
            }
        }
    }
}

impl Effects {
    /// Apply an event and drain any follow-up events produced by its effects.
    async fn dispatch(&mut self, event: Event, mut reply: ReplyTarget) {
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let transition = dictation::transition(self.dictation, event);
            self.dictation = transition.state;
            reply.respond(transition.reply);

            for effect in transition.effects {
                match effect {
                    Effect::StartSession => {
                        let event =
                            match session::start(&self.cfg, &mut self.active, &mut self.pcm_rx)
                                .await
                            {
                                Ok(()) => Event::SessionStarted,
                                Err(error) => Event::SessionStartFailed(format!("{error:#}")),
                            };
                        events.push_back(event);
                    }
                    Effect::BufferAudio(pcm) => {
                        if let Err(error) = session::buffer(&mut self.active, &pcm).await {
                            tracing::warn!(error = %format!("{error:#}"), "send_audio failed");
                            events.push_back(Event::AudioBufferFailed(format!("{error:#}")));
                        }
                    }
                    Effect::StopSession => {
                        let stop = session::finalize(&mut self.active, &mut self.pcm_rx);
                        let event = match tokio::time::timeout(FINALIZE_TIMEOUT, stop).await {
                            Ok(Ok(())) => Event::FinalizationStarted,
                            Ok(Err(error)) => Event::StopFailed(format!("{error:#}")),
                            Err(_) => Event::StopFailed("finalize timed out".to_string()),
                        };
                        events.push_back(event);
                    }
                    Effect::QueueInput(text) => match self.input_tx.try_send(text) {
                        Ok(()) => events.push_back(Event::InputQueued),
                        Err(error) => {
                            tracing::warn!(error = %error, "text input queue full or dead");
                            events.push_back(Event::InputDropped);
                        }
                    },
                    Effect::TeardownSession => {
                        session::teardown(&mut self.active, &mut self.pcm_rx).await;
                    }
                    Effect::ArmFinalizeDeadline => {
                        self.finalize_deadline =
                            Some(Box::pin(tokio::time::sleep(FINALIZE_TIMEOUT)));
                    }
                    Effect::DisarmFinalizeDeadline => {
                        self.finalize_deadline = None;
                    }
                    Effect::Notify(Notification::Info(message)) => {
                        notify("voice-input", &message);
                    }
                    Effect::Notify(Notification::Error(message)) => {
                        notify("voice-input error", &message);
                    }
                }
            }
        }
    }
}

pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ipc::Command>(8);
    ipc::spawn_accept_loop(listener, cmd_tx);

    // Final text is corrected and committed by a dedicated task: slow network
    // or text input work must never stall the event loop.
    let (input_tx, mut input_rx) = mpsc::channel::<String>(16);
    let (input_report_tx, mut input_report_rx) = mpsc::unbounded_channel::<InputOutcome>();
    let backend = crate::input_method::default_backend(cfg.input_method.backend);
    let polisher = Polisher::from_config(&cfg.polish)?;
    tracing::info!(?backend, "input method backend");
    tokio::spawn(async move {
        let mut input_method = InputMethod::new(backend);
        while let Some(text) = input_rx.recv().await {
            let original = text;
            // Cloud work is optional; queued originals take priority.
            let corrected = polisher
                .polish_when_idle(original.clone(), input_rx.is_empty())
                .await;
            let text = input_method
                .compatible_text(&original, &corrected)
                .to_string();
            let outcome = match input_method.commit_text(&text).await {
                Ok(()) => InputOutcome::Succeeded,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "text input failed");
                    InputOutcome::Failed(format!("{error:#}"))
                }
            };
            if input_report_tx.send(outcome).is_err() {
                break;
            }
        }
    });

    let mut effects = Effects {
        cfg,
        dictation: Dictation::default(),
        active: None,
        pcm_rx: None,
        input_tx,
        finalize_deadline: None,
    };

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                match cmd {
                    ipc::Command::Status(reply) => {
                        let _ = reply.send(effects.dictation.label().to_string());
                    }
                    ipc::Command::Toggle(reply) => {
                        effects.dispatch(Event::Toggle, ReplyTarget::Requested(reply)).await;
                    }
                }
            }

            outcome = input_report_rx.recv() => {
                match outcome {
                    Some(outcome) => {
                        effects.dispatch(
                            Event::InputFinished(outcome),
                            ReplyTarget::NotRequested,
                        ).await;
                    }
                    None => anyhow::bail!("text input worker stopped unexpectedly"),
                }
            }

            chunk = async { effects.pcm_rx.as_mut().unwrap().recv().await },
                    if effects.dictation.records_audio() && effects.pcm_rx.is_some() => {
                match chunk {
                    Some(pcm) => {
                        effects.dispatch(
                            Event::AudioCaptured(pcm),
                            ReplyTarget::NotRequested,
                        ).await;
                    }
                    None => {
                        tracing::warn!("capture ended unexpectedly");
                        effects.dispatch(
                            Event::CaptureFailed("microphone capture ended".to_string()),
                            ReplyTarget::NotRequested,
                        ).await;
                    }
                }
            }

            event = async { session::next_event(effects.active.as_mut().unwrap()).await },
                    if effects.dictation.accepts_provider_events() && effects.active.is_some() => {
                match event {
                    Some(TranscriptEvent::Stable(text)) => {
                        tracing::info!(text, "final transcript");
                        effects.dispatch(Event::Transcript(text), ReplyTarget::NotRequested).await;
                    }
                    Some(TranscriptEvent::Completed) => {
                        effects.dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested).await;
                    }
                    Some(TranscriptEvent::Failed(error)) => {
                        tracing::warn!(error = %format!("{error:#}"), "provider failed");
                        effects.dispatch(
                            Event::ProviderFailed(format!("{error:#}")),
                            ReplyTarget::NotRequested,
                        ).await;
                    }
                    None => {
                        tracing::warn!("event stream ended without Completed");
                        effects.dispatch(Event::ProviderEnded, ReplyTarget::NotRequested).await;
                    }
                }
            }

            _ = async { effects.finalize_deadline.as_mut().unwrap().await },
                    if effects.finalize_deadline.is_some() => {
                tracing::warn!("finalize timed out");
                effects.dispatch(Event::FinalizeTimedOut, ReplyTarget::NotRequested).await;
            }
        }
    }
}
