use std::time::Duration;

use super::hold::MIN_HOLD;
use super::{
    Control, Dictation, Effect, Event, HoldStart, InputOutcome, Notification, OutputTracker, Phase,
    StartOrigin, TerminalOutcome, Transition, transition,
};

fn shortcut_start() -> Dictation {
    Dictation {
        start_origin: Some(StartOrigin::Shortcut),
        ..Dictation::default()
    }
}

fn recording() -> Dictation {
    Dictation {
        phase: Phase::Recording(Control::Toggle),
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

fn hold_start(start: HoldStart) -> Dictation {
    Dictation {
        start_origin: Some(StartOrigin::Hold(start)),
        ..Dictation::default()
    }
}

fn hold_recording() -> Dictation {
    Dictation {
        phase: Phase::Recording(Control::Hold),
        ..Dictation::default()
    }
}

fn unchanged(state: Dictation) -> Transition {
    Transition {
        state,
        effects: Vec::new(),
        reply: None,
    }
}

/// A release after a hold long enough to keep the take.
const KEPT: Duration = Duration::from_secs(2);

/// A release after an accidental tap.
const TAP: Duration = MIN_HOLD.checked_sub(Duration::from_millis(1)).unwrap();

/// Apply `events` in order: the final state, plus every effect and reply on
/// the way.
fn run(
    state: Dictation,
    events: impl IntoIterator<Item = Event>,
) -> (Dictation, Vec<Effect>, Vec<String>) {
    events.into_iter().fold(
        (state, Vec::new(), Vec::new()),
        |(state, mut effects, mut replies), event| {
            let transition = transition(state, event);
            effects.extend(transition.effects);
            replies.extend(transition.reply);
            (transition.state, effects, replies)
        },
    )
}

#[test]
fn hold_press_starts_a_take_that_arms_the_hold_deadline() {
    assert_eq!(
        transition(Dictation::default(), Event::HoldPressed),
        Transition {
            state: hold_start(HoldStart::Held),
            effects: vec![Effect::StartSession],
            reply: None,
        }
    );
    assert_eq!(
        transition(hold_start(HoldStart::Held), Event::SessionStarted),
        Transition {
            state: hold_recording(),
            effects: vec![Effect::ArmHoldDeadline],
            reply: None,
        }
    );
}

#[test]
fn release_before_the_take_starts_discards_it() {
    let released = transition(
        hold_start(HoldStart::Held),
        Event::HoldReleased {
            effective: Duration::ZERO,
        },
    );
    assert_eq!(released, unchanged(hold_start(HoldStart::ReleasedEarly)));
    assert_eq!(
        transition(released.state, Event::SessionStarted),
        Transition {
            state: Dictation::default(),
            effects: vec![Effect::TeardownSession],
            reply: None,
        }
    );
}

#[test]
fn release_after_the_minimum_hold_stops_like_a_toggle() {
    assert_eq!(
        transition(
            hold_recording(),
            Event::HoldReleased {
                effective: MIN_HOLD
            }
        ),
        Transition {
            state: finalizing(),
            effects: vec![Effect::DisarmHoldDeadline, Effect::StopSession],
            reply: None,
        }
    );
}

#[test]
fn accidental_tap_discards_the_take_without_a_notification() {
    assert_eq!(
        transition(hold_recording(), Event::HoldReleased { effective: TAP }),
        Transition {
            state: Dictation::default(),
            effects: vec![Effect::DisarmHoldDeadline, Effect::TeardownSession],
            reply: None,
        }
    );
}

#[test]
fn hold_start_failure_notifies_without_a_reply() {
    for start in [HoldStart::Held, HoldStart::ReleasedEarly] {
        assert_eq!(
            transition(
                hold_start(start),
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
}

#[test]
fn hold_edges_are_ignored_where_they_do_not_apply() {
    let queued_start = Dictation {
        start_origin: Some(StartOrigin::QueuedRestart),
        ..Dictation::default()
    };
    let states = [
        (recording(), Event::HoldPressed),
        (recording(), Event::HoldReleased { effective: KEPT }),
        (hold_recording(), Event::HoldPressed),
        (shortcut_start(), Event::HoldPressed),
        (shortcut_start(), Event::HoldReleased { effective: KEPT }),
        (queued_start, Event::HoldPressed),
        (queued_start, Event::HoldReleased { effective: KEPT }),
        (hold_start(HoldStart::Held), Event::HoldPressed),
        (hold_start(HoldStart::ReleasedEarly), Event::HoldPressed),
        (
            hold_start(HoldStart::ReleasedEarly),
            Event::HoldReleased { effective: KEPT },
        ),
        (
            Dictation::default(),
            Event::HoldReleased { effective: KEPT },
        ),
    ];
    for (state, event) in states {
        assert_eq!(
            transition(state, event.clone()),
            unchanged(state),
            "{state:?} {event:?}"
        );
    }
}

#[test]
fn hold_press_while_finalizing_does_not_queue_a_take() {
    for state in [finalizing(), pending_input(), completed_input()] {
        assert_eq!(transition(state, Event::HoldPressed), unchanged(state));
    }
}

#[test]
fn toggle_stops_a_hold_take_and_the_later_release_is_ignored() {
    assert_eq!(
        run(
            hold_recording(),
            [Event::Toggle, Event::HoldReleased { effective: KEPT }]
        ),
        (
            finalizing(),
            vec![Effect::DisarmHoldDeadline, Effect::StopSession],
            vec!["finalizing".to_string()],
        )
    );
}

#[test]
fn toggle_while_a_hold_take_starts_answers_starting() {
    assert_eq!(
        transition(hold_start(HoldStart::Held), Event::Toggle),
        Transition {
            state: hold_start(HoldStart::Held),
            effects: Vec::new(),
            reply: Some("starting".to_string()),
        }
    );
}

#[test]
fn holding_through_finalizing_never_starts_a_take() {
    assert_eq!(
        run(
            finalizing(),
            [
                Event::HoldPressed,
                Event::ProviderCompleted,
                Event::HoldReleased { effective: KEPT },
            ]
        ),
        (
            Dictation::default(),
            vec![
                Effect::DisarmFinalizeDeadline,
                Effect::TeardownSession,
                Effect::Notify(Notification::Info("dictation finished".to_string())),
            ],
            Vec::new(),
        )
    );
}

#[test]
fn only_a_hold_recording_arms_the_hold_deadline() {
    assert_eq!(
        run(
            Dictation::default(),
            [Event::HoldPressed, Event::SessionStarted]
        ),
        (
            hold_recording(),
            vec![Effect::StartSession, Effect::ArmHoldDeadline],
            Vec::new(),
        )
    );
    assert_eq!(
        run(Dictation::default(), [Event::Toggle, Event::SessionStarted]),
        (
            recording(),
            vec![Effect::StartSession],
            vec!["recording".to_string()],
        )
    );
}

#[test]
fn every_way_out_of_a_hold_recording_disarms_its_deadline_once() {
    let cases = [
        (
            Event::HoldReleased {
                effective: MIN_HOLD,
            },
            Transition {
                state: finalizing(),
                effects: vec![Effect::DisarmHoldDeadline, Effect::StopSession],
                reply: None,
            },
        ),
        (
            Event::HoldReleased { effective: TAP },
            Transition {
                state: Dictation::default(),
                effects: vec![Effect::DisarmHoldDeadline, Effect::TeardownSession],
                reply: None,
            },
        ),
        (
            Event::Toggle,
            Transition {
                state: finalizing(),
                effects: vec![Effect::DisarmHoldDeadline, Effect::StopSession],
                reply: Some("finalizing".to_string()),
            },
        ),
        (
            Event::CaptureFailed("microphone ended".to_string()),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::DisarmHoldDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("microphone ended".to_string())),
                ],
                reply: None,
            },
        ),
        (
            Event::AudioBufferFailed("sink closed".to_string()),
            Transition {
                state: Dictation::default(),
                effects: vec![
                    Effect::DisarmHoldDeadline,
                    Effect::TeardownSession,
                    Effect::Notify(Notification::Error("send audio: sink closed".to_string())),
                ],
                reply: None,
            },
        ),
        (
            Event::HoldTimedOut,
            Transition {
                state: finalizing(),
                effects: vec![
                    Effect::DisarmHoldDeadline,
                    Effect::StopSession,
                    Effect::Notify(Notification::Error(
                        "hold exceeded 60s; recording stopped".to_string(),
                    )),
                ],
                reply: None,
            },
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(
            transition(hold_recording(), event.clone()),
            expected,
            "{event:?}"
        );
    }
}

#[test]
fn takes_that_never_record_never_touch_the_hold_deadline() {
    assert_eq!(
        run(
            Dictation::default(),
            [
                Event::HoldPressed,
                Event::SessionStartFailed("offline".to_string())
            ]
        ),
        (
            Dictation::default(),
            vec![
                Effect::StartSession,
                Effect::TeardownSession,
                Effect::Notify(Notification::Error("offline".to_string())),
            ],
            Vec::new(),
        )
    );
    assert_eq!(
        run(
            Dictation::default(),
            [
                Event::HoldPressed,
                Event::HoldReleased {
                    effective: Duration::ZERO
                },
                Event::SessionStarted,
            ]
        ),
        (
            Dictation::default(),
            vec![Effect::StartSession, Effect::TeardownSession],
            Vec::new(),
        )
    );
}

#[test]
fn release_and_timeout_stop_the_take_once_in_either_order() {
    let timeout_notice = Effect::Notify(Notification::Error(
        "hold exceeded 60s; recording stopped".to_string(),
    ));
    assert_eq!(
        run(
            hold_recording(),
            [Event::HoldReleased { effective: KEPT }, Event::HoldTimedOut]
        ),
        (
            finalizing(),
            vec![Effect::DisarmHoldDeadline, Effect::StopSession],
            Vec::new(),
        )
    );
    assert_eq!(
        run(
            hold_recording(),
            [Event::HoldTimedOut, Event::HoldReleased { effective: KEPT }]
        ),
        (
            finalizing(),
            vec![
                Effect::DisarmHoldDeadline,
                Effect::StopSession,
                timeout_notice
            ],
            Vec::new(),
        )
    );
}

#[test]
fn hold_timeout_is_ignored_outside_a_hold_recording() {
    let states = [
        Dictation::default(),
        hold_start(HoldStart::Held),
        hold_start(HoldStart::ReleasedEarly),
        shortcut_start(),
        recording(),
        finalizing(),
        pending_input(),
        draining(),
        completed_input(),
    ];
    for state in states {
        assert_eq!(
            transition(state, Event::HoldTimedOut),
            unchanged(state),
            "{state:?}"
        );
    }
}

#[test]
fn a_whole_hold_take_disarms_once_and_never_replies() {
    assert_eq!(
        run(
            Dictation::default(),
            [
                Event::HoldPressed,
                Event::SessionStarted,
                Event::HoldReleased { effective: KEPT },
                Event::FinalizationStarted,
                Event::Transcript("hello".to_string()),
                Event::InputQueued,
                Event::ProviderCompleted,
                Event::InputFinished(InputOutcome::Succeeded),
            ]
        ),
        (
            Dictation::default(),
            vec![
                Effect::StartSession,
                Effect::ArmHoldDeadline,
                Effect::DisarmHoldDeadline,
                Effect::StopSession,
                Effect::ArmFinalizeDeadline,
                Effect::QueueInput("hello".to_string()),
                Effect::DisarmFinalizeDeadline,
                Effect::TeardownSession,
                Effect::Notify(Notification::Info("dictation finished".to_string())),
            ],
            Vec::new(),
        )
    );
}

#[test]
fn hold_edges_leave_a_starting_toggle_take_to_its_caller() {
    let (state, effects, replies) = run(
        shortcut_start(),
        [Event::HoldPressed, Event::HoldReleased { effective: KEPT }],
    );
    assert_eq!(
        (state, effects, replies),
        (shortcut_start(), Vec::new(), Vec::new())
    );
    assert_eq!(
        transition(state, Event::SessionStarted),
        Transition {
            state: recording(),
            effects: Vec::new(),
            reply: Some("recording".to_string()),
        }
    );
}

#[test]
fn late_reports_after_a_discarded_tap_are_ignored() {
    let (tapped, _, _) = run(hold_recording(), [Event::HoldReleased { effective: TAP }]);
    assert_eq!(tapped, Dictation::default());
    for late in [
        Event::CaptureFailed("late".to_string()),
        Event::ProviderFailed("late".to_string()),
        Event::SessionStarted,
    ] {
        assert_eq!(
            transition(tapped, late.clone()),
            unchanged(tapped),
            "{late:?}"
        );
    }
}
