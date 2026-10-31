//! Shortcut backend selection, key recording, and operation reports.

use gpui_kit::component::{StyledExt, button::ButtonVariants, input::Input, scroll::ScrollbarMode};
use gpui_kit::*;
use limen_core::{Action as ShortcutAction, Backend, Keys, Note, Program, SessionState, Shortcut};

use crate::a11y;
use crate::app::AppView;
use crate::bridge::{BridgeCommand, ShortcutRequest};
use crate::recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

pub fn shortcuts(view: &mut AppView, cx: &mut Context<AppView>, window: &mut Window) -> Div {
    let preferred = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.shortcut.preferred);
    // Follow the daemon's keys until the user edits a field, but only when
    // they differ: setting a value resets the field's cursor and selection,
    // and this runs on every frame.
    if let Some(preferred) = preferred {
        for (input, touched, keys) in [
            (&view.toggle_input, view.toggle_touched, preferred.toggle),
            (&view.hold_input, view.hold_touched, preferred.hold),
        ] {
            let text = keys.to_string();
            if !touched && input.read(cx).value() != text {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
    }
    let selector = div().h_flex().gap_2().children(
        [
            ("backend-portal", Backend::Portal, "portal".to_string()),
            (
                "backend-gnome",
                Backend::GnomeKeybinding,
                rust_i18n::t!("shortcuts.gnome_backend").to_string(),
            ),
            (
                "backend-none",
                Backend::None,
                rust_i18n::t!("shortcuts.none_backend").to_string(),
            ),
        ]
        .map(|(id, backend, label)| {
            let button = a11y::button(id).label(label);
            match backend == view.selected_backend {
                true => button.primary(),
                false => button.outline(),
            }
            .on_click(cx.listener(move |this, _, _, _| {
                this.selected_backend = backend;
                this.backend_selected = true;
            }))
        }),
    );
    let body = match view.selected_backend {
        Backend::Portal => portal(view, cx),
        Backend::GnomeKeybinding => gnome(view, cx),
        Backend::None => div()
            .v_flex()
            .gap_2()
            .child(rust_i18n::t!("shortcuts.bind_command").to_string())
            .child(
                a11y::button("copy-toggle-command")
                    .outline()
                    .label("limen toggle")
                    .on_click(|_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            "limen toggle".to_string(),
                        ));
                    }),
            ),
    };
    let report = view.shortcut_report.as_ref().map(|result| {
        let (report, error) = match result {
            Ok(report) => (report, None),
            Err(failure) => (&failure.report, Some(failure.message.clone())),
        };
        let lines = report
            .notes
            .iter()
            .map(format_note)
            .chain(error)
            .collect::<Vec<_>>();
        a11y::status("shortcut-report", lines.join("\n"))
            .v_flex()
            .gap_1()
            .children(lines)
    });
    let page = div()
        .id("shortcuts-page")
        .track_focus(&view.recorder_focus)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
            if event.keystroke.key == "backspace" && (this.toggle_focused || this.hold_focused) {
                return;
            }
            let event = match event.keystroke.key.as_str() {
                "escape" => RecorderEvent::Escape,
                "backspace" => RecorderEvent::Backspace,
                _ => RecorderEvent::Key(event.keystroke.clone()),
            };
            let (next, action) = this.recorder.clone().apply(event);
            this.recorder = next;
            if let Some(action) = action {
                let request = match action {
                    RecorderAction::Set(keys) => ShortcutRequest::Set {
                        action: ShortcutAction::Toggle,
                        keys,
                    },
                    RecorderAction::Clear => ShortcutRequest::Clear {
                        action: ShortcutAction::Toggle,
                    },
                };
                let _ = this.commands.try_send(BridgeCommand::Shortcut(request));
            }
        }))
        .child(
            div()
                .v_flex()
                .gap_5()
                .p_8()
                .on_scroll(view.shortcuts_scroll.on_scroll())
                .child(super::title(rust_i18n::t!("nav.shortcuts")))
                .child(selector)
                .child(body)
                .child(
                    a11y::button("apply-shortcut-backend")
                        .primary()
                        .label(rust_i18n::t!("shortcuts.apply").to_string())
                        .on_click(cx.listener(apply)),
                )
                .children(report),
        );
    view.shortcuts_scroll
        .container(page, ScrollbarMode::Scrolling)
        .flex_1()
}

