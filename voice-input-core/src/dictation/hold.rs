//! Hold-to-talk: what the hold key's edges and the hold deadline do.

use std::time::Duration;

use super::{
    Control, Dictation, Effect, HoldStart, Notification, Phase, StartOrigin, Transition, unchanged,
};

/// Holds shorter than this are accidental taps: the take is discarded.
pub const MIN_HOLD: Duration = Duration::from_millis(300);

/// A hold recording stops after this long even if no release arrives, so a
/// lost release signal cannot record forever.
pub(crate) const MAX_HOLD: Duration = Duration::from_secs(60);

pub(super) fn pressed(mut state: Dictation) -> Transition {
    match (state.phase, state.start_origin) {
        (Phase::Idle, None) => {
            state.start_origin = Some(StartOrigin::Hold(HoldStart::Held));
            Transition {
                state,
                effects: vec![Effect::StartSession],
                reply: None,
            }
        }
        // Key repeat, a take already starting, or one being finalized:
        // pressing the hold key never queues a take.
        (Phase::Idle, Some(_)) | (Phase::Recording(_) | Phase::Finalizing | Phase::Draining, _) => {
            unchanged(state)
        }
    }
}

/// `effective`: how long the hold recorded.
pub(super) fn released(mut state: Dictation, effective: Duration) -> Transition {
    match (state.phase, state.start_origin) {
        (Phase::Idle, Some(StartOrigin::Hold(HoldStart::Held))) => {
            state.start_origin = Some(StartOrigin::Hold(HoldStart::ReleasedEarly));
            unchanged(state)
        }
        (Phase::Recording(Control::Hold), _) => match effective < MIN_HOLD {
            true => {
                state.phase = Phase::Idle;
                Transition {
                    state,
                    effects: vec![Effect::DisarmHoldDeadline, Effect::TeardownSession],
                    reply: None,
                }
            }
            false => {
                state.phase = Phase::Finalizing;
                Transition {
                    state,
                    effects: vec![Effect::DisarmHoldDeadline, Effect::StopSession],
                    reply: None,
                }
            }
        },
        (
            Phase::Idle,
            None
            | Some(
                StartOrigin::Shortcut
                | StartOrigin::QueuedRestart
                | StartOrigin::Hold(HoldStart::ReleasedEarly),
            ),
        )
        | (Phase::Recording(Control::Toggle) | Phase::Finalizing | Phase::Draining, _) => {
            unchanged(state)
        }
    }
}

/// [`MAX_HOLD`] passed: stop and transcribe as if the key was released.
pub(super) fn timed_out(mut state: Dictation) -> Transition {
    match state.phase {
        Phase::Recording(Control::Hold) => {
            state.phase = Phase::Finalizing;
            Transition {
                state,
                effects: vec![
                    Effect::DisarmHoldDeadline,
                    Effect::StopSession,
                    Effect::Notify(Notification::Error(format!(
                        "hold exceeded {}s; recording stopped",
                        MAX_HOLD.as_secs()
                    ))),
                ],
                reply: None,
            }
        }
        Phase::Idle | Phase::Recording(Control::Toggle) | Phase::Finalizing | Phase::Draining => {
            unchanged(state)
        }
    }
}
