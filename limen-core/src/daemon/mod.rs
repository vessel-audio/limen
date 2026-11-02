//! The resident daemon: translate I/O into events, apply pure dictation
//! transitions, and interpret the resulting effects.
//!
//! Actor model: the dictation loop in [`run`] is an actor that alone owns the
//! `Dictation` state. It handles one message at a time from its mailboxes —
//! IPC commands, session actor reports, text input outcomes, and the finalize
//! and hold deadlines — and never awaits slow work itself. Effects become messages to
//! the actors that own the slow resources:
//! - the session actor (`session`) owns microphone capture and the ASR session;
//! - the text input worker owns the input method and commits final text.
//!
//! Their outcomes come back as events, so the loop stays responsive to IPC
//! even while pw-record is being reaped or a take is being finalized.

mod input;
mod lifecycle;
mod projection;
mod reload;
pub(crate) mod snapshot;
mod spawn;

pub use lifecycle::run;
pub use spawn::{DaemonTermination, daemon_peer_pid, spawn_daemon, terminate_daemon};

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::config::{Config, PolishConfig};
use crate::dictation::{self, Control, Dictation, Effect, Event, InputOutcome, Notification};
use crate::ipc;
use crate::session::{SessionCommand, SessionHandle};
use crate::shortcut::portal::ShortcutHandle;
use crate::shortcut::{Activation, ActivationReceipt, HoldEdge};

use input::{InputJob, InputReport, MAX_OUTSTANDING_INPUT};
use projection::{Completion, Projection};

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type Deadline = Pin<Box<tokio::time::Sleep>>;

/// State of the dictation actor: business state plus the addresses of the
/// actors that perform its effects.
struct DictationActor {
    dictation: Dictation,
    session: SessionHandle,
    /// Mailbox of the text input worker.
    input_tx: mpsc::UnboundedSender<InputJob>,
    /// The `[polish]` section the text input worker polishes with.
    polish: watch::Sender<PolishConfig>,
    finalize_deadline: Option<Deadline>,
    /// Stops a hold recording whose release never arrived.
    hold_deadline: Option<Deadline>,
    /// When the running hold recording started; how long it lasted decides
    /// whether its release keeps the take.
    hold_started: Option<Instant>,
    /// When the hold key went down; `Some` until it is released.
    hold_pressed: Option<Instant>,
    /// IPC caller of a shortcut start, answered once the session actor reports
    /// whether the take started.
    start_caller: Option<oneshot::Sender<String>>,
    /// Shows a desktop notification: `(summary, body)`.
    notify: fn(&str, &str),
    /// Read again when a client asks to apply the `[shortcut]`, `[provider]`, or
    /// `[polish]` section.
    config_file: PathBuf,
    shortcuts: ShortcutHandle,
    outstanding_input: usize,
    projection: Projection,
    delivery: ipc::v1::Delivery,
}

/// Whether the message being handled came from an IPC caller awaiting a reply.
enum ReplyTarget {
    NotRequested,
    Requested(oneshot::Sender<String>),
}

/// The dictation actor's mailboxes; its loop ends when one of them closes.
struct Mailboxes {
    commands: mpsc::Receiver<ipc::Command>,
    input_reports: mpsc::UnboundedReceiver<InputReport>,
    session_reports: mpsc::UnboundedReceiver<Event>,
    activations: mpsc::UnboundedReceiver<ActivationReceipt>,
}

impl DictationActor {
    fn new(
        session: SessionHandle,
        input_tx: mpsc::UnboundedSender<InputJob>,
        polish: watch::Sender<PolishConfig>,
        notify: fn(&str, &str),
        config_file: PathBuf,
        shortcuts: ShortcutHandle,
        projection: Projection,
    ) -> Self {
        Self {
            dictation: Dictation::default(),
            session,
            input_tx,
            polish,
            finalize_deadline: None,
            hold_deadline: None,
            hold_started: None,
            hold_pressed: None,
            start_caller: None,
            notify,
            config_file,
            shortcuts,
            outstanding_input: 0,
            projection,
            delivery: ipc::v1::Delivery::Input,
        }
    }

