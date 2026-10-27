//! Pure dictation transitions.
//!
//! Events describe things that happened; effects describe work for the runtime.
//! No microphone, channel, task handle, network client, or clock lives here.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    #[default]
    Idle,
    Recording,
    Finalizing,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Recording => "recording",
            Phase::Finalizing => "finalizing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartOrigin {
    Shortcut,
    QueuedRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOutcome {
    Success,
    Failure,
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
    AudioCaptured(Vec<i16>),
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
    BufferAudio(Vec<i16>),
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

    pub(crate) fn records_audio(self) -> bool {
        matches!(self.phase, Phase::Recording)
    }

    pub(crate) fn accepts_provider_events(self) -> bool {
        matches!(self.phase, Phase::Finalizing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct OutputTracker {
    pending_segments: usize,
    terminal_outcome: Option<TerminalOutcome>,
    dropped_text: bool,
    input_failed: bool,
}

impl OutputTracker {
    fn segment_enqueued(&mut self) {
        self.pending_segments += 1;
    }

    fn segment_dropped(&mut self) {
        self.dropped_text = true;
    }

    fn segment_finished(&mut self, outcome: InputOutcome) {
        if self.pending_segments == 0 {
            return;
        }
        self.pending_segments -= 1;
        if matches!(outcome, InputOutcome::Failed(_)) {
            self.input_failed = true;
        }
    }

    fn should_finish(self) -> bool {
        self.terminal_outcome.is_some() && self.pending_segments == 0
    }

    fn completion_message(self) -> Option<&'static str> {
        match self.terminal_outcome {
            Some(TerminalOutcome::Success) if self.pending_segments == 0 => {
                match (self.dropped_text, self.input_failed) {
                    (true, true) | (true, false) => {
                        Some("dictation finished (some text was dropped)")
                    }
                    (false, true) => Some("dictation finished (some text failed)"),
                    (false, false) => Some("dictation finished"),
                }
            }
            Some(TerminalOutcome::Success) | Some(TerminalOutcome::Failure) | None => None,
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Apply an external event without performing any I/O.
pub(crate) fn transition(mut state: Dictation, event: Event) -> Transition {
    match event {
        Event::Toggle => match state.phase {
            Phase::Idle => match state.start_origin {
                Some(StartOrigin::Shortcut | StartOrigin::QueuedRestart) => unchanged(state),
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
            Phase::Finalizing => {
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
        Event::AudioCaptured(pcm) => match state.phase {
            Phase::Recording => Transition {
                state,
                effects: vec![Effect::BufferAudio(pcm)],
                reply: None,
            },
            Phase::Idle | Phase::Finalizing => unchanged(state),
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
            Phase::Idle | Phase::Finalizing => unchanged(state),
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
            Phase::Idle | Phase::Finalizing => unchanged(state),
        },
        Event::FinalizationStarted => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::ArmFinalizeDeadline],
                reply: None,
            },
            Phase::Idle | Phase::Recording => unchanged(state),
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
            Phase::Idle | Phase::Recording => unchanged(state),
        },
        Event::Transcript(text) => match state.phase {
            Phase::Finalizing => Transition {
                state,
                effects: vec![Effect::QueueInput(text)],
                reply: None,
            },
            Phase::Idle | Phase::Recording => unchanged(state),
        },
        Event::ProviderCompleted => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Success,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording => unchanged(state),
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
            Phase::Idle | Phase::Recording => unchanged(state),
        },
        Event::ProviderEnded => match state.phase {
            Phase::Finalizing => terminal(
                state,
                TerminalOutcome::Failure,
                vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
            ),
            Phase::Idle | Phase::Recording => unchanged(state),
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
            Phase::Idle | Phase::Recording => unchanged(state),
        },
        Event::InputQueued => match state.phase {
            Phase::Finalizing => {
                state.output.segment_enqueued();
                finish_if_drained(unchanged(state))
            }
            Phase::Idle | Phase::Recording => unchanged(state),
        },
        Event::InputDropped => match state.phase {
            Phase::Finalizing => {
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
            Phase::Finalizing => {
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
    state.phase = Phase::Finalizing;
    state.output.terminal_outcome = Some(outcome);
    finish_if_drained(Transition {
        state,
        effects,
        reply: None,
    })
}

fn finish_if_drained(mut transition: Transition) -> Transition {
    if transition.state.phase != Phase::Finalizing || !transition.state.output.should_finish() {
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
mod tests {
    use super::{
        Dictation, Effect, Event, InputOutcome, Notification, OutputTracker, Phase, StartOrigin,
        TerminalOutcome, Transition, transition,
    };

    fn shortcut_start() -> Dictation {
        Dictation {
            start_origin: Some(StartOrigin::Shortcut),
            ..Dictation::default()
        }
    }

    fn recording() -> Dictation {
        Dictation {
            phase: Phase::Recording,
            ..Dictation::default()
        }
    }

    fn finalizing() -> Dictation {
        Dictation {
            phase: Phase::Finalizing,
            ..Dictation::default()
        }
    }

    fn pending_input() -> Dictation {
        Dictation {
            output: OutputTracker {
                pending_segments: 1,
                ..OutputTracker::default()
            },
            ..finalizing()
        }
    }

    fn completed_input() -> Dictation {
        Dictation {
            output: OutputTracker {
                pending_segments: 1,
                terminal_outcome: Some(TerminalOutcome::Success),
                ..OutputTracker::default()
            },
            ..pending_input()
        }
    }

    #[test]
    fn shortcut_start_answers_only_after_session_starts() {
        assert_eq!(
            transition(Dictation::default(), Event::Toggle),
            Transition {
                state: Dictation {
                    start_origin: Some(StartOrigin::Shortcut),
                    ..Dictation::default()
                },
                effects: vec![Effect::StartSession],
                reply: None,
            }
        );
        assert_eq!(
            transition(shortcut_start(), Event::SessionStarted),
            Transition {
                state: recording(),
                effects: Vec::new(),
                reply: Some("recording".to_string()),
            }
        );
    }

    #[test]
    fn start_failure_replies_and_cleans_up() {
        assert_eq!(
            transition(
                shortcut_start(),
                Event::SessionStartFailed("offline".to_string())
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("offline".to_string())),
                ],
                reply: Some("error: offline".to_string()),
            }
        );
    }

    #[test]
    fn stop_acknowledges_first_and_successful_stop_arms_deadline() {
        assert_eq!(
            transition(recording(), Event::Toggle),
            Transition {
                state: finalizing(),
                effects: vec![Effect::StopSession],
                reply: Some("finalizing".to_string()),
            }
        );
        assert_eq!(
            transition(finalizing(), Event::FinalizationStarted),
            Transition {
                state: finalizing(),
                effects: vec![Effect::ArmFinalizeDeadline],
                reply: None,
            }
        );
    }

    #[test]
    fn recording_buffers_audio_without_accepting_transcripts() {
        assert_eq!(
            transition(recording(), Event::AudioCaptured(vec![1, 2])),
            Transition {
                state: recording(),
                effects: vec![Effect::BufferAudio(vec![1, 2])],
                reply: None,
            }
        );
        assert_eq!(
            transition(recording(), Event::Transcript("early".to_string())),
            Transition {
                state: recording(),
                effects: Vec::new(),
                reply: None,
            }
        );
    }

    #[test]
    fn final_text_waits_for_input_drain_before_completion() {
        assert_eq!(
            transition(finalizing(), Event::Transcript("hello".to_string())),
            Transition {
                state: finalizing(),
                effects: vec![Effect::QueueInput("hello".to_string())],
                reply: None,
            }
        );
        assert_eq!(
            transition(finalizing(), Event::InputQueued),
            Transition {
                state: pending_input(),
                effects: Vec::new(),
                reply: None,
            }
        );
        assert_eq!(
            transition(pending_input(), Event::ProviderCompleted),
            Transition {
                state: completed_input(),
                effects: vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
                reply: None,
            }
        );
        assert_eq!(
            transition(
                completed_input(),
                Event::InputFinished(InputOutcome::Succeeded)
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![Effect::Notify(Notification::Info(
                    "dictation finished".to_string()
                ))],
                reply: None,
            }
        );
    }

    #[test]
    fn failed_input_notifies_and_marks_completion_failure() {
        assert_eq!(
            transition(
                completed_input(),
                Event::InputFinished(InputOutcome::Failed("no focus".to_string()))
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::Notify(Notification::Error("commit text: no focus".to_string())),
                    Effect::Notify(Notification::Info(
                        "dictation finished (some text failed)".to_string()
                    )),
                ],
                reply: None,
            }
        );
    }

    #[test]
    fn dropped_input_is_reported_in_final_completion() {
        let dropped = transition(finalizing(), Event::InputDropped);
        assert_eq!(
            dropped,
            Transition {
                state: Dictation {
                    output: OutputTracker {
                        dropped_text: true,
                        ..OutputTracker::default()
                    },
                    ..finalizing()
                },
                effects: vec![Effect::Notify(Notification::Error(
                    "dropped text: input method is stuck".to_string()
                ))],
                reply: None,
            }
        );
        assert_eq!(
            transition(dropped.state, Event::ProviderCompleted),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::DisarmFinalizeDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Info(
                        "dictation finished (some text was dropped)".to_string()
                    )),
                ],
                reply: None,
            }
        );
    }

    #[test]
    fn provider_failure_waits_for_output_before_queued_restart() {
        let queued = transition(pending_input(), Event::Toggle);
        let failed = transition(queued.state, Event::ProviderFailed("HTTP 500".to_string()));
        assert_eq!(
            failed,
            Transition {
                state: Dictation {
                    pending_restart: true,
                    output: OutputTracker {
                        pending_segments: 1,
                        terminal_outcome: Some(TerminalOutcome::Failure),
                        ..OutputTracker::default()
                    },
                    ..finalizing()
                },
                effects: vec![
                    Effect::DisarmFinalizeDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("HTTP 500".to_string())),
                ],
                reply: None,
            }
        );
        assert_eq!(
            transition(failed.state, Event::InputFinished(InputOutcome::Succeeded)),
            Transition {
                state: Dictation {
                    start_origin: Some(StartOrigin::QueuedRestart),
                    ..Dictation::default()
                },
                effects: vec![Effect::StartSession],
                reply: None,
            }
        );
    }

    #[test]
    fn stop_capture_and_deadline_failures_do_not_complete_successfully() {
        assert_eq!(
            transition(finalizing(), Event::StopFailed("capture stuck".to_string())),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("capture stuck".to_string())),
                ],
                reply: None,
            }
        );
        assert_eq!(
            transition(
                recording(),
                Event::CaptureFailed("microphone ended".to_string())
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("microphone ended".to_string())),
                ],
                reply: None,
            }
        );
        assert_eq!(
            transition(
                recording(),
                Event::AudioBufferFailed("sink closed".to_string())
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("send audio: sink closed".to_string())),
                ],
                reply: None,
            }
        );
        assert_eq!(
            transition(finalizing(), Event::FinalizeTimedOut),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::DisarmFinalizeDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error(
                        "provider did not finish in time".to_string()
                    )),
                ],
                reply: None,
            }
        );
    }

    #[test]
    fn finalizing_toggle_restarts_only_after_old_output_drains() {
        let queued = transition(completed_input(), Event::Toggle);
        assert_eq!(
            queued,
            Transition {
                state: Dictation {
                    pending_restart: true,
                    ..completed_input()
                },
                effects: Vec::new(),
                reply: Some("finalizing (restart queued)".to_string()),
            }
        );
        let restart = transition(queued.state, Event::InputFinished(InputOutcome::Succeeded));
        assert_eq!(
            restart,
            Transition {
                state: Dictation {
                    start_origin: Some(StartOrigin::QueuedRestart),
                    ..Dictation::default()
                },
                effects: vec![
                    Effect::Notify(Notification::Info("dictation finished".to_string())),
                    Effect::StartSession,
                ],
                reply: None,
            }
        );
        assert_eq!(
            transition(restart.state, Event::SessionStarted),
            Transition {
                state: recording(),
                effects: vec![Effect::Notify(Notification::Info("listening…".to_string()))],
                reply: None,
            }
        );
        assert_eq!(
            transition(
                restart.state,
                Event::SessionStartFailed("offline".to_string())
            ),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("offline".to_string())),
                ],
                reply: None,
            }
        );
    }

    #[test]
    fn provider_eof_is_failure_without_a_success_notification() {
        assert_eq!(
            transition(finalizing(), Event::ProviderEnded),
            Transition {
                state: Dictation::default(),
                effects: vec![Effect::DisarmFinalizeDeadline, Effect::TeardownSession],
                reply: None,
            }
        );
    }
}
