//! Shortcut backend selection, key recording, and operation reports.

use gpui_kit::component::{
    StyledExt,
    button::{Button, ButtonVariants},
    input::Input,
};
use gpui_kit::*;
use voice_input_core::{Action as ShortcutAction, Backend, Keys, Note, Program, Shortcut};

use crate::app::AppView;
use crate::bridge::{BridgeCommand, ShortcutRequest};
use crate::recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

pub fn shortcuts(view: &mut AppView, cx: &mut Context<AppView>) -> Div {
    let selector = div().h_flex().gap_2().children(
        [
            ("backend-portal", Backend::Portal, "portal"),
            (
                "backend-gnome",
                Backend::GnomeKeybinding,
                "GNOME keybinding",
            ),
            ("backend-none", Backend::None, "none"),
        ]
        .map(|(id, backend, label)| {
            Button::new(id)
                .outline()
                .label(label)
                .on_click(cx.listener(move |this, _, _, _| {
                    this.selected_backend = backend;
                }))
        }),
    );
    let body = match view.selected_backend {
        Backend::Portal => portal(view, cx),
        Backend::GnomeKeybinding => gnome(view, cx),
        Backend::None => div()
            .v_flex()
            .gap_2()
            .child("Bind this command in your desktop settings:")
            .child(
                Button::new("copy-toggle-command")
                    .outline()
                    .label("voice-input toggle")
                    .on_click(|_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            "voice-input toggle".to_string(),
                        ));
                    }),
            ),
    };
    let report = view.shortcut_report.as_ref().map(|result| match result {
        Ok(report) => div()
            .v_flex()
            .gap_1()
            .children(report.notes.iter().map(format_note)),
        Err(error) => div().child(error.clone()),
    });
    div()
        .v_flex()
        .gap_3()
        .p_4()
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, _| {
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
        .child(selector)
        .child(body)
        .child(
            Button::new("apply-shortcut-backend")
                .primary()
                .label("Apply")
                .on_click(cx.listener(apply)),
        )
        .children(report)
}

fn portal(view: &AppView, _: &mut Context<AppView>) -> Div {
    let status = view.model.snapshot.as_ref().map(|snapshot| {
        let status = &snapshot.shortcut;
        format!(
            "Session: {:?}\n{}\n{}",
            status.session,
            trigger(&status.actual.toggle),
            trigger(&status.actual.hold)
        )
    });
    div()
        .v_flex()
        .gap_3()
        .children(status)
        .child(
            Button::new("edit-portal-shortcuts")
                .outline()
                .label("Change in GNOME Settings")
                .on_click({
                    let commands = view.commands.clone();
                    move |_, _, _| {
                        let _ = commands.try_send(BridgeCommand::Shortcut(ShortcutRequest::Edit));
                    }
                }),
        )
        .child("Preferred toggle")
        .child(Input::new(&view.toggle_input))
        .child("Preferred hold-to-talk")
        .child(Input::new(&view.hold_input))
}

fn gnome(view: &mut AppView, cx: &mut Context<AppView>) -> Div {
    let (current, error) = match &view.recorder {
        RecorderState::Idle { current } => (
            current.map_or("not set".to_string(), |key| key.to_string()),
            None,
        ),
        RecorderState::Recording { .. } => (
            "Press a shortcut… (Esc cancels, Backspace clears)".to_string(),
            None,
        ),
        RecorderState::Invalid { error, .. } => (
            "invalid shortcut".to_string(),
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
                .child(current)
                .child(
                    Button::new("record-toggle")
                        .outline()
                        .label("Record")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.recorder = this.recorder.clone().apply(RecorderEvent::Start).0;
                        })),
                )
                .child(
                    Button::new("clear-toggle")
                        .ghost()
                        .label("Clear")
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
        .child(Input::new(&view.toggle_input))
        .child(
            Button::new("set-toggle-manual")
                .outline()
                .label("Set toggle")
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
        .child("Hold-to-talk is not supported by this backend.")
        .children(error)
}

fn apply(view: &mut AppView, _: &ClickEvent, _: &mut Window, cx: &mut Context<AppView>) {
    let toggle = view.toggle_input.read(cx).value();
    let hold = view.hold_input.read(cx).value();
    let preferred = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.shortcut.preferred);
    let toggle = parse_or_default(
        toggle.to_string(),
        preferred.map_or(Shortcut::DEFAULT_TOGGLE, |keys| keys.toggle),
    );
    let hold = parse_or_default(
        hold.to_string(),
        preferred.map_or(Shortcut::DEFAULT_HOLD, |keys| keys.hold),
    );
    let result = match (toggle, hold) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(toggle), Ok(hold)) => {
            let keys = match view.selected_backend {
                Backend::Portal => Keys {
                    toggle: Some(toggle),
                    hold: Some(hold),
                },
                Backend::GnomeKeybinding => Keys {
                    toggle: Some(toggle),
                    hold: None,
                },
                Backend::None => Keys {
                    toggle: None,
                    hold: None,
                },
            };
            let _ = view
                .commands
                .try_send(BridgeCommand::Shortcut(ShortcutRequest::Setup {
                    backend: view.selected_backend,
                    keys,
                }));
            Ok(())
        }
    };
    if let Err(error) = result {
        view.shortcut_report = Some(Err(error));
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

fn trigger(trigger: &Option<voice_input_core::Trigger>) -> String {
    trigger.as_ref().map_or("not bound".to_string(), |trigger| {
        if trigger.shortcuts.is_empty() {
            trigger.description.clone()
        } else {
            trigger
                .shortcuts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" or ")
        }
    })
}

fn format_note(note: &Note) -> String {
    match note {
        Note::Installed(path) => format!("Installed {}", path.display()),
        Note::UnstableLauncher(path) => format!(
            "{} is in the Nix store and stops working after an upgrade",
            path.display()
        ),
        Note::NotChecked(schema) => format!("Not checked: {schema}"),
        Note::DaemonNotRunning => {
            "The daemon is not running; the change takes effect when it starts.".to_string()
        }
        Note::GnomeBinding {
            keys,
            command,
            program,
        } => format!(
            "Toggle {keys} → {command} ({})",
            match program {
                Program::Stable(_) => "stable path",
                Program::Unstable(_) => "unstable path",
            }
        ),
        Note::NoCustomShortcut => "No GNOME custom shortcut runs voice-input toggle.".to_string(),
        Note::CustomShortcutRemoved => "Removed the GNOME custom shortcut.".to_string(),
        Note::CustomShortcutDisabled(path) => {
            format!("Disabled the GNOME custom shortcut at {path}; it stays otherwise.")
        }
    }
}
