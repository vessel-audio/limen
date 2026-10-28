//! Pure dictation transitions.
//!
//! Events describe things that happened; effects describe work for the runtime.
//! No microphone, channel, task handle, network client, or clock lives here.
//! In the daemon, session effects become messages to the session actor, whose
//! reports come back as events.
//!
//! A take is controlled either by toggling (press once to start, again to
//! stop) or by holding the hold-to-talk key. The runtime measures how long a
//! hold lasted and enforces [`MAX_HOLD`] through the hold deadline effects.

mod hold;
mod output;

use std::time::Duration;

pub(crate) use hold::{MAX_HOLD, MIN_HOLD};
use output::{OutputTracker, TerminalOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Recording(Control),
    /// Waiting for the session to deliver the final transcript.
    Finalizing,
    /// The session is over; waiting only for queued text to be committed.
    /// Late session reports are ignored so they cannot reopen the take.
    Draining,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Recording(_) => "recording",
            Phase::Finalizing | Phase::Draining => "finalizing",
        }
    }
}

/// Which input ends the take: a second toggle or the release of the hold key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Toggle,
    Hold,
}

impl Control {
    /// `effects` that end a recording, preceded by disarming the deadline
    /// that only a hold recording has.
    fn end_recording(self, effects: impl IntoIterator<Item = Effect>) -> Vec<Effect> {
        match self {
            Control::Toggle => effects.into_iter().collect(),
            Control::Hold => std::iter::once(Effect::DisarmHoldDeadline)
                .chain(effects)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartOrigin {
    Shortcut,
    QueuedRestart,
    Hold(HoldStart),
}

/// Whether the hold key is still down while its take is starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldStart {
    Held,
    /// Released before the take started: it is discarded once it starts.
    ReleasedEarly,
}

/// Result of one attempted text input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputOutcome {
    Succeeded,
    Failed(String),
}

/// External facts that can change a dictation session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Toggle,
    HoldPressed,
    /// `effective`: how long the hold recorded, measured by the runtime from
    /// the start of the recording (zero when it never started).
    HoldReleased {
        effective: Duration,
    },
    /// [`MAX_HOLD`] passed since the hold recording started.
    HoldTimedOut,
    SessionStarted,
    SessionStartFailed(String),
    AudioBufferFailed(String),
    CaptureFailed(String),
    FinalizationStarted,
    StopFailed(String),
    Transcript(String),
    ProviderCompleted,
    ProviderFailed(String),
    ProviderEnded,
    FinalizeTimedOut,
    InputQueued,
    InputDropped,
    InputFinished(InputOutcome),
}

/// User-visible notification requested by a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notification {
    Info(String),
    Error(String),
}

/// Work that the runtime must perform after a pure transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    StartSession,
    StopSession,
    QueueInput(String),
    TeardownSession,
    ArmFinalizeDeadline,
    DisarmFinalizeDeadline,
    /// A hold recording started: start measuring it and arm [`MAX_HOLD`].
    ArmHoldDeadline,
    /// The hold recording ended: stop measuring it and disarm its deadline.
    DisarmHoldDeadline,
    Notify(Notification),
}

/// Complete result of applying one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transition {
    pub(crate) state: Dictation,
    pub(crate) effects: Vec<Effect>,
    pub(crate) reply: Option<String>,
}

/// All business state for the single active dictation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Dictation {
    phase: Phase,
    /// Only a toggle queues the next take; hold-to-talk never does.
    pending_restart: bool,
    start_origin: Option<StartOrigin>,
    output: OutputTracker,
}

impl Dictation {
    pub(crate) fn label(self) -> &'static str {
        self.phase.label()
    }
}