fn portal(view: &AppView, _: &mut Context<AppView>) -> Div {
    let status = view.model.snapshot.as_ref().map(|snapshot| {
        let status = &snapshot.shortcut;
        div()
            .v_flex()
            .gap_1()
            .child(format!(
                "{}: {}",
                rust_i18n::t!("shortcuts.session"),
                match status.session {
                    SessionState::Disabled =>
                        rust_i18n::t!("shortcuts.session_disabled").to_string(),
                    SessionState::Binding => rust_i18n::t!("shortcuts.session_binding").to_string(),
                    SessionState::Bound => rust_i18n::t!("shortcuts.session_bound").to_string(),
                    SessionState::Cancelled =>
                        rust_i18n::t!("shortcuts.session_cancelled").to_string(),
                    SessionState::Failed => rust_i18n::t!("shortcuts.session_failed").to_string(),
                }
            ))
            .child(trigger(&status.actual.toggle))
            .child(trigger(&status.actual.hold))
            .children(status.last_error.clone())
            .children(match status.session {
                SessionState::Cancelled | SessionState::Failed => Some(
                    a11y::button("retry-portal")
                        .outline()
                        .label(rust_i18n::t!("shortcuts.retry").to_string())
                        .on_click({
                            let commands = view.commands.clone();
                            let preferred = status.preferred;
                            move |_, _, _| {
                                let _ = commands.try_send(BridgeCommand::Shortcut(
                                    ShortcutRequest::Setup {
                                        backend: Backend::Portal,
                                        keys: Keys {
                                            toggle: Some(preferred.toggle),
                                            hold: Some(preferred.hold),
                                        },
                                    },
                                ));
                            }
                        }),
                ),
                SessionState::Disabled | SessionState::Binding | SessionState::Bound => None,
            })
    });
    div()
        .v_flex()
        .gap_3()
        .children(status)
        .child(
            a11y::button("edit-portal-shortcuts")
                .outline()
                .label(rust_i18n::t!("shortcuts.edit").to_string())
                .on_click({
                    let commands = view.commands.clone();
                    move |_, _, _| {
                        let _ = commands.try_send(BridgeCommand::Shortcut(ShortcutRequest::Edit));
                    }
                }),
        )
        .child(rust_i18n::t!("shortcuts.hold_order").to_string())
        .child(rust_i18n::t!("shortcuts.preferred_toggle").to_string())
        .child(
            Input::new(&view.toggle_input)
                .accessibility_id("toggle-input")
                .aria_label(rust_i18n::t!("shortcuts.preferred_toggle").to_string()),
        )
        .child(rust_i18n::t!("shortcuts.preferred_hold").to_string())
        .child(
            Input::new(&view.hold_input)
                .accessibility_id("hold-input")
                .aria_label(rust_i18n::t!("shortcuts.preferred_hold").to_string()),
        )
}

fn gnome(view: &mut AppView, cx: &mut Context<AppView>) -> Div {
    let (current, error) = match &view.recorder {
        RecorderState::Idle { current } => (
            current.map_or(rust_i18n::t!("shortcuts.not_set").to_string(), |key| {
                key.to_string()
            }),
            None,
        ),
        RecorderState::Recording { .. } => (rust_i18n::t!("shortcuts.recording").to_string(), None),
        RecorderState::Invalid { error, .. } => (
            rust_i18n::t!("shortcuts.invalid").to_string(),
            Some(match error {
                RecorderError::ShiftedSymbol => rust_i18n::t!("hint.shifted_symbol").to_string(),
                RecorderError::Invalid(error) => error.clone(),
            }),
        ),
    };
    div()
        .v_flex()
        .gap_2()
        .child(
            div()
                .h_flex()
                .gap_2()
                .child(a11y::status("recorder-state", current.clone()).child(current))
                .child(
                    a11y::button("record-toggle")
                        .outline()
                        .label(rust_i18n::t!("shortcuts.record").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.recorder = this.recorder.clone().apply(RecorderEvent::Start).0;
                            this.recorder_focus.focus(window, cx);
                        })),
                )
                .child(
                    a11y::button("clear-toggle")
                        .ghost()
                        .label(rust_i18n::t!("action.clear").to_string())
                        .on_click({
                            let commands = view.commands.clone();
                            move |_, _, _| {
                                let _ = commands.try_send(BridgeCommand::Shortcut(
                                    ShortcutRequest::Clear {
                                        action: ShortcutAction::Toggle,
                                    },
                                ));
                            }
                        }),
                ),
        )
        .child(
            Input::new(&view.toggle_input)
                .accessibility_id("toggle-input")
                .aria_label(rust_i18n::t!("shortcuts.preferred_toggle").to_string()),
        )
        .child(
            a11y::button("set-toggle-manual")
                .outline()
                .label(rust_i18n::t!("shortcuts.set_toggle").to_string())
                .on_click(cx.listener(|this, _, _, cx| {
                    let text = this.toggle_input.read(cx).value();
                    let (next, action) = this
                        .recorder
                        .clone()
                        .apply(RecorderEvent::Manual(text.to_string()));
                    this.recorder = next;
                    if let Some(RecorderAction::Set(keys)) = action {
                        let _ =
                            this.commands
                                .try_send(BridgeCommand::Shortcut(ShortcutRequest::Set {
                                    action: ShortcutAction::Toggle,
                                    keys,
                                }));
                    }
                })),
        )
        .child(rust_i18n::t!("shortcuts.no_hold").to_string())
        .children(error.map(|error| a11y::alert("recorder-error", error.clone()).child(error)))
}

