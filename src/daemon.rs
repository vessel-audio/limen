//! The resident daemon: translate I/O into events, apply pure dictation
//! transitions, and interpret the resulting effects.
//!
//! Actor model: the dictation loop in [`run`] is an actor that alone owns the
//! `Dictation` state. It handles one message at a time from its mailboxes —
//! IPC commands, session actor reports, text input outcomes, and the finalize
//! deadline — and never awaits slow work itself. Effects become messages to
//! the actors that own the slow resources:
//! - the session actor (`session`) owns microphone capture and the ASR session;
//! - the text input worker owns the input method and commits final text.
//!
//! Their outcomes come back as events, so the loop stays responsive to IPC
//! even while pw-record is being reaped or a take is being finalized.

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
use crate::session::{SessionCommand, SessionHandle};

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type Deadline = Pin<Box<tokio::time::Sleep>>;

/// State of the dictation actor: business state plus the addresses of the
/// actors that perform its effects.
struct DictationActor {
    dictation: Dictation,
    session: SessionHandle,
    /// Mailbox of the text input worker.
    input_tx: mpsc::Sender<String>,
    finalize_deadline: Option<Deadline>,
    /// IPC caller of a shortcut start, answered once the session actor reports
    /// whether the take started.
    start_caller: Option<oneshot::Sender<String>>,
}

/// Whether the message being handled came from an IPC caller awaiting a reply.
enum ReplyTarget {
    NotRequested,
    Requested(oneshot::Sender<String>),
}

impl DictationActor {
    /// Handle one message: apply its event and drain any follow-up events
    /// produced by its effects. Never awaits; slow effects are messages to
    /// other actors.
    fn dispatch(&mut self, event: Event, mut reply: ReplyTarget) {
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let transition = dictation::transition(self.dictation, event);
            self.dictation = transition.state;
            // Only the message itself can come from an IPC caller.
            let caller = std::mem::replace(&mut reply, ReplyTarget::NotRequested);
            match (transition.reply, caller) {
                (Some(message), ReplyTarget::Requested(caller)) => {
                    let _ = caller.send(message);
                }
                (Some(message), ReplyTarget::NotRequested) => match self.start_caller.take() {
                    Some(caller) => {
                        let _ = caller.send(message);
                    }
                    None => tracing::warn!(message, "transition reply had no IPC target"),
                },
                // Only a shortcut start defers its answer: the core replies once
                // the session actor reports whether the take started.
                (None, ReplyTarget::Requested(caller)) => {
                    self.start_caller = Some(caller);
                }
                (None, ReplyTarget::NotRequested) => {}
            }

            for effect in transition.effects {
                match effect {
                    Effect::StartSession => self.session.tell(SessionCommand::Start),
                    Effect::StopSession => self.session.tell(SessionCommand::Stop),
                    Effect::TeardownSession => self.session.tell(SessionCommand::Teardown),
                    Effect::QueueInput(text) => match self.input_tx.try_send(text) {
                        Ok(()) => events.push_back(Event::InputQueued),
                        Err(error) => {
                            tracing::warn!(error = %error, "text input queue full or dead");
                            events.push_back(Event::InputDropped);
                        }
                    },
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

    // Text input worker: an actor that owns the input method and commits one
    // queued text at a time, so slow polishing or text input never stalls the
    // dictation loop.
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

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (session_report_tx, mut session_report_rx) = mpsc::unbounded_channel::<Event>();
    let mut actor = DictationActor {
        dictation: Dictation::default(),
        session: SessionHandle::spawn(cfg, session_report_tx),
        input_tx,
        finalize_deadline: None,
        start_caller: None,
    };

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                match cmd {
                    ipc::Command::Status(reply) => {
                        let _ = reply.send(actor.dictation.label().to_string());
                    }
                    ipc::Command::Toggle(reply) => {
                        actor.dispatch(Event::Toggle, ReplyTarget::Requested(reply));
                    }
                }
            }

            outcome = input_report_rx.recv() => {
                match outcome {
                    Some(outcome) => {
                        actor.dispatch(Event::InputFinished(outcome), ReplyTarget::NotRequested);
                    }
                    None => anyhow::bail!("text input worker stopped unexpectedly"),
                }
            }

            event = session_report_rx.recv() => {
                match event {
                    Some(event) => actor.dispatch(event, ReplyTarget::NotRequested),
                    None => anyhow::bail!("session actor stopped unexpectedly"),
                }
            }

            _ = async { actor.finalize_deadline.as_mut().unwrap().await },
                    if actor.finalize_deadline.is_some() => {
                tracing::warn!("finalize timed out");
                actor.dispatch(Event::FinalizeTimedOut, ReplyTarget::NotRequested);
            }
        }
    }
}
