//! The takes on the dictation page, newest first, in one list: each
//! transcript with how long its request took, and each request that failed.
//! The daemon keeps more transcripts than requests, so older transcripts
//! show without a duration.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Sizable, StyledExt, WindowExt, button::ButtonVariants, scroll::ScrollbarMode,
};
use gpui_kit::*;
use limen_core::{DictationState, HistoryEntry, HistoryOutcome, RequestEntry, RequestOutcome};

use crate::a11y;
use crate::app::AppView;
use crate::bridge::BridgeCommand;
use crate::motion;

/// Identifies one request: its take, and its start in case a take ever
/// produces more than one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestKey {
    take: u64,
    started_ms: u64,
}

/// Width of the time column, so details line up under the status.
const TIME_WIDTH: f32 = 64.;
const DOT: f32 = 6.;
const GAP: f32 = 12.;

/// One row of the list.
#[derive(Debug, PartialEq)]
enum Row<'a> {
    /// A transcript, and how long its take's request took while the daemon
    /// still lists that request.
    Transcript {
        entry: &'a HistoryEntry,
        took: Option<u64>,
    },
    /// A request that produced no transcript, and why.
    Failed {
        request: &'a RequestEntry,
        error: &'a str,
    },
}

/// The rows, newest take first. A succeeded request shows as its
/// transcript's duration; one whose transcript was blank or cleared shows
/// nothing, so clearing empties the list but for failures.
fn rows<'a>(history: &'a [HistoryEntry], requests: &'a [RequestEntry]) -> Vec<Row<'a>> {
    let mut rows = history
        .iter()
        .map(|entry| {
            let took = requests
                .iter()
                .rev()
                .find(|request| {
                    request.take == entry.take
                        && matches!(request.outcome, RequestOutcome::Succeeded)
                })
                .map(|request| request.duration_ms);
            ((entry.take, entry.at_ms), Row::Transcript { entry, took })
        })
        .chain(
            requests
                .iter()
                .filter_map(|request| match &request.outcome {
                    RequestOutcome::Failed { error } => Some((
                        (request.take, request.started_ms),
                        Row::Failed { request, error },
                    )),
                    RequestOutcome::Succeeded => None,
                }),
        )
        .collect::<Vec<_>>();
    rows.sort_by(|(newer, _), (older, _)| older.cmp(newer));
    rows.into_iter().map(|(_, row)| row).collect()
}