    /// Handle one message: apply its event and drain any follow-up events
    /// produced by its effects. Never awaits; slow effects are messages to
    /// other actors.
    fn dispatch(&mut self, event: Event, mut reply: ReplyTarget) {
        let previous = self.dictation.view().state;
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let answers_start =
                matches!(event, Event::SessionStarted | Event::SessionStartFailed(_));
            let transcribing =
                self.dictation.view().state == dictation::DictationState::Transcribing;
            let transition = dictation::transition(self.dictation, event);
            self.dictation = transition.state;
            // The transition that ends transcription decides the request's
            // outcome; input-delivery errors from other transitions never do.
            if transcribing
                && self.dictation.view().state != dictation::DictationState::Transcribing
            {
                let failure = transition.effects.iter().find_map(|effect| {
                    let Effect::Notify(Notification::Error(message)) = effect else {
                        return None;
                    };
                    Some(message.as_str())
                });
                self.projection.finish_request(failure, unix_ms());
            }
            // Only the message itself can come from an IPC caller.
            let caller = std::mem::replace(&mut reply, ReplyTarget::NotRequested);
            match (transition.reply, caller) {
                (Some(message), ReplyTarget::Requested(caller)) => {
                    let _ = caller.send(message);
                }
                // A take started by an IPC toggle answers the caller parked for it.
                (Some(message), ReplyTarget::NotRequested) if answers_start => {
                    match self.start_caller.take() {
                        Some(caller) => {
                            let _ = caller.send(message);
                        }
                        None => tracing::debug!(message, "the take was not started over IPC"),
                    }
                }
                // Nobody asked, e.g. a toggle from a global shortcut.
                (Some(message), ReplyTarget::NotRequested) => {
                    tracing::debug!(message, "reply without an IPC caller");
                }
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
                    Effect::QueueInput(text) => {
                        let entry = self.projection.queue(&text, unix_ms());
                        let job = InputJob {
                            entry,
                            delivery: self.delivery,
                            text,
                        };
                        match self.outstanding_input < MAX_OUTSTANDING_INPUT {
                            false => {
                                tracing::warn!("too many outstanding text input jobs");
                                self.projection.drop(entry);
                                events.push_back(Event::InputDropped);
                            }
                            true => match self.input_tx.send(job) {
                                Ok(()) => {
                                    self.outstanding_input += 1;
                                    events.push_back(Event::InputQueued);
                                }
                                Err(error) => {
                                    tracing::warn!(error = %error, "text input worker is gone");
                                    self.projection.drop(entry);
                                    events.push_back(Event::InputDropped);
                                }
                            },
                        }
                    }
                    Effect::ArmFinalizeDeadline => {
                        self.finalize_deadline =
                            Some(Box::pin(tokio::time::sleep(FINALIZE_TIMEOUT)));
                    }
                    Effect::DisarmFinalizeDeadline => {
                        self.finalize_deadline = None;
                    }
                    Effect::ArmHoldDeadline => {
                        self.hold_started = Some(Instant::now());
                        self.hold_deadline =
                            Some(Box::pin(tokio::time::sleep(dictation::MAX_HOLD)));
                    }
                    Effect::DisarmHoldDeadline => {
                        self.hold_started = None;
                        self.hold_deadline = None;
                    }
                    Effect::Notify(Notification::Info(message)) => {
                        (self.notify)("Limen", &message);
                    }
                    Effect::Notify(Notification::Error(message)) => {
                        self.projection.record_error(message.clone(), unix_ms());
                        (self.notify)("Limen error", &message);
                    }
                }
            }
        }
        let view = self.dictation.view();
        if let (true, false) = (
            matches!(view.state, dictation::DictationState::Starting { .. }),
            matches!(previous, dictation::DictationState::Starting { .. }),
        ) {
            self.delivery = ipc::v1::Delivery::Input;
        }
        self.projection.publish(view, unix_ms());
    }

    fn start(&mut self) -> ipc::v1::Reply {
        let view = self.dictation.view();
        match view.state {
            dictation::DictationState::Idle => {
                self.dispatch(Event::Toggle, ReplyTarget::NotRequested);
                ipc::v1::Reply::Accepted {
                    take: self.projection.take(),
                }
            }
            dictation::DictationState::Transcribing | dictation::DictationState::Committing => {
                let take = self.projection.take() + 1;
                match view.queued {
                    true => {}
                    false => self.dispatch(Event::Toggle, ReplyTarget::NotRequested),
                }
                ipc::v1::Reply::Accepted { take }
            }
            dictation::DictationState::Starting { .. }
            | dictation::DictationState::Recording { .. } => ipc::v1::Reply::Error {
                message: "already recording".to_string(),
            },
        }
    }

    fn stop(&mut self, delivery: ipc::v1::Delivery) -> ipc::v1::Reply {
        match self.dictation.view().state {
            dictation::DictationState::Recording { .. } => {
                let take = self.projection.take();
                self.delivery = delivery;
                self.dispatch(Event::Toggle, ReplyTarget::NotRequested);
                ipc::v1::Reply::Accepted { take }
            }
            dictation::DictationState::Idle
            | dictation::DictationState::Starting { .. }
            | dictation::DictationState::Transcribing
            | dictation::DictationState::Committing => ipc::v1::Reply::Error {
                message: "not recording".to_string(),
            },
        }
    }

    fn toggle(&mut self, reply: ReplyTarget) {
        if matches!(
            self.dictation.view().state,
            dictation::DictationState::Recording { .. }
        ) {
            self.delivery = ipc::v1::Delivery::Input;
        }
        self.dispatch(Event::Toggle, reply);
    }

    /// Dispatch an edge of the hold-to-talk key. Nobody waits for the reply.
    fn hold(&mut self, edge: HoldEdge) {
        self.hold_at(edge, Instant::now());
    }

    /// Apply an edge as received at `now`, not when a later mailbox turn
    /// happens to handle it.
    fn hold_at(&mut self, edge: HoldEdge, now: Instant) {
        match (edge, self.hold_pressed) {
            // Key repeat while the key is down. Held across the end of a take,
            // it must not start the next one: that takes a new press.
            (HoldEdge::Press, Some(_)) => {}
            (HoldEdge::Press, None) => {
                self.hold_pressed = Some(now);
                self.dispatch(Event::HoldPressed, ReplyTarget::NotRequested);
            }
            (HoldEdge::Release, pressed) => {
                if matches!(
                    self.dictation.view().state,
                    dictation::DictationState::Recording {
                        control: Control::Hold
                    }
                ) {
                    self.delivery = ipc::v1::Delivery::Input;
                }
                self.hold_pressed = None;
                let effective = effective_hold(self.hold_started, now);
                let held = pressed.map(|pressed| now.saturating_duration_since(pressed));
                tracing::info!(?effective, ?held, "hold key released");
                self.dispatch(Event::HoldReleased { effective }, ReplyTarget::NotRequested);
            }
        }
    }

    /// Apply the `[shortcut]` section of the config again. The whole file must
    /// be valid, though only that section applies without a restart. The
    /// caller hears back once the shortcut actor shows the new state.
    fn rebind(&self, reply: oneshot::Sender<ipc::v1::Reply>) {
        match Config::load(&self.config_file) {
            Ok(config) => {
                let rebound = self.shortcuts.rebind(config.shortcut);
                tokio::spawn(async move {
                    let _ = reply.send(match rebound.await {
                        Ok(()) => ipc::v1::Reply::Ok,
                        Err(_) => ipc::v1::Reply::Error {
                            message: "the shortcut backend stopped".to_string(),
                        },
                    });
                });
            }
            Err(error) => {
                let _ = reply.send(ipc::v1::Reply::Error {
                    message: format!("{error:#}"),
                });
            }
        }
    }

    /// Run all mailboxes and return owned actors for the common cleanup path.
    async fn run(mut self, mut mailboxes: Mailboxes) -> lifecycle::ActorExit {
        let result = self.run_inner(&mut mailboxes).await;
        lifecycle::ActorExit {
            result,
            session: self.session,
            shortcuts: self.shortcuts,
        }
    }

    /// Message loop: handle one message at a time until a mailbox closes.
    async fn run_inner(&mut self, mailboxes: &mut Mailboxes) -> Result<()> {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = terminate.recv() => return Ok(()),
                _ = interrupt.recv() => return Ok(()),
                cmd = mailboxes.commands.recv() => {
                    let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                    match cmd {
                        ipc::Command::Shutdown => return Ok(()),
                        ipc::Command::ClearHistory(reply) => {
                            self.projection.clear_history();
                            self.projection.publish(self.dictation.view(), unix_ms());
                            let _ = reply.send(ipc::v1::Reply::Ok);
                        }
                        ipc::Command::Status(reply) => {
                            let _ = reply.send(self.dictation.label().to_string());
                        }
                        ipc::Command::Toggle(reply) => {
                            self.toggle(ReplyTarget::Requested(reply));
                        }
                        ipc::Command::Start(reply) => {
                            let _ = reply.send(self.start());
                        }
                        ipc::Command::Stop { delivery, reply } => {
                            let _ = reply.send(self.stop(delivery));
                        }
                        ipc::Command::Hold(edge, reply) => {
                            // Answered at once: an edge never waits for the take to start.
                            let _ = reply.send(ipc::v1::Reply::Ok);
                            self.hold(edge);
                        }
                        ipc::Command::ShortcutRebind(reply) => self.rebind(reply),
                        ipc::Command::ShortcutStatus(reply) => {
                            let _ = reply.send(ipc::v1::Reply::ShortcutStatus(self.shortcuts.status()));
                        }
                        ipc::Command::ProviderReload(reply) => {
                            let _ = reply.send(self.reload_provider());
                        }
                        ipc::Command::PolishReload(reply) => {
                            let _ = reply.send(self.reload_polish());
                        }
                    }
                }

                outcome = mailboxes.input_reports.recv() => {
                    match outcome {
                        Some(report) => {
                            let InputReport { entry, delivery, text, outcome } = report;
                            self.outstanding_input = self.outstanding_input.checked_sub(1).expect("input report without an outstanding job");
                            let completion = match (&outcome, delivery) {
                                (InputOutcome::Succeeded, ipc::v1::Delivery::Input) => Completion::Committed,
                                (InputOutcome::Succeeded, ipc::v1::Delivery::Keep) => Completion::Kept,
                                (InputOutcome::Failed(error), _) => Completion::CommitFailed(error.clone()),
                            };
                            self.projection.complete(entry, text, completion);
                            self.dispatch(Event::InputFinished(outcome), ReplyTarget::NotRequested);
                        }
                        None => anyhow::bail!("text input worker stopped unexpectedly"),
                    }
                }

                event = mailboxes.session_reports.recv() => {
                    match event {
                        Some(event) => self.dispatch(event, ReplyTarget::NotRequested),
                        None => anyhow::bail!("session actor stopped unexpectedly"),
                    }
                }

                activation = mailboxes.activations.recv() => {
                    match activation {
                        Some(ActivationReceipt { activation: Activation::Toggle, .. }) => self.toggle(ReplyTarget::NotRequested),
                        Some(ActivationReceipt { activation: Activation::Hold(edge), received_at }) => {
                            self.hold_at(edge, received_at);
                        }
                        None => anyhow::bail!("shortcut actor stopped unexpectedly"),
                    }
                }

                _ = async { self.finalize_deadline.as_mut().unwrap().await },
                        if self.finalize_deadline.is_some() => {
                    tracing::warn!("finalize timed out");
                    self.dispatch(Event::FinalizeTimedOut, ReplyTarget::NotRequested);
                }

                _ = async { self.hold_deadline.as_mut().unwrap().await },
                        if self.hold_deadline.is_some() => {
                    // Keep the pressed marker latched: after a lost release, a repeat
                    // cannot be told from a fresh press. A real release clears it.
                    tracing::warn!("hold exceeded its maximum duration");
                    self.dispatch(Event::HoldTimedOut, ReplyTarget::NotRequested);
                }
            }
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// How long the hold recording has lasted when the key is released at
/// `released`; zero when no hold recording is running.
fn effective_hold(started: Option<Instant>, released: Instant) -> Duration {
    started.map_or(Duration::ZERO, |started| {
        released.saturating_duration_since(started)
    })
}

#[cfg(test)]
mod actor_tests;
#[cfg(test)]
mod tests;