fn apply(view: &mut AppView, _: &ClickEvent, _: &mut Window, cx: &mut Context<AppView>) {
    let toggle = view.toggle_input.read(cx).value();
    let hold = view.hold_input.read(cx).value();
    let preferred = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.shortcut.preferred);
    let toggle = match view.toggle_touched {
        true => parse_or_default(
            toggle.to_string(),
            preferred.map_or(Shortcut::DEFAULT_TOGGLE, |keys| keys.toggle),
        ),
        false => Ok(preferred.map_or(Shortcut::DEFAULT_TOGGLE, |keys| keys.toggle)),
    };
    let hold = match view.hold_touched {
        true => parse_or_default(
            hold.to_string(),
            preferred.map_or(Shortcut::DEFAULT_HOLD, |keys| keys.hold),
        ),
        false => Ok(preferred.map_or(Shortcut::DEFAULT_HOLD, |keys| keys.hold)),
    };
    let result = keys_for_backend(view.selected_backend, toggle, hold).map(|keys| {
        let _ = view
            .commands
            .try_send(BridgeCommand::Shortcut(ShortcutRequest::Setup {
                backend: view.selected_backend,
                keys,
            }));
    });
    if let Err(error) = result {
        view.shortcut_report = Some(Err(crate::bridge::ShortcutFailure {
            report: Default::default(),
            message: error,
        }));
    }
}

fn keys_for_backend(
    backend: Backend,
    toggle: Result<Shortcut, String>,
    hold: Result<Shortcut, String>,
) -> Result<Keys, String> {
    match backend {
        Backend::None => Ok(Keys {
            toggle: None,
            hold: None,
        }),
        Backend::GnomeKeybinding => toggle.map(|toggle| Keys {
            toggle: Some(toggle),
            hold: None,
        }),
        Backend::Portal => match (toggle, hold) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(toggle), Ok(hold)) => Ok(Keys {
                toggle: Some(toggle),
                hold: Some(hold),
            }),
        },
    }
}

fn parse_or_default(text: String, default: Shortcut) -> Result<Shortcut, String> {
    match text.trim().is_empty() {
        true => Ok(default),
        false => text
            .parse::<Shortcut>()
            .and_then(Shortcut::validate)
            .map_err(|error| error.to_string()),
    }
}

fn trigger(trigger: &Option<limen_core::Trigger>) -> String {
    trigger.as_ref().map_or(
        rust_i18n::t!("shortcuts.not_bound").to_string(),
        |trigger| {
            if trigger.shortcuts.is_empty() {
                trigger.description.clone()
            } else {
                trigger
                    .shortcuts
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(&format!(" {} ", rust_i18n::t!("shortcuts.or")))
            }
        },
    )
}

fn format_note(note: &Note) -> String {
    match note {
        Note::Installed(path) => format!("{} {}", rust_i18n::t!("note.installed"), path.display()),
        Note::UnstableLauncher(path) => {
            format!("{} {}", path.display(), rust_i18n::t!("note.unstable"))
        }
        Note::NotChecked(schema) => format!("{}: {schema}", rust_i18n::t!("note.not_checked")),
        Note::DaemonNotRunning => rust_i18n::t!("note.daemon_offline").to_string(),
        Note::GnomeBinding {
            keys,
            command,
            program,
        } => format!(
            "{} {keys} → {command} ({})",
            rust_i18n::t!("note.toggle"),
            match program {
                Program::Stable(_) => rust_i18n::t!("note.stable_path"),
                Program::Unstable(_) => rust_i18n::t!("note.unstable_path"),
            }
        ),
        Note::NoCustomShortcut => rust_i18n::t!("note.no_custom").to_string(),
        Note::CustomShortcutRemoved => rust_i18n::t!("note.removed").to_string(),
        Note::CustomShortcutDisabled(path) => {
            format!("{}: {path}", rust_i18n::t!("note.disabled"))
        }
    }
}

#[cfg(test)]
mod tests {
    use limen_core::{Backend, Keys, Shortcut};

    use super::keys_for_backend;

    #[test]
    fn backends_ignore_invalid_keys_they_do_not_use() {
        let invalid = || Err("invalid key".to_string());
        assert_eq!(
            keys_for_backend(Backend::None, invalid(), invalid()),
            Ok(Keys {
                toggle: None,
                hold: None,
            })
        );
        assert_eq!(
            keys_for_backend(
                Backend::GnomeKeybinding,
                Ok(Shortcut::DEFAULT_TOGGLE),
                invalid(),
            ),
            Ok(Keys {
                toggle: Some(Shortcut::DEFAULT_TOGGLE),
                hold: None,
            })
        );
        assert!(keys_for_backend(Backend::Portal, invalid(), invalid()).is_err());
    }
}
