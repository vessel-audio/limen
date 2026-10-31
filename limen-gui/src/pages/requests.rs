//! The daemon's recent transcription requests, newest first.

use gpui_kit::component::{ActiveTheme, StyledExt, scroll::ScrollbarMode};
use gpui_kit::*;
use limen_core::{DictationState, HistoryEntry, RequestEntry, RequestOutcome};

use crate::app::AppView;
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

pub fn requests(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let (requests, history, in_flight) = view.model.snapshot.as_ref().map_or_else(
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
                snapshot.state.requests.clone(),
                snapshot.state.history.clone(),
                in_flight,
            )
        },
    );
    let failed = requests
        .iter()
        .filter(|request| matches!(request.outcome, RequestOutcome::Failed { .. }))
        .count();
    let summary = match requests.len() {
        0 => rust_i18n::t!("requests.kept").to_string(),
        count => format!(
            "{count} {} · {} {} · {} {failed}",
            rust_i18n::t!("requests.count"),
            rust_i18n::t!("requests.average"),
            duration(
                requests
                    .iter()
                    .map(|request| request.duration_ms)
                    .sum::<u64>()
                    / count as u64
            ),
            rust_i18n::t!("requests.failed"),
        ),
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
    let rows = requests
        .iter()
        .rev()
        .map(|request| entry(view, request, &history, cx))
        .collect::<Vec<_>>();
    let empty = (rows.is_empty() && live.is_none()).then(|| {
        div()
            .px_4()
            .py_6()
            .text_sm()
            .text_center()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("requests.empty").to_string())
    });
    div()
        .v_flex()
        .flex_1()
        .min_h_0()
        .gap_3()
        .child(
            div()
                .h_flex()
                .items_baseline()
                .justify_between()
                .gap_4()
                .px_1()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .font_medium()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!("requests.title").to_string()),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary),
                ),
        )
        // The page's only scroll container, so a wheel event never moves two
        // scrollers at once.
        .child(
            view.requests_scroll
                .container(
                    div().id("requests").child(
                        div()
                            .v_flex()
                            .on_scroll(view.requests_scroll.on_scroll())
                            .children(live.into_iter().chain(rows).enumerate().map(
                                |(index, row)| match index {
                                    0 => row,
                                    _ => row.border_t_1().border_color(cx.theme().border),
                                },
                            ))
                            .children(empty),
                    ),
                    ScrollbarMode::Scrolling,
                )
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18)),
        )
}

fn entry(
    view: &AppView,
    request: &RequestEntry,
    history: &[HistoryEntry],
    cx: &mut Context<AppView>,
) -> Div {
    match &request.outcome {
        RequestOutcome::Succeeded => {
            let transcript = history
                .iter()
                .filter(|entry| entry.take == request.take)
                .map(|entry| entry.text.as_deref().unwrap_or(&entry.original))
                .collect::<Vec<_>>()
                .join(" ");
            div()
                .v_flex()
                .gap_1()
                .px_4()
                .py_3()
                .child(
                    headline(
                        request.started_ms,
                        dot(cx.theme().success),
                        rust_i18n::t!("requests.succeeded").to_string(),
                        cx,
                    )
                    .child(div().flex_1())
                    .child(took(request.duration_ms, cx)),
                )
                .children((!transcript.is_empty()).then(|| {
                    detail()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(transcript)
                }))
        }
        RequestOutcome::Failed { error } => {
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
                            .child(error.clone())
                            .with_animation(
                                SharedString::from(format!("request-detail-{id}")),
                                motion::enter(),
                                |detail, t| detail.opacity(t),
                            )
                            .into_any_element(),
                        false => error_line
                            .truncate()
                            .child(error.clone())
                            .into_any_element(),
                    }),
            )
        }
    }
}

/// A row's first line: time, status dot, and status; callers append a spacer
/// and the duration.
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
