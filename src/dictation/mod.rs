//! Pure dictation transitions.
//!
//! Events describe things that happened; effects describe work for the runtime.
//! No microphone, channel, task handle, network client, or clock lives here.
//! In the daemon, session effects become messages to the session actor, whose
//! reports come back as events.

mod output;

use output::{OutputTracker, TerminalOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Recording,
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
            Phase::Recording => "recording",
            Phase::Finalizing | Phase::Draining => "finalizing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartOrigin {
    Shortcut,
    QueuedRestart,
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
                Some(StartOrigin::Shortcut | StartOrigin::QueuedRestart) => Transition {
                    state,
                    effects: Vec::new(),
                    reply: Some("starting".to_string()),
                },
                None => {
                    state.start_origin = Some(StartOrigin::Shortcut);
                    Transition {
                        state,
                        effects: vec![Effect::StartSession],
                        reply: None,
                    }
                }
            },
            Phase::Recording => {
                state.phase = Phase::Finalizing;
                Transition {
                    state,
                    effects: vec![Effect::StopSession],
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
        Event::SessionStarted => match state.start_origin.take() {
            Some(origin) => {
                state.phase = Phase::Recording;
                state.output.reset();
                let (effects, reply) = match origin {
                    StartOrigin::Shortcut => (Vec::new(), Some("recording".to_string())),
                    StartOrigin::QueuedRestart => (
                        vec![Effect::Notify(Notification::Info("listening…".to_string()))],
                        None,
                    ),
                };
                Transition {
                    state,
                    effects,
                    reply,
                }
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
                    StartOrigin::QueuedRestart => None,
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
            Phase::Recording => terminal(
                state,
                TerminalOutcome::Failure,
                vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(format!("send audio: {error}"))),
                ],
            ),
            Phase::Idle | Phase::Finalizing | Phase::Draining => unchanged(state),
        },
        Event::CaptureFailed(error) => match state.phase {
            Phase::Recording => terminal(
                state,
                TerminalOutcome::Failure,
                vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(error)),
                ],
            ),
            Phase::Idle | Phase::Finalizing | Phase::Draining => unchanged(state),
        },
        Event::FinalizationStarted => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::ArmFinalizeDeadline],
                reply: None,
            },
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
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
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
        },
        Event::Transcript(text) => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::QueueInput(text)],
                reply: None,
            },
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
        },
        Event::ProviderCompleted => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Success,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
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
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
        },
        Event::ProviderEnded => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
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
            Phase::Idle | Phase::Recording | Phase::Draining => unchanged(state),
        },
        Event::InputQueued => match state.phase {
            Phase::Finalizing | Phase::Draining => {
                state.output.segment_enqueued();
                finish_if_drained(unchanged(state))
            }
            Phase::Idle | Phase::Recording => unchanged(state),
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
            Phase::Idle | Phase::Recording => unchanged(state),
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
            Phase::Idle | Phase::Recording => unchanged(state),
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
