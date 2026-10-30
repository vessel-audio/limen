//! GPUI pages rendered from the pure GUI model.

use gpui_kit::component::{
    ActiveTheme, StyledExt, WindowExt, button::ButtonVariants, scroll::ScrollbarMode,
};
use gpui_kit::*;
use voice_input_core::{HistoryEntry, HistoryOutcome};

use crate::a11y;
use crate::app::AppView;
use crate::bridge::BridgeCommand;

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
            a11y::button("clear-history")
                .ghost()
                .label(rust_i18n::t!("action.clear").to_string())
                .on_click(cx.listener(|this, _, _, _| {
                    this.confirm_clear = true;
                })),
        );
    let card_background = cx.theme().muted.opacity(0.25);
    let body = match entries.is_empty() {
        true => div()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("history.empty").to_string()),
        false => view
            .history_scroll
            .container(
                div().id("history-entries").child(
                    div()
                        .v_flex()
                        .gap_2()
                        .on_scroll(view.history_scroll.on_scroll())
                        .children(
                            entries
                                .iter()
                                .rev()
                                .map(|entry| history_card(entry, card_background)),
                        ),
                ),
                ScrollbarMode::Scrolling,
            )
            .flex_1(),
    };
    let confirmation = view.confirm_clear.then(|| {
        let question = rust_i18n::t!("history.clear_confirm").to_string();
        a11y::alert("clear-confirmation", question.clone())
            .h_flex()
            .gap_2()
            .child(question)
            .child(
                a11y::button("confirm-clear")
                    .primary()
                    .label(rust_i18n::t!("action.clear").to_string())
                    .on_click(cx.listener(|this, _, _, _| {
                        let _ = this.commands.try_send(BridgeCommand::ClearHistory);
                        this.confirm_clear = false;
                    })),
            )
            .child(
                a11y::button("cancel-clear")
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

mod dictation;
mod requests;
mod shortcuts;
pub use dictation::dictation;
pub use requests::RequestKey;
pub use shortcuts::shortcuts;

/// Serif page heading shared by every non-dictation page.
pub fn title(text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    div()
        .id("page-title")
        .accessibility_id("page-title")
        .role(Role::Heading)
        .aria_label(text.clone())
        .text_xl()
        .font_family("serif")
        .child(text)
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
                    a11y::button(format!("copy-{}", entry.id))
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
