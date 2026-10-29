//! The parts of the GlobalShortcuts portal protocol that need no bus: names,
//! request paths, call arguments, and what responses and signals say.

use std::collections::HashMap;

use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use crate::shortcut::status::{PerAction, Trigger};
use crate::shortcut::{Action, Shortcut};

/// The well-known name of the portal.
pub(super) const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub(super) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub(super) const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
pub(super) const GLOBAL_SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
pub(super) const REQUEST: &str = "org.freedesktop.portal.Request";
pub(super) const SESSION: &str = "org.freedesktop.portal.Session";

/// A dictionary of options or results, `a{sv}`.
pub(super) type Vardict = HashMap<String, OwnedVariant>;

/// The shortcuts of a session as the portal lists them, `a(sa{sv})`.
pub(super) type Shortcuts = Vec<(String, Vardict)>;

/// The path of the Request object that a call made with `token` creates.
/// Computing it lets the caller subscribe to its `Response` before calling,
/// so that a response sent right away cannot be missed.
pub(super) fn request_path(unique_name: &str, token: &str) -> String {
    object_path("request", unique_name, token)
}

fn object_path(kind: &str, unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/{kind}/{sender}/{token}")
}

/// What `BindShortcuts` is asked to bind: each action with its description
/// and the key GNOME should suggest.
pub(super) fn bind_argument(
    preferred: PerAction<Shortcut>,
) -> Vec<(&'static str, HashMap<&'static str, Variant<'static>>)> {
    [
        (Action::Toggle, "Start or stop dictation", preferred.toggle),
        (Action::Hold, "Hold to talk", preferred.hold),
    ]
    .map(|(action, description, shortcut)| {
        (
            action.id(),
            HashMap::from([
                ("description", Variant::from(description)),
                (
                    "preferred_trigger",
                    Variant::from(shortcut.portal_trigger()),
                ),
            ]),
        )
    })
    .into()
}

/// How a portal request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome<T> {
    Done(T),
    /// The user dismissed the dialog.
    Cancelled,
    Failed(String),
}

/// The session handle a `CreateSession` response carries. The portal
/// documents it as a string, though it names an object.
pub(super) fn created_session(response: u32, results: &Vardict) -> Outcome<OwnedObjectPath> {
    ended(response, || {
        let handle = results
            .get("session_handle")
            .ok_or("the response carries no session_handle")?;
        String::try_from(handle.clone())
            .ok()
            .and_then(|path| OwnedObjectPath::try_from(path).ok())
            .or_else(|| OwnedObjectPath::try_from(handle.clone()).ok())
            .ok_or_else(|| format!("the session_handle {handle:?} is no object path"))
    })
}

/// The bindings a `BindShortcuts` response carries.
pub(super) fn bound(response: u32, results: &Vardict) -> Outcome<PerAction<Option<Trigger>>> {
    ended(response, || {
        let shortcuts = results
            .get("shortcuts")
            .ok_or("the response carries no shortcuts")?;
        let shortcuts = Shortcuts::try_from(shortcuts.clone())
            .map_err(|_| format!("the shortcuts {shortcuts:?} are not a(sa{{sv}})"))?;
        triggers(&shortcuts)
    })
}

fn ended<T>(response: u32, done: impl FnOnce() -> Result<T, String>) -> Outcome<T> {
    match response {
        0 => match done() {
            Ok(value) => Outcome::Done(value),
            Err(error) => Outcome::Failed(format!("unexpected portal response: {error}")),
        },
        1 => Outcome::Cancelled,
        code => Outcome::Failed(format!("the portal ended the request with code {code}")),
    }
}

/// The binding of each action among `shortcuts`, from a response or a
/// `ShortcutsChanged` signal. Shortcuts of other IDs are ignored. A missing
/// description means unbound; a present one of the wrong type is an error.
pub(super) fn triggers(shortcuts: &Shortcuts) -> Result<PerAction<Option<Trigger>>, String> {
    let trigger = |action: Action| {
        let Some((_, properties)) = shortcuts.iter().find(|(id, _)| id == action.id()) else {
            return Ok(None);
        };
        match properties.get("trigger_description") {
            None => Ok(None),
            Some(description) => String::try_from(description.clone())
                .map(|description| Some(Trigger::new(description)))
                .map_err(|_| {
                    format!(
                        "the {} trigger_description {description:?} is not a string",
                        action.id()
                    )
                }),
        }
    };
    Ok(PerAction {
        toggle: trigger(Action::Toggle)?,
        hold: trigger(Action::Hold)?,
    })
}

/// What a change of the portal's owner means for a session made while
/// `baseline` owned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OwnerChange {
    /// The portal that holds the session went away: the session is gone.
    Restarted,
    /// A portal started, e.g. activated by our first call.
    Appeared(String),
    Unrelated,
}

