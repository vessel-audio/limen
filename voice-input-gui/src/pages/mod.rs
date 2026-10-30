//! GPUI pages rendered from the pure GUI model.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, StyledExt, WindowExt,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use voice_input_core::{DictationState, HistoryEntry, HistoryOutcome};

use crate::app::{AppView, Page};
use crate::bridge::BridgeCommand;
use crate::model::{DictationButton, DisabledReason};

pub fn dictation(view: &AppView, cx: &mut Context<AppView>) -> Stateful<Div> {
    let (title, detail) = view.model.snapshot.as_ref().map_or(
        (
            rust_i18n::t!("connection.connecting").to_string(),
            String::new(),
        ),
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
    let level_meter = view.model.snapshot.as_ref().and_then(|snapshot| {
        if !matches!(
            snapshot.state.dictation.state,
            DictationState::Recording { .. }
        ) {
            return None;
        }
        let bars = if view.level_history.is_empty() {
            "▁▁▁▁▁▁▁▁".to_string()
        } else {
            view.level_history
                .iter()
                .map(|rms| match *rms {
                    value if value < 0.12 => '▁',
                    value if value < 0.25 => '▂',
                    value if value < 0.37 => '▃',
                    value if value < 0.50 => '▄',
                    value if value < 0.62 => '▅',
                    value if value < 0.75 => '▆',
                    value if value < 0.87 => '▇',
                    _ => '█',
                })
                .collect::<String>()
        };
        Some(
            div().v_flex().items_center().child(bars).children(
                view.no_sound_detected()
                    .then(|| rust_i18n::t!("hint.no_sound").to_string()),
            ),
        )
    });
    let hold_timeout = view.model.snapshot.as_ref().and_then(|snapshot| {
        snapshot.state.dictation.since_ms.and_then(|since| {
            (matches!(
                snapshot.state.dictation.state,
                DictationState::Recording {
                    control: voice_input_core::Control::Hold,
                }
            ) && elapsed(since) >= 45)
                .then(|| rust_i18n::t!("hint.hold_timeout").to_string())
        })
    });
    let latest = view
        .model
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.state.history.last().cloned());
    let recording = view.model.snapshot.as_ref().is_some_and(|snapshot| {
        matches!(
            snapshot.state.dictation.state,
            DictationState::Recording { .. }
        )
    });
    let halo = div()
        .size(px(64.))
        .rounded_full()
        .bg(cx.theme().primary.opacity(0.12))
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(IconName::Mic)
                .size(px(28.))
                .text_color(cx.theme().primary),
        );
    let primary = cx.theme().primary;
    let error = view
        .model
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.state.last_error.clone())
        .map(|error| error.message)
        .or_else(|| view.model.command_error.clone());
    let content = div()
        .v_flex()
        .min_h(relative(1.))
        .items_center()
        .justify_center()
        .gap_6()
        .p_8()
        .child(
            div()
                .v_flex()
                .w_full()
                .max_w(px(560.))
                .items_center()
                .gap_5()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .px_10()
                .py_12()
                .child(match recording {
                    true => halo
                        .with_animation("recording-halo", crate::motion::pulse(), move |halo, t| {
                            halo.bg(primary.opacity(0.32 * t))
                        })
                        .into_any_element(),
                    false => halo.into_any_element(),
                })
                .child(
                    div()
                        .v_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_2xl()
                                .font_family("serif")
                                .child(format!("{title}{detail}")),
                        )
                        .children(error.map(|message| {
                            div()
                                .v_flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .w_full()
                                        .text_sm()
                                        .text_center()
                                        .text_color(cx.theme().danger)
                                        .line_clamp(2)
                                        .child(message),
                                )
                                .child(
                                    Button::new("view-history")
                                        .ghost()
                                        .small()
                                        .label(rust_i18n::t!("action.view_history").to_string())
                                        .on_click(
                                            cx.listener(|this, _, _, _| this.page = Page::History),
                                        ),
                                )
                        })),
                )
                .children(level_meter)
                .child(
                    Button::new("dictation-primary")
                        .primary()
                        .label(label)
                        .w_full()
                        .max_w(px(320.))
                        .disabled(!enabled)
                        .on_click(move |_, _, _| {
                            if let Some(command) = command.clone() {
                                let _ = commands.try_send(command);
                            }
                        }),
                )
                .children(hold_timeout)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(match button {
                            DictationButton::Disabled(DisabledReason::ReleaseShortcut) => {
                                rust_i18n::t!("hint.release_shortcut").to_string()
                            }
                            _ => rust_i18n::t!("hint.keep_delivery").to_string(),
                        }),
                )
                .children(latest.map(|entry| {
                    div()
                        .w_full()
                        .text_left()
                        .child(history_card(&entry, cx.theme().muted.opacity(0.25)))
                })),
        )
        .child(requests::requests(view, cx));
    // Centre the card while it fits; scroll from the top once it does not.
    div()
        .id("dictation-page")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(content)
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
        .items_start()
        .justify_between()
        .child(
            div()
                .v_flex()
                .gap_1p5()
                .child(title(format!(
                    "{} · {}",
                    rust_i18n::t!("history.title"),
                    entries.len()
                )))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!("history.memory").to_string()),
                ),
        )
        .child(
            Button::new("clear-history")
                .ghost()
                .label(rust_i18n::t!("action.clear").to_string())
                .on_click(cx.listener(|this, _, _, _| {
                    this.confirm_clear = true;
                })),
        );
    let card_background = cx.theme().muted.opacity(0.25);
    let body = match entries.is_empty() {
        true => div()
            .id("history-empty")
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("history.empty").to_string()),
        false => div()
            .id("history-entries")
            .v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_2()
            .children(
                entries
                    .iter()
                    .rev()
                    .map(|entry| history_card(entry, card_background)),
            ),
    };
    let confirmation = view.confirm_clear.then(|| {
        div()
            .h_flex()
            .gap_2()
            .child(rust_i18n::t!("history.clear_confirm").to_string())
            .child(
                Button::new("confirm-clear")
                    .primary()
                    .label(rust_i18n::t!("action.clear").to_string())
                    .on_click(cx.listener(|this, _, _, _| {
                        let _ = this.commands.try_send(BridgeCommand::ClearHistory);
                        this.confirm_clear = false;
                    })),
            )
            .child(
                Button::new("cancel-clear")
                    .ghost()
                    .label(rust_i18n::t!("action.cancel").to_string())
                    .on_click(cx.listener(|this, _, _, _| {
                        this.confirm_clear = false;
                    })),
            )
    });
    div()
        .v_flex()
        .flex_1()
        .min_h_0()
        .gap_6()
        .p_8()
        .child(header)
        .child(body)
        .children(confirmation)
}