pub fn takes(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let (history, requests, in_flight) = view.model.snapshot.as_ref().map_or_else(
        || (Vec::new(), Vec::new(), None),
        |snapshot| {
            let in_flight = match snapshot.state.dictation.state {
                DictationState::Transcribing => snapshot.state.dictation.since_ms,
                DictationState::Idle
                | DictationState::Starting { .. }
                | DictationState::Recording { .. }
                | DictationState::Committing => None,
            };
            (
                snapshot.state.history.clone(),
                snapshot.state.requests.clone(),
                in_flight,
            )
        },
    );
    let failed = requests
        .iter()
        .filter(|request| matches!(request.outcome, RequestOutcome::Failed { .. }))
        .count();
    let summary = (!requests.is_empty()).then(|| {
        div()
            .min_w_0()
            .text_xs()
            .truncate()
            .text_color(cx.theme().muted_foreground)
            .child(format!(
                "{} {} · {} {} · {} {failed}",
                requests.len(),
                rust_i18n::t!("requests.count"),
                rust_i18n::t!("requests.average"),
                duration(
                    requests
                        .iter()
                        .map(|request| request.duration_ms)
                        .sum::<u64>()
                        / requests.len() as u64
                ),
                rust_i18n::t!("requests.failed"),
            ))
    });
    // Clearing asks in place of the summary and the button, so the list
    // never moves.
    let actions = match (history.is_empty(), view.confirm_clear) {
        (true, _) => div().children(summary),
        (false, false) => div()
            .h_flex()
            .min_w_0()
            .items_center()
            .gap_3()
            .children(summary)
            .child(
                a11y::button("clear-history")
                    .ghost()
                    .xsmall()
                    .label(rust_i18n::t!("action.clear").to_string())
                    .on_click(cx.listener(|this, _, _, _| {
                        this.confirm_clear = true;
                    })),
            ),
        (false, true) => {
            let question = rust_i18n::t!("history.clear_confirm").to_string();
            div().child(
                a11y::alert("clear-confirmation", question.clone())
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(question)
                    .child(
                        a11y::button("confirm-clear")
                            .primary()
                            .xsmall()
                            .label(rust_i18n::t!("action.clear").to_string())
                            .on_click(cx.listener(|this, _, _, _| {
                                let _ = this.commands.try_send(BridgeCommand::ClearHistory);
                                this.confirm_clear = false;
                            })),
                    )
                    .child(
                        a11y::button("cancel-clear")
                            .ghost()
                            .xsmall()
                            .label(rust_i18n::t!("action.cancel").to_string())
                            .on_click(cx.listener(|this, _, _, _| {
                                this.confirm_clear = false;
                            })),
                    ),
            )
        }
    };
    let live = in_flight.map(|since_ms| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        div().px_4().py_3().child(
            headline(
                since_ms,
                dot(cx.theme().primary).with_animation(
                    "request-in-flight",
                    motion::pulse(),
                    |dot, t| dot.opacity(t),
                ),
                rust_i18n::t!("requests.in_flight").to_string(),
                cx,
            )
            .child(div().flex_1())
            .child(took(now.saturating_sub(since_ms), cx)),
        )
    });
    let rows = rows(&history, &requests)
        .into_iter()
        .map(|row| match row {
            Row::Transcript { entry, took } => transcript(entry, took, cx),
            Row::Failed { request, error } => failure(view, request, error, cx),
        })
        .collect::<Vec<_>>();
    let empty = (rows.is_empty() && live.is_none()).then(|| {
        div()
            .px_4()
            .py_6()
            .text_sm()
            .text_center()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("history.empty").to_string())
    });
    div()
        .v_flex()
        .flex_1()
        .min_h_0()
        .gap_3()
        .child(super::section_header(rust_i18n::t!("history.title"), cx).child(actions))
        // The page's only list, filling the rest of it; its scrollbar stays
        // visible to say how much more there is.
        .child(
            view.takes_scroll
                .container(
                    div().id("takes").child(
                        div()
                            .v_flex()
                            .on_scroll(view.takes_scroll.on_scroll())
                            .children(live.into_iter().chain(rows).enumerate().map(
                                |(index, row)| match index {
                                    0 => row,
                                    _ => row.border_t_1().border_color(cx.theme().border),
                                },
                            ))
                            .children(empty),
                    ),
                    ScrollbarMode::Always,
                )
                .flex_1()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18)),
        )
}

/// A transcript: when and how it ended, how long its request took, a
/// button that copies it, the text, and the original it was polished from.
fn transcript(entry: &HistoryEntry, duration_ms: Option<u64>, cx: &mut Context<AppView>) -> Div {
    let (outcome, color) = match &entry.outcome {
        HistoryOutcome::Pending => ("history.pending", cx.theme().primary),
        HistoryOutcome::Committed => ("history.committed", cx.theme().success),
        HistoryOutcome::Kept => ("history.kept", cx.theme().success),
        HistoryOutcome::CommitFailed { .. } => ("history.failed", cx.theme().danger),
        HistoryOutcome::Dropped => ("history.dropped", cx.theme().muted_foreground),
    };
    let text = entry.text.clone().unwrap_or_else(|| entry.original.clone());
    let original = (entry.text.is_some() && entry.text.as_deref() != Some(entry.original.as_str()))
        .then(|| format!("{}: {}", rust_i18n::t!("history.original"), entry.original));
    div()
        .v_flex()
        .gap_1()
        .px_4()
        .py_3()
        .child(
            headline(
                entry.at_ms,
                dot(color),
                rust_i18n::t!(outcome).to_string(),
                cx,
            )
            .child(div().flex_1())
            .children(duration_ms.map(|duration_ms| took(duration_ms, cx)))
            .child(
                a11y::button(format!("copy-{}", entry.id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .tooltip(rust_i18n::t!("action.copy").to_string())
                    .accessibility_label(rust_i18n::t!("action.copy").to_string())
                    .on_click({
                        let text = text.clone();
                        move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            window.push_notification(rust_i18n::t!("toast.copied"), cx);
                        }
                    }),
            ),
        )
        .child(detail().child(text))
        .children(original.map(|original| {
            detail()
                .text_color(cx.theme().muted_foreground)
                .child(original)
        }))
}

/// A failed request: one line of its error, all of it once pressed.
fn failure(view: &AppView, request: &RequestEntry, error: &str, cx: &mut Context<AppView>) -> Div {
    let error = error.to_string();
    let key = RequestKey {
        take: request.take,
        started_ms: request.started_ms,
    };
    let id = format!("{}-{}", request.take, request.started_ms);
    let error_line = detail().text_color(cx.theme().danger);
    let expanded = view.expanded_request == Some(key);
    div().child(
        div()
            .id(SharedString::from(format!("request-{id}")))
            .role(Role::Button)
            .accessibility_id(format!("request-{id}"))
            .aria_label(rust_i18n::t!("requests.failed_status").to_string())
            .aria_description(error.clone())
            .aria_expanded(expanded)
            .v_flex()
            .gap_1()
            .px_4()
            .py_3()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().accent.opacity(0.5)))
            .on_click(cx.listener(move |this, _, _, _| {
                this.expanded_request = match this.expanded_request == Some(key) {
                    true => None,
                    false => Some(key),
                };
            }))
            .child(
                headline(
                    request.started_ms,
                    dot(cx.theme().danger),
                    rust_i18n::t!("requests.failed_status").to_string(),
                    cx,
                )
                .child(div().flex_1())
                .child(took(request.duration_ms, cx)),
            )
            .child(match expanded {
                true => error_line
                    .child(error)
                    .with_animation(
                        SharedString::from(format!("request-detail-{id}")),
                        motion::enter(),
                        |detail, t| detail.opacity(t),
                    )
                    .into_any_element(),
                false => error_line.truncate().child(error).into_any_element(),
            }),
    )
}

