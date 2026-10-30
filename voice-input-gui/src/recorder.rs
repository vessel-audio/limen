//! Pure GPUI-keystroke to global-shortcut recording transitions.

use gpui_kit::Keystroke;
use voice_input_core::Shortcut;

/// The recorder's current state and the value preserved on failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderState {
    Idle {
        current: Option<Shortcut>,
    },
    Recording {
        previous: Option<Shortcut>,
    },
    Invalid {
        previous: Option<Shortcut>,
        error: RecorderError,
    },
}

/// A key that cannot safely be recorded automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderError {
    ShiftedSymbol,
    Invalid(String),
}

/// User input accepted by the recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Start,
    Key(Keystroke),
    Escape,
    Backspace,
    Manual(String),
}

/// A validated operation for the shared shortcut operation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Set(Shortcut),
    Clear,
}

impl RecorderState {
    /// Apply one recorder event without touching configuration or GPUI.
    pub fn apply(self, event: Event) -> (Self, Option<Action>) {
        let previous = match &self {
            Self::Idle { current } | Self::Recording { previous: current } => *current,
            Self::Invalid { previous, .. } => *previous,
        };
        match event {
            Event::Start => (Self::Recording { previous }, None),
            Event::Escape => (Self::Idle { current: previous }, None),
            Event::Backspace => match self {
                Self::Recording { .. } => (Self::Idle { current: None }, Some(Action::Clear)),
                Self::Idle { .. } | Self::Invalid { .. } => (self, None),
            },
            Event::Manual(text) => match text.parse::<Shortcut>().and_then(Shortcut::validate) {
                Ok(shortcut) => (
                    Self::Idle {
                        current: Some(shortcut),
                    },
                    Some(Action::Set(shortcut)),
                ),
                Err(error) => (
                    Self::Invalid {
                        previous,
                        error: RecorderError::Invalid(error.to_string()),
                    },
                    None,
                ),
            },
            Event::Key(keystroke) => match self {
                Self::Idle { .. } | Self::Invalid { .. } => (self, None),
                Self::Recording { previous } => match map(keystroke) {
                    Ok(Some(shortcut)) => (
                        Self::Idle {
                            current: Some(shortcut),
                        },
                        Some(Action::Set(shortcut)),
                    ),
                    Ok(None) => (Self::Recording { previous }, None),
                    Err(error) => (Self::Invalid { previous, error }, None),
                },
            },
        }
    }
}

