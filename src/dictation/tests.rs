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

fn draining() -> Dictation {
    Dictation {
        phase: Phase::Draining,
        ..Dictation::default()
    }
}

fn completed_input() -> Dictation {
    Dictation {
        output: OutputTracker {
            pending_segments: 1,
            terminal_outcome: Some(TerminalOutcome::Success),
            ..OutputTracker::default()
        },
        ..draining()
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
fn toggle_while_starting_answers_without_starting_twice() {
    assert_eq!(
        transition(shortcut_start(), Event::Toggle),
        Transition {
            state: shortcut_start(),
            effects: Vec::new(),
            reply: Some("starting".to_string()),
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
fn recording_does_not_accept_transcripts() {
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
                ..draining()
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

#[test]
fn timed_out_take_ignores_late_session_reports_while_draining() {
    let timed_out = transition(pending_input(), Event::FinalizeTimedOut);
    assert_eq!(
        timed_out,
        Transition {
            state: Dictation {
                output: OutputTracker {
                    pending_segments: 1,
                    terminal_outcome: Some(TerminalOutcome::Failure),
                    ..OutputTracker::default()
                },
                ..draining()
            },
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
    for late in [
        Event::FinalizationStarted,
        Event::StopFailed("late".to_string()),
        Event::Transcript("late".to_string()),
        Event::ProviderCompleted,
        Event::ProviderFailed("late".to_string()),
        Event::ProviderEnded,
        Event::FinalizeTimedOut,
    ] {
        assert_eq!(
            transition(timed_out.state, late),
            Transition {
                state: timed_out.state,
                effects: Vec::new(),
                reply: None,
            }
        );
    }
}