mod requests;
mod shortcuts;
pub use shortcuts::shortcuts;

/// Serif page heading shared by every non-dictation page.
pub fn title(text: impl Into<SharedString>) -> Div {
    div().text_xl().font_family("serif").child(text.into())
}

fn history_card(entry: &HistoryEntry, background: Hsla) -> Div {
    let outcome = match &entry.outcome {
        HistoryOutcome::Pending => "history.pending",
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
        .gap_2()
        .rounded_lg()
        .bg(background)
        .p_3()
        .child(
            div()
                .h_flex()
                .justify_between()
                .child(format!(
                    "{} · {}",
                    format_time(entry.at_ms),
                    rust_i18n::t!(outcome)
                ))
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

fn format_time(at_ms: u64) -> String {
    let seconds = (at_ms / 1000) as libc::time_t;
    let mut broken_down = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r initializes `broken_down` when it returns a non-null
    // pointer. The input points to a plain integer and the output is POD.
    let result = unsafe { libc::localtime_r(&seconds, broken_down.as_mut_ptr()) };
    if result.is_null() {
        return "--:--:--".to_string();
    }
    // SAFETY: the successful call initialized every field used here.
    let broken_down = unsafe { broken_down.assume_init() };
    format!(
        "{:02}:{:02}:{:02}",
        broken_down.tm_hour, broken_down.tm_min, broken_down.tm_sec
    )
}

fn elapsed(since_ms: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now.saturating_sub(since_ms) / 1000
}