fn map(keystroke: Keystroke) -> Result<Option<Shortcut>, RecorderError> {
    let key = keystroke.key.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "shift" | "control" | "ctrl" | "alt" | "super" | "meta" | "logo" | "win" | "cmd"
    ) {
        return Ok(None);
    }
    if keystroke.modifiers.shift
        && key.chars().count() == 1
        && !key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && !matches!(key.as_str(), "space" | "grave")
    {
        return Err(RecorderError::ShiftedSymbol);
    }
    let key = match key.as_str() {
        "space" => "Space".to_string(),
        "`" | "grave" => "Grave".to_string(),
        "-" => "Minus".to_string(),
        "=" => "Equal".to_string(),
        "[" => "BracketLeft".to_string(),
        "]" => "BracketRight".to_string(),
        "\\" => "Backslash".to_string(),
        ";" => "Semicolon".to_string(),
        "'" => "Apostrophe".to_string(),
        "," => "Comma".to_string(),
        "." => "Period".to_string(),
        "/" => "Slash".to_string(),
        _ if key.len() == 1
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric()) =>
        {
            key
        }
        _ if key.strip_prefix('f').is_some_and(|number| {
            !number.is_empty()
                && !number.starts_with('0')
                && number.bytes().all(|byte| byte.is_ascii_digit())
                && number
                    .parse::<u8>()
                    .is_ok_and(|number| (1..=24).contains(&number))
        }) =>
        {
            key.to_ascii_uppercase()
        }
        _ => {
            return Err(RecorderError::Invalid(format!(
                "unsupported key {key}; use manual entry"
            )));
        }
    };
    let mut parts = Vec::new();
    if keystroke.modifiers.shift {
        parts.push("Shift");
    }
    if keystroke.modifiers.control {
        parts.push("Ctrl");
    }
    if keystroke.modifiers.alt {
        parts.push("Alt");
    }
    if keystroke.modifiers.platform {
        parts.push("Super");
    }
    parts.push(&key);
    let text = parts.join("+");
    text.parse::<Shortcut>()
        .and_then(Shortcut::validate)
        .map(Some)
        .map_err(|error| RecorderError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Keystroke, Modifiers};

    use super::{Action, Event, RecorderError, RecorderState};
    use voice_input_core::Shortcut;

    fn key(modifiers: Modifiers, key: &str) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_string(),
            key_char: None,
        }
    }

    fn recording() -> RecorderState {
        RecorderState::Recording {
            previous: Some(Shortcut::DEFAULT_TOGGLE),
        }
    }

    #[test]
    fn common_global_shortcuts_are_recorded() {
        let cases = [
            (
                key(
                    Modifiers {
                        platform: true,
                        ..Modifiers::default()
                    },
                    "z",
                ),
                Shortcut::DEFAULT_TOGGLE,
            ),
            (
                key(
                    Modifiers {
                        control: true,
                        alt: true,
                        ..Modifiers::default()
                    },
                    "w",
                ),
                "Ctrl+Alt+W".parse().unwrap(),
            ),
            (key(Modifiers::default(), "f9"), "F9".parse().unwrap()),
            (
                key(
                    Modifiers {
                        platform: true,
                        ..Modifiers::default()
                    },
                    "space",
                ),
                "Super+Space".parse().unwrap(),
            ),
            (
                key(
                    Modifiers {
                        platform: true,
                        ..Modifiers::default()
                    },
                    "`",
                ),
                "Super+Grave".parse().unwrap(),
            ),
        ];
        for (keystroke, expected) in cases {
            let (state, action) = recording().apply(Event::Key(keystroke));
            assert_eq!(action, Some(Action::Set(expected)));
            assert_eq!(
                state,
                RecorderState::Idle {
                    current: Some(expected),
                }
            );
        }
    }

    #[test]
    fn modifier_only_keys_keep_recording_and_escape_cancels() {
        let (state, action) = recording().apply(Event::Key(key(
            Modifiers {
                platform: true,
                ..Modifiers::default()
            },
            "super",
        )));
        assert_eq!((state, action), (recording(), None));
        let (state, action) = recording().apply(Event::Escape);
        assert_eq!(
            (state, action),
            (
                RecorderState::Idle {
                    current: Some(Shortcut::DEFAULT_TOGGLE),
                },
                None,
            )
        );
    }

    #[test]
    fn backspace_clears_the_previous_shortcut() {
        let (state, action) = recording().apply(Event::Backspace);
        assert_eq!(
            (state, action),
            (RecorderState::Idle { current: None }, Some(Action::Clear))
        );
    }

    #[test]
    fn backspace_outside_recording_does_not_clear_or_block_manual_editing() {
        let (state, action) = RecorderState::Idle {
            current: Some(Shortcut::DEFAULT_TOGGLE),
        }
        .apply(Event::Backspace);
        assert_eq!(action, None);
        assert_eq!(
            state,
            RecorderState::Idle {
                current: Some(Shortcut::DEFAULT_TOGGLE),
            }
        );
    }

    #[test]
    fn shifted_symbols_are_rejected_instead_of_recorded_incorrectly() {
        let (state, action) = recording().apply(Event::Key(key(
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            "!",
        )));
        assert_eq!(action, None);
        assert_eq!(
            state,
            RecorderState::Invalid {
                previous: Some(Shortcut::DEFAULT_TOGGLE),
                error: RecorderError::ShiftedSymbol,
            }
        );
    }

    #[test]
    fn manual_entry_accepts_both_formats_and_keeps_the_old_value_on_error() {
        for text in ["Super+Z", "<Super>z"] {
            let (state, action) = recording().apply(Event::Manual(text.to_string()));
            assert_eq!(action, Some(Action::Set(Shortcut::DEFAULT_TOGGLE)));
            assert_eq!(
                state,
                RecorderState::Idle {
                    current: Some(Shortcut::DEFAULT_TOGGLE),
                }
            );
        }
        let (state, action) = recording().apply(Event::Manual("Shift+A".to_string()));
        assert_eq!(action, None);
        assert!(matches!(state, RecorderState::Invalid { .. }));
    }
}
