//! GPUI pages rendered from the pure GUI model.

use gpui_kit::component::{
    Disableable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use voice_input_core::{DictationState, HistoryEntry, HistoryOutcome};

use crate::app::AppView;
use crate::bridge::BridgeCommand;
use crate::model::DictationButton;

pub fn dictation(view: &AppView, _: &mut Context<AppView>) -> Div {
    let (title, detail) = view.model.snapshot.as_ref().map_or(
        ("Connecting".to_string(), String::new()),
        |snapshot| {
            let detail = snapshot
                .state
                .dictation
                .since_ms
                .map(|since| format!(" {:02}:{:02}", elapsed(since) / 60, elapsed(since) % 60))
                .unwrap_or_default();
            match snapshot.state.dictation.state {
                DictationState::Idle => (rust_i18n::t!("status.idle").to_string(), detail),
                DictationState::Starting { .. } => {
                    (rust_i18n::t!("status.starting").to_string(), detail)
                }
                DictationState::Recording {
                    control: voice_input_core::Control::Toggle,
                } => (rust_i18n::t!("status.recording").to_string(), detail),
                DictationState::Recording {
                    control: voice_input_core::Control::Hold,
                } => (rust_i18n::t!("status.hold_recording").to_string(), detail),
                DictationState::Transcribing => {
                    (rust_i18n::t!("status.transcribing").to_string(), detail)
                }
                DictationState::Committing => {
                    (rust_i18n::t!("status.committing").to_string(), detail)
                }
            }
        },
    );
    let button = view.model.button();
    let (label, command, enabled) = match button {
        DictationButton::Start => (
            rust_i18n::t!("action.start").to_string(),
            Some(BridgeCommand::Start),
            true,
        ),
        DictationButton::Stop => (
            rust_i18n::t!("action.stop").to_string(),
            Some(BridgeCommand::Stop),
            true,
        ),
        DictationButton::RecordAnother => (
            rust_i18n::t!("action.record_another").to_string(),
            Some(BridgeCommand::Start),
            true,
        ),
        DictationButton::Queued => (rust_i18n::t!("status.queued").to_string(), None, false),
        DictationButton::Disabled(_) => (title.clone(), None, false),
    };
    let commands = view.commands.clone();
    let latest = view
        .model
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.state.history.last().cloned());
    let error = view
        .model
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.state.last_error.clone())
        .map(|error| error.message)
        .or_else(|| view.model.command_error.clone());
    div()
        .v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_4()
        .p_4()
        .child(
            div()
                .v_flex()
                .items_center()
                .gap_2()
                .child(format!("{title}{detail}"))
                .children(error.map(|message| div().child(message))),
        )
        .child(
            Button::new("dictation-primary")
                .primary()
                .label(label)
                .disabled(!enabled)
                .on_click(move |_, _, _| {
                    if let Some(command) = command.clone() {
                        let _ = commands.try_send(command);
                    }
                }),
        )
        .child(rust_i18n::t!("hint.keep_delivery").to_string())
        .children(latest.map(|entry| history_card(&entry)))
}

pub fn history(view: &mut AppView, cx: &mut Context<AppView>) -> Div {
    let entries = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.state.history.clone())
        .unwrap_or_default();
    let header = div()
        .h_flex()
        .items_center()
        .justify_between()
        .child(format!(
            "{} {} · {}",
            rust_i18n::t!("history.title"),
            entries.len(),
            rust_i18n::t!("history.memory")
        ))
        .child(
            Button::new("clear-history")
                .ghost()
                .label(rust_i18n::t!("action.clear").to_string())
                .on_click(cx.listener(|this, _, _, _| {
                    this.confirm_clear = true;
                })),
        );
    let body = match entries.is_empty() {
        true => div().child(rust_i18n::t!("history.empty").to_string()),
        false => div()
            .v_flex()
            .gap_2()
            .children(entries.iter().rev().map(history_card)),
    };
    let confirmation = view.confirm_clear.then(|| {
        div()
            .h_flex()
            .gap_2()
            .child("Clear completed history?")
            .child(
                Button::new("confirm-clear")
                    .primary()
                    .label(rust_i18n::t!("action.clear").to_string())
                    .on_click({
                        let commands = view.commands.clone();
                        move |_, _, _| {
                            let _ = commands.try_send(BridgeCommand::ClearHistory);
                        }
                    }),
            )
            .child(
                Button::new("cancel-clear")
                    .ghost()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, _| {
                        this.confirm_clear = false;
                    })),
            )
    });
    div()
        .v_flex()
        .flex_1()
        .gap_3()
        .p_4()
        .child(header)
        .child(body)
        .children(confirmation)
}

