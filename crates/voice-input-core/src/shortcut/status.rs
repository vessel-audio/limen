//! What the daemon reports about the global shortcuts it binds, for
//! `voice-input shortcut status` and the GUI.

use serde::{Deserialize, Serialize};

use super::{Backend, Shortcut, accelerators_in};

/// The state of the shortcut backend in the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutStatus {
    pub backend: Backend,
    pub session: SessionState,
    /// The keys asked for: the prefill of the config.
    pub preferred: PerAction<Shortcut>,
    /// What the desktop bound, as it describes it.
    pub actual: PerAction<Option<Trigger>>,
    pub last_error: Option<String>,
}

/// Where the portal session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The backend is not the portal.
    Disabled,
    /// Registering or binding, possibly waiting for the user's confirmation.
    Binding,
    Bound,
    /// The user cancelled the confirmation dialog.
    Cancelled,
    Failed,
}

/// One value for each action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PerAction<T> {
    pub toggle: T,
    pub hold: T,
}

/// A binding as the desktop describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// The portal's `trigger_description`, e.g. `Press <Super>z`.
    pub description: String,
    /// The shortcuts named by the description; empty when none can be told.
    pub shortcuts: Vec<Shortcut>,
}

impl Trigger {
    pub fn new(description: String) -> Self {
        let shortcuts = accelerators_in(&description);
        Self {
            description,
            shortcuts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PerAction, SessionState, ShortcutStatus, Trigger};
    use crate::shortcut::{Backend, Shortcut};

    #[test]
    fn status_serializes_as_documented() {
        let status = ShortcutStatus {
            backend: Backend::Portal,
            session: SessionState::Bound,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction {
                toggle: Some(Trigger::new("Press <Super>z".to_string())),
                hold: None,
            },
            last_error: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            json,
            r#"{"backend":"portal","session":"bound","preferred":{"toggle":"Super+Z","hold":"Super+X"},"actual":{"toggle":{"description":"Press <Super>z","shortcuts":["Super+Z"]},"hold":null},"last_error":null}"#
        );
        assert_eq!(
            serde_json::from_str::<ShortcutStatus>(&json).unwrap(),
            status
        );
    }

    #[test]
    fn descriptions_without_accelerators_have_no_shortcuts() {
        assert_eq!(
            Trigger::new("<Super>zni bosing".to_string()),
            Trigger {
                description: "<Super>zni bosing".to_string(),
                shortcuts: Vec::new(),
            }
        );
    }
}