/// A row's first line: time, status dot, and status; callers append a spacer
/// and what follows.
fn headline(at_ms: u64, dot: impl IntoElement, status: String, cx: &mut Context<AppView>) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(GAP))
        .text_sm()
        .child(
            div()
                .flex_shrink_0()
                .w(px(TIME_WIDTH))
                .text_color(cx.theme().muted_foreground)
                .child(super::format_time(at_ms)),
        )
        .child(dot)
        .child(div().font_medium().child(status))
}

/// A row's second line, indented under the status.
fn detail() -> Div {
    div().pl(px(TIME_WIDTH + DOT + 2. * GAP)).text_sm()
}

fn dot(color: Hsla) -> Div {
    div().flex_shrink_0().size(px(DOT)).rounded_full().bg(color)
}

fn took(duration_ms: u64, cx: &mut Context<AppView>) -> Div {
    div()
        .flex_shrink_0()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(duration(duration_ms))
}

fn duration(duration_ms: u64) -> String {
    match duration_ms {
        0..1000 => format!("{duration_ms} ms"),
        _ => format!("{:.1} s", duration_ms as f64 / 1000.),
    }
}

#[cfg(test)]
mod tests {
    use limen_core::{HistoryEntry, HistoryOutcome, RequestEntry, RequestOutcome};

    use super::{Row, rows};

    fn kept(take: u64) -> HistoryEntry {
        HistoryEntry {
            id: take,
            take,
            at_ms: take * 1000 + 500,
            original: format!("take {take}"),
            text: Some(format!("take {take}")),
            outcome: HistoryOutcome::Kept,
            truncated: false,
        }
    }

    fn request(take: u64, outcome: RequestOutcome) -> RequestEntry {
        RequestEntry {
            take,
            started_ms: take * 1000,
            duration_ms: take * 100,
            outcome,
        }
    }

    #[test]
    fn a_take_shows_once_with_its_requests_duration() {
        // Take 1's request has left the daemon's list; take 4's transcript
        // was blank.
        let history = [kept(1), kept(3), kept(5)];
        let requests = [
            request(
                2,
                RequestOutcome::Failed {
                    error: "503".to_string(),
                },
            ),
            request(3, RequestOutcome::Succeeded),
            request(4, RequestOutcome::Succeeded),
            request(5, RequestOutcome::Succeeded),
        ];
        assert_eq!(
            rows(&history, &requests),
            [
                Row::Transcript {
                    entry: &history[2],
                    took: Some(500)
                },
                Row::Transcript {
                    entry: &history[1],
                    took: Some(300)
                },
                Row::Failed {
                    request: &requests[0],
                    error: "503"
                },
                Row::Transcript {
                    entry: &history[0],
                    took: None
                },
            ]
        );
    }
}
