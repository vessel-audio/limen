//! Read-only projection of the pure dictation state.

use serde::{Deserialize, Serialize};

use super::{Control, Dictation, Phase, StartOrigin};

/// The externally observable dictation state and whether another take is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationView {
    /// The current phase and its control mode, when relevant.
    #[serde(flatten)]
    pub state: DictationState,
    /// Whether a toggle requested another take while output was finishing.
    pub queued: bool,
}

/// A dictation phase without actor resources or implementation-only state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DictationState {
    /// No take is active or starting.
    Idle,
    /// The session actor is opening the microphone and provider.
    Starting { control: Control },
    /// Audio is being captured.
    Recording { control: Control },
    /// Capture has stopped and the provider is producing final text.
    Transcribing,
    /// The provider is done and queued input is finishing.
    Committing,
}

impl Dictation {
    /// Project the pure state without changing it or consuming actor resources.
    pub fn view(self) -> DictationView {
        let state = match (self.phase, self.start_origin) {
            (Phase::Idle, None) => DictationState::Idle,
            (Phase::Idle, Some(StartOrigin::Shortcut | StartOrigin::QueuedRestart)) => {
                DictationState::Starting {
                    control: Control::Toggle,
                }
            }
            (Phase::Idle, Some(StartOrigin::Hold(_))) => DictationState::Starting {
                control: Control::Hold,
            },
            (Phase::Recording(control), _) => DictationState::Recording { control },
            (Phase::Finalizing, _) => DictationState::Transcribing,
            (Phase::Draining, _) => DictationState::Committing,
        };
        DictationView {
            state,
            queued: self.pending_restart,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Control, Dictation, DictationState, DictationView, Phase, StartOrigin};

    fn view(state: DictationState) -> DictationView {
        DictationView {
            state,
            queued: false,
        }
    }

    #[test]
    fn every_internal_state_has_a_public_view() {
        let cases = [
            (Dictation::default(), view(DictationState::Idle)),
            (
                Dictation {
                    start_origin: Some(StartOrigin::Shortcut),
                    ..Dictation::default()
                },
                view(DictationState::Starting {
                    control: Control::Toggle,
                }),
            ),
            (
                Dictation {
                    start_origin: Some(StartOrigin::QueuedRestart),
                    ..Dictation::default()
                },
                view(DictationState::Starting {
                    control: Control::Toggle,
                }),
            ),
            (
                Dictation {
                    start_origin: Some(StartOrigin::Hold(super::super::HoldStart::Held)),
                    ..Dictation::default()
                },
                view(DictationState::Starting {
                    control: Control::Hold,
                }),
            ),
            (
                Dictation {
                    phase: Phase::Recording(Control::Toggle),
                    ..Dictation::default()
                },
                view(DictationState::Recording {
                    control: Control::Toggle,
                }),
            ),
            (
                Dictation {
                    phase: Phase::Recording(Control::Hold),
                    ..Dictation::default()
                },
                view(DictationState::Recording {
                    control: Control::Hold,
                }),
            ),
            (
                Dictation {
                    phase: Phase::Finalizing,
                    ..Dictation::default()
                },
                view(DictationState::Transcribing),
            ),
            (
                Dictation {
                    phase: Phase::Draining,
                    ..Dictation::default()
                },
                view(DictationState::Committing),
            ),
        ];

        for (dictation, expected) in cases {
            assert_eq!(dictation.view(), expected);
        }
    }

    #[test]
    fn the_view_exposes_a_queued_restart() {
        let dictation = Dictation {
            phase: Phase::Finalizing,
            pending_restart: true,
            ..Dictation::default()
        };
        assert_eq!(
            dictation.view(),
            DictationView {
                state: DictationState::Transcribing,
                queued: true,
            }
        );
        assert_eq!(
            serde_json::to_value(dictation.view()).unwrap(),
            serde_json::json!({"state": "transcribing", "queued": true})
        );
    }
}