/// Apply an external event without performing any I/O.
pub(crate) fn transition(mut state: Dictation, event: Event) -> Transition {
    match event {
        Event::Toggle => match state.phase {
            Phase::Idle => match state.start_origin {
                // A start is already in flight; answer without starting twice.
                Some(StartOrigin::Shortcut | StartOrigin::QueuedRestart | StartOrigin::Hold(_)) => {
                    Transition {
                        state,
                        effects: Vec::new(),
                        reply: Some("starting".to_string()),
                    }
                }
                None => {
                    state.start_origin = Some(StartOrigin::Shortcut);
                    Transition {
                        state,
                        effects: vec![Effect::StartSession],
                        reply: None,
                    }
                }
            },
            Phase::Recording(control) => {
                state.phase = Phase::Finalizing;
                Transition {
                    state,
                    effects: control.end_recording([Effect::StopSession]),
                    reply: Some("finalizing".to_string()),
                }
            }
            Phase::Finalizing | Phase::Draining => {
                state.pending_restart = true;
                Transition {
                    state,
                    effects: Vec::new(),
                    reply: Some("finalizing (restart queued)".to_string()),
                }
            }
        },
        Event::HoldPressed => hold::pressed(state),
        Event::HoldReleased { effective } => hold::released(state, effective),
        Event::HoldTimedOut => hold::timed_out(state),
        Event::SessionStarted => match state.start_origin.take() {
            // Released before the take could start: discard it.
            Some(StartOrigin::Hold(HoldStart::ReleasedEarly)) => Transition {
                state,
                effects: vec![Effect::TeardownSession],
                reply: None,
            },
            Some(StartOrigin::Shortcut) => start_recording(
                state,
                Control::Toggle,
                Vec::new(),
                Some("recording".to_string()),
            ),
            Some(StartOrigin::QueuedRestart) => start_recording(
                state,
                Control::Toggle,
                vec![Effect::Notify(Notification::Info("listening…".to_string()))],
                None,
            ),
            Some(StartOrigin::Hold(HoldStart::Held)) => {
                start_recording(state, Control::Hold, vec![Effect::ArmHoldDeadline], None)
            }
            None => unchanged(state),
        },
        Event::SessionStartFailed(error) => match state.start_origin {
            None => unchanged(state),
            Some(origin) => {
                state.start_origin = None;
                state.phase = Phase::Idle;
                let reply = match origin {
                    StartOrigin::Shortcut => Some(format!("error: {error}")),
                    StartOrigin::QueuedRestart | StartOrigin::Hold(_) => None,
                };
                Transition {
                    state,
                    effects: vec![
                        Effect::TeardownSession,
                        Effect::Notify(Notification::Error(error)),
                    ],
                    reply,
                }
            }
        },
        Event::AudioBufferFailed(error) => match state.phase {
            Phase::Recording(control) => terminal(
                state,
                TerminalOutcome::Failure,
                control.end_recording([
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(format!("send audio: {error}"))),
                ]),
            ),
            Phase::Idle | Phase::Finalizing | Phase::Draining => unchanged(state),
        },
        Event::CaptureFailed(error) => match state.phase {
            Phase::Recording(control) => terminal(
                state,
                TerminalOutcome::Failure,
                control.end_recording([
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(error)),
                ]),
            ),
            Phase::Idle | Phase::Finalizing | Phase::Draining => unchanged(state),
        },
        Event::FinalizationStarted => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::ArmFinalizeDeadline],
                reply: None,
            },
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::StopFailed(error) => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(error)),
                ],
            ),
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::Transcript(text) => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::QueueInput(text)],
                reply: None,
            },
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::ProviderCompleted => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Success,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::ProviderFailed(error) => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![
                    Effect::DisarmFinalizeDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(error)),
                ],
            ),
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::ProviderEnded => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::FinalizeTimedOut => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![
                    Effect::DisarmFinalizeDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(
                        "provider did not finish in time".to_string(),
                    )),
                ],
            ),
            Phase::Idle | Phase::Recording(_) | Phase::Draining => unchanged(state),
        },
        Event::InputQueued => match state.phase {
            Phase::Finalizing | Phase::Draining => {
                state.output.segment_enqueued();
                finish_if_drained(unchanged(state))
            }
            Phase::Idle | Phase::Recording(_) => unchanged(state),
        },
        Event::InputDropped => match state.phase {
            Phase::Finalizing | Phase::Draining => {
                state.output.segment_dropped();
                finish_if_drained(Transition {
                    state,
                    effects: vec![Effect::Notify(Notification::Error(
                        "dropped text: input method is stuck".to_string(),
                    ))],
                    reply: None,
                })
            }
            Phase::Idle | Phase::Recording(_) => unchanged(state),
        },
        Event::InputFinished(outcome) => match state.phase {
            Phase::Finalizing | Phase::Draining => {
                let effects = match &outcome {
                    InputOutcome::Succeeded => Vec::new(),
                    InputOutcome::Failed(error) => vec![Effect::Notify(Notification::Error(
                        format!("commit text: {error}"),
                    ))],
                };
                state.output.segment_finished(outcome);
                finish_if_drained(Transition {
                    state,
                    effects,
                    reply: None,
                })
            }
            Phase::Idle | Phase::Recording(_) => unchanged(state),
        },
    }
}

fn unchanged(state: Dictation) -> Transition {
    Transition {
        state,
        effects: Vec::new(),
        reply: None,
    }
}

/// The session started a fresh take that `control` will end.
fn start_recording(
    mut state: Dictation,
    control: Control,
    effects: Vec<Effect>,
    reply: Option<String>,
) -> Transition {
    state.phase = Phase::Recording(control);
    state.output.reset();
    Transition {
        state,
        effects,
        reply,
    }
}

fn terminal(mut state: Dictation, outcome: TerminalOutcome, effects: Vec<Effect>) -> Transition {
    state.phase = Phase::Draining;
    state.output.terminal_outcome = Some(outcome);
    finish_if_drained(Transition {
        state,
        effects,
        reply: None,
    })
}

fn finish_if_drained(mut transition: Transition) -> Transition {
    if transition.state.phase != Phase::Draining || !transition.state.output.should_finish() {
        return transition;
    }

    transition.state.phase = Phase::Idle;
    if let Some(message) = transition.state.output.completion_message() {
        transition
            .effects
            .push(Effect::Notify(Notification::Info(message.to_string())));
    }
    transition.state.output.reset();
    if transition.state.pending_restart {
        transition.state.pending_restart = false;
        transition.state.start_origin = Some(StartOrigin::QueuedRestart);
        transition.effects.push(Effect::StartSession);
    }
    transition
}

#[cfg(test)]
mod tests;
