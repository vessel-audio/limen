//! Recent transcription requests shown below the dictation card.

use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;
use voice_input_core::{DictationState, RequestEntry, RequestOutcome};

use crate::app::AppView;
use crate::motion;

pub fn requests(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let (requests, in_flight) = view.model.snapshot.as_ref().map_or_else(
        || (Vec::new(), None),
        |snapshot| {
            let in_flight = match snapshot.state.dictation.state {
                DictationState::Transcribing => snapshot.state.dictation.since_ms,
                DictationState::Idle
                | DictationState::Starting { .. }
                | DictationState::Recording { .. }
                | DictationState::Committing => None,
            };
            (snapshot.state.requests.clone(), in_flight)
        },
    );
    let failed = requests
        .iter()
        .filter(|request| matches!(request.outcome, RequestOutcome::Failed { .. }))
        .count();
    let summary = match requests.len() {
        0 => None,
        count => Some(format!(
            "{} {} · {} {failed}",
            rust_i18n::t!("requests.average"),
            duration(
                requests
                    .iter()
                    .map(|request| request.duration_ms)
                    .sum::<u64>()
                    / count as u64
            ),
            rust_i18n::t!("requests.failed"),
        )),
    };
    let live = in_flight.map(|since_ms| {
        row()
            .child(dot(cx.theme().primary).with_animation(
                "request-in-flight",
                motion::pulse(),
                |dot, t| dot.opacity(t),
            ))
            .child(time(since_ms, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(rust_i18n::t!("requests.in_flight").to_string()),
            )
            .child(took(
                (std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64)
                    .saturating_sub(since_ms),
                cx,
            ))
    });
    let rows = requests
        .iter()
        .rev()
        .map(|request| entry(view, request, cx))
        .collect::<Vec<_>>();
    let empty = (rows.is_empty() && live.is_none()).then(|| {
        div()
            .px_4()
            .py_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(rust_i18n::t!("requests.empty").to_string())
    });
    div()
        .v_flex()
        .w_full()
        .max_w(px(560.))
        .gap_2()
        .child(
            div()
                .h_flex()
                .items_baseline()
                .justify_between()
                .px_1()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child(rust_i18n::t!("requests.title").to_string()),
                )
                .children(summary.map(|summary| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary)
                })),
        )
        .child(
            div()
                .id("requests")
                .v_flex()
                .max_h(px(212.))
                .overflow_y_scroll()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .children(live.into_iter().chain(rows).enumerate().map(
                    |(index, row)| match index {
                        0 => row,
                        _ => row.border_t_1().border_color(cx.theme().border),
                    },
                ))
                .children(empty),
        )
}

fn entry(view: &AppView, request: &RequestEntry, cx: &mut Context<AppView>) -> Div {
    let started_ms = request.started_ms;
    match &request.outcome {
        RequestOutcome::Succeeded => row()
            .child(dot(cx.theme().success))
            .child(time(started_ms, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(rust_i18n::t!("requests.succeeded").to_string()),
            )
            .child(took(request.duration_ms, cx)),
        RequestOutcome::Failed { error } => {
            let expanded = view.expanded_request == Some(started_ms);
            let summary = row()
                .child(dot(cx.theme().danger))
                .child(time(started_ms, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
                .child(took(request.duration_ms, cx));
            div().child(
                div()
                    .id(SharedString::from(format!("request-{started_ms}")))
                    .v_flex()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().accent.opacity(0.5)))
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.expanded_request = match this.expanded_request == Some(started_ms) {
                            true => None,
                            false => Some(started_ms),
                        };
                    }))
                    .child(summary)
                    .children(expanded.then(|| {
                        // Indent under the summary text, past the dot and time.
                        div()
                            .pl(px(110.))
                            .pr_4()
                            .pb_3()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(error.clone())
                            .with_animation(
                                SharedString::from(format!("request-detail-{started_ms}")),
                                motion::enter(),
                                |detail, t| detail.opacity(t),
                            )
                    })),
            )
        }
    }
}

fn row() -> Div {
    div()
        .h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .px_4()
        .py(px(10.))
        .text_sm()
}

fn dot(color: Hsla) -> Div {
    div().flex_shrink_0().size(px(6.)).rounded_full().bg(color)
}

fn time(at_ms: u64, cx: &mut Context<AppView>) -> Div {
    div()
        .flex_shrink_0()
        .w(px(64.))
        .text_color(cx.theme().muted_foreground)
        .child(super::format_time(at_ms))
}

fn took(duration_ms: u64, cx: &mut Context<AppView>) -> Div {
    div()
        .flex_shrink_0()
        .text_color(cx.theme().muted_foreground)
        .child(duration(duration_ms))
}

fn duration(duration_ms: u64) -> String {
    match duration_ms {
        0..1000 => format!("{duration_ms} ms"),
        _ => format!("{:.1} s", duration_ms as f64 / 1000.),
    }
}