/// `old` and `new` are the owners `NameOwnerChanged` reports; empty when
/// there was or is none.
pub(super) fn owner_change(baseline: Option<&str>, old: &str, new: &str) -> OwnerChange {
    match (baseline, old.is_empty(), new.is_empty()) {
        (Some(owner), false, _) if owner == old => OwnerChange::Restarted,
        (None, true, false) => OwnerChange::Appeared(new.to_string()),
        (Some(_), _, _) | (None, _, _) => OwnerChange::Unrelated,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::{OwnedObjectPath, OwnedVariant, Variant};

    use super::{
        Outcome, OwnerChange, Vardict, bind_argument, bound, created_session, owner_change,
        request_path, triggers,
    };
    use crate::shortcut::Shortcut;
    use crate::shortcut::status::{PerAction, Trigger};

    fn vardict<'a>(entries: impl IntoIterator<Item = (&'a str, Variant<'a>)>) -> Vardict {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), OwnedVariant::try_from(value).unwrap()))
            .collect()
    }

    fn shortcut(id: &str, description: &str) -> (String, HashMap<String, Variant<'static>>) {
        (
            id.to_string(),
            HashMap::from([(
                "trigger_description".to_string(),
                Variant::from(description.to_string()),
            )]),
        )
    }

    #[test]
    fn request_paths_follow_the_sender_and_token() {
        assert_eq!(
            request_path(":1.123", "vi_3"),
            "/org/freedesktop/portal/desktop/request/1_123/vi_3"
        );
    }

    #[test]
    fn bind_asks_for_the_preferred_keys() {
        let argument = bind_argument(PerAction {
            toggle: Shortcut::DEFAULT_TOGGLE,
            hold: "Ctrl+Alt+H".parse().unwrap(),
        });
        assert_eq!(
            argument,
            [
                (
                    "toggle",
                    HashMap::from([
                        ("description", Variant::from("Start or stop dictation")),
                        ("preferred_trigger", Variant::from("LOGO+z")),
                    ])
                ),
                (
                    "hold",
                    HashMap::from([
                        ("description", Variant::from("Hold to talk")),
                        ("preferred_trigger", Variant::from("CTRL+ALT+h")),
                    ])
                ),
            ]
        );
    }

    #[test]
    fn session_responses_carry_the_handle() {
        let path = "/org/freedesktop/portal/desktop/session/1_123/vi_1";
        assert_eq!(
            [
                created_session(0, &vardict([("session_handle", Variant::from(path))])),
                created_session(
                    0,
                    &vardict([(
                        "session_handle",
                        Variant::from(OwnedObjectPath::try_from(path).unwrap())
                    )])
                ),
                created_session(1, &Vardict::new()),
                created_session(2, &Vardict::new()),
            ],
            [
                Outcome::Done(OwnedObjectPath::try_from(path).unwrap()),
                Outcome::Done(OwnedObjectPath::try_from(path).unwrap()),
                Outcome::Cancelled,
                Outcome::Failed("the portal ended the request with code 2".to_string()),
            ]
        );
    }

    #[test]
    fn bind_responses_carry_the_triggers() {
        let shortcuts = vec![
            shortcut("toggle", "Press <Super>z"),
            shortcut("hold", "Press <Super>x"),
            shortcut("other", "Press <Super>o"),
        ];
        assert_eq!(
            bound(0, &vardict([("shortcuts", Variant::from(shortcuts))])),
            Outcome::Done(PerAction {
                toggle: Some(Trigger::new("Press <Super>z".to_string())),
                hold: Some(Trigger::new("Press <Super>x".to_string())),
            })
        );
        assert_eq!(bound(1, &Vardict::new()), Outcome::Cancelled);
    }

    #[test]
    fn malformed_responses_fail_with_a_reason() {
        let cases = [
            created_session(0, &Vardict::new()),
            created_session(0, &vardict([("session_handle", Variant::from(5u32))])),
            created_session(
                0,
                &vardict([("session_handle", Variant::from("not a path"))]),
            ),
        ];
        for outcome in cases {
            assert!(
                matches!(&outcome, Outcome::Failed(reason) if reason.starts_with("unexpected portal response")),
                "{outcome:?}"
            );
        }
        for results in [
            Vardict::new(),
            vardict([("shortcuts", Variant::from("toggle"))]),
            vardict([(
                "shortcuts",
                Variant::from(vec![(
                    "toggle".to_string(),
                    HashMap::from([("trigger_description".to_string(), Variant::from(7u32))]),
                )]),
            )]),
        ] {
            assert!(
                matches!(bound(0, &results), Outcome::Failed(_)),
                "{results:?}"
            );
        }
    }

    #[test]
    fn shortcuts_without_a_description_are_unbound() {
        let shortcuts = vec![("toggle".to_string(), Vardict::new())];
        assert_eq!(triggers(&shortcuts), Ok(PerAction::default()));
    }

    #[test]
    fn only_the_session_owner_going_away_restarts() {
        assert_eq!(
            [
                owner_change(Some(":1.5"), ":1.5", ""),
                owner_change(Some(":1.5"), ":1.5", ":1.9"),
                owner_change(None, "", ":1.9"),
                owner_change(Some(":1.5"), "", ":1.9"),
            ],
            [
                OwnerChange::Restarted,
                OwnerChange::Restarted,
                OwnerChange::Appeared(":1.9".to_string()),
                OwnerChange::Unrelated,
            ]
        );
    }
}