pub fn shortcuts(view: &AppView, _: &mut Context<AppView>) -> Div {
    let backend = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.shortcut.backend.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    div()
        .v_flex()
        .gap_3()
        .p_4()
        .child(rust_i18n::t!("nav.shortcuts").to_string())
        .child(format!("Backend: {backend}"))
}

pub fn about(view: &AppView, cx: &mut Context<AppView>) -> impl IntoElement {
    let config = view.paths.config_file.clone();
    let log = view.paths.log_file.clone();
    div()
        .v_flex()
        .gap_3()
        .p_4()
        .child(rust_i18n::t!("about.title").to_string())
        .child(format!("Version: {}", env!("CARGO_PKG_VERSION")))
        .child("Protocol: 1")
        .child(
            view.model
                .daemon_version
                .as_deref()
                .unwrap_or("unknown")
                .to_string(),
        )
        .child(format!("Socket: {}", view.paths.socket.display()))
        .child(format!("Config: {}", config.display()))
        .child(format!("Log: {}", log.display()))
        .child(
            div()
                .h_flex()
                .gap_2()
                .child(
                    Button::new("open-config")
                        .outline()
                        .label("Open config")
                        .on_click(move |_, _, cx| cx.open_with_system(&config)),
                )
                .child(
                    Button::new("open-log")
                        .outline()
                        .label("Open log")
                        .on_click(move |_, _, cx| cx.open_with_system(&log)),
                )
                .child(
                    Button::new("restart-daemon")
                        .outline()
                        .label("Restart daemon")
                        .on_click({
                            let commands = view.commands.clone();
                            move |_, _, _| {
                                let _ = commands.try_send(BridgeCommand::RestartDaemon);
                            }
                        }),
                )
                .child(
                    Button::new("stop-daemon")
                        .outline()
                        .label("Stop daemon")
                        .on_click({
                            let commands = view.commands.clone();
                            move |_, _, _| {
                                let _ = commands.try_send(BridgeCommand::ShutdownDaemon);
                            }
                        }),
                )
                .child(
                    Button::new("close-about")
                        .ghost()
                        .label("Close")
                        .on_click(cx.listener(|this, _, _, _| this.about = false)),
                ),
        )
}

fn history_card(entry: &HistoryEntry) -> Div {
    let outcome = match &entry.outcome {
        HistoryOutcome::Pending => "pending",
        HistoryOutcome::Committed => "history.committed",
        HistoryOutcome::Kept => "history.kept",
        HistoryOutcome::CommitFailed { .. } => "history.failed",
        HistoryOutcome::Dropped => "history.dropped",
    };
    let text = entry.text.clone().unwrap_or_else(|| entry.original.clone());
    let original = (entry.text.is_some() && entry.text.as_deref() != Some(entry.original.as_str()))
        .then(|| format!("{}: {}", rust_i18n::t!("history.original"), entry.original));
    div()
        .v_flex()
        .gap_1()
        .border_b_1()
        .pb_2()
        .child(
            div()
                .h_flex()
                .justify_between()
                .child(format!("{} · {}", entry.at_ms, rust_i18n::t!(outcome)))
                .child(
                    Button::new(format!("copy-{}", entry.id))
                        .ghost()
                        .label(rust_i18n::t!("action.copy").to_string())
                        .on_click({
                            let text = text.clone();
                            move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                window.push_notification(rust_i18n::t!("toast.copied"), cx);
                            }
                        }),
                ),
        )
        .child(text)
        .children(original)
}

fn elapsed(since_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now.saturating_sub(since_ms) / 1000
}
