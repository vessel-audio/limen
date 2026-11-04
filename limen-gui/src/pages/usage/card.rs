//! One provider's dashboard card: what it served over the chosen days, the
//! rate it was used at, its daily use as a chart, and each of its models.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt};
use gpui_kit::*;
use limen_core::{Balance, ModelRole, ProviderUsage, Tally, UsageRow};

use super::chart::{self, Measure};
use super::{logo, service_label};

pub(super) fn card(
    provider: &ProviderUsage,
    balance: Option<&Result<Balance, String>>,
    first_day: i64,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let service = provider.service;
    let prefix = format!("usage-{}", service.as_str());
    let total = &provider.total;
    let days = provider.daily.len().max(1) as f64;
    // A provider whose answers had a price is charted by cost; one whose
    // answers never had one, by requests.
    let measure = match total.cost_usd > 0. {
        true => Measure::Cost,
        false => Measure::Requests,
    };
    let per_day = match measure {
        Measure::Cost => total.cost_usd / days,
        Measure::Requests => total.requests as f64 / days,
    };
    let rate = match measure {
        Measure::Cost => dollars(per_day),
        Measure::Requests => format!("{per_day:.1}"),
    };
    let balance_text = balance.map(|balance| match balance {
        Ok(Balance::Remaining(money)) => money
            .iter()
            .map(|money| {
                let amount = format!("{} {}", money.amount, money.currency);
                let left = rust_i18n::t!("usage.left", amount = amount).to_string();
                // How long the balance lasts at this rate, where both are in
                // the same currency.
                match (
                    money.currency.as_str(),
                    money.amount.parse::<f64>(),
                    measure,
                ) {
                    ("USD", Ok(amount), Measure::Cost) if per_day > 0. => {
                        let days = (amount / per_day).floor() as u64;
                        format!("{left} · {}", rust_i18n::t!("usage.lasts", days = days))
                    }
                    _ => left,
                }
            })
            .collect::<Vec<_>>()
            .join(" · "),
        Ok(Balance::Spent(money)) => {
            let amount = format!("{} {}", money.amount, money.currency);
            rust_i18n::t!("usage.spent", amount = amount).to_string()
        }
        Err(error) => error.clone(),
    });
    let figure = |id: String, label: String, value: String| {
        div()
            .v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .id(SharedString::from(id.clone()))
                    .accessibility_id(id)
                    .role(Role::Status)
                    .aria_label(value.clone())
                    .text_lg()
                    .font_semibold()
                    .child(value),
            )
    };
    let requests = match total.failures {
        0 => total.requests.to_string(),
        failures => format!(
            "{} · {}",
            total.requests,
            rust_i18n::t!("usage.failed", count = failures)
        ),
    };
    let id = SharedString::from(format!("usage-card-{}", service.as_str()));
    div()
        .id(id.clone())
        .accessibility_id(id)
        .role(Role::Group)
        .aria_label(service_label(service).to_string())
        .v_flex()
        .gap_5()
        .p_6()
        .rounded_xl()
        // Canvas with a hairline, not a cream card: the chart's coral keeps
        // its contrast on the canvas, and falls short of 3:1 on cream.
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .cursor(CursorStyle::OpenHand)
                .child(
                    Icon::new(IconName::GripVertical)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(logo(service, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_medium()
                        .child(service_label(service)),
                )
                .children(balance_text.map(|text| {
                    let id = SharedString::from(format!("{prefix}-balance"));
                    div()
                        .id(id.clone())
                        .accessibility_id(id)
                        .role(Role::Status)
                        .aria_label(text.clone())
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(match balance {
                            Some(Err(_)) => theme.danger,
                            Some(Ok(_)) | None => theme.muted_foreground,
                        })
                        .child(text)
                })),
        )
        .child(
            div()
                .h_flex()
                .gap_8()
                .child(figure(
                    format!("{prefix}-cost"),
                    rust_i18n::t!("usage.cost").to_string(),
                    cost(total),
                ))
                .child(figure(
                    format!("{prefix}-rate"),
                    rust_i18n::t!("usage.per_day").to_string(),
                    rate,
                ))
                .child(figure(
                    format!("{prefix}-requests"),
                    rust_i18n::t!("usage.request_count").to_string(),
                    requests,
                )),
        )
        .child(chart::daily(
            &prefix,
            &provider.daily,
            first_day,
            measure,
            cx,
        ))
        .children((!provider.models.is_empty()).then(|| {
            div().v_flex().children(
                provider
                    .models
                    .iter()
                    .enumerate()
                    .map(|(index, row)| model_row(row, &prefix, index, cx)),
            )
        }))
}

/// One model's lines: its role, name, and cost, then its requests and what
/// it used, so a narrow card never squeezes the name out.
fn model_row(row: &UsageRow, prefix: &str, index: usize, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let tally = &row.tally;
    let (role, used) = match row.role {
        ModelRole::Asr => {
            let seconds = tally.audio_ms / 1000;
            (
                rust_i18n::t!("usage.asr"),
                format!("{}:{:02}", seconds / 60, seconds % 60),
            )
        }
        ModelRole::Polish => (
            rust_i18n::t!("usage.polish"),
            rust_i18n::t!(
                "usage.tokens",
                input = compact(tally.input_tokens),
                output = compact(tally.output_tokens)
            )
            .to_string(),
        ),
    };
    let requests = rust_i18n::t!("usage.requests", count = tally.requests).to_string();
    let id = SharedString::from(format!("{prefix}-model-{index}"));
    let label = format!(
        "{} · {role} · {requests} · {used} · {}",
        row.model,
        cost(tally)
    );
    div()
        .id(id.clone())
        .accessibility_id(id)
        .role(Role::ListItem)
        .aria_label(label)
        .v_flex()
        .gap_0p5()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .h_flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(role.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(row.model.clone()),
                )
                .child(div().flex_shrink_0().text_sm().child(cost(tally))),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .truncate()
                .child(format!("{requests} · {used}")),
        )
}

/// `amount` in dollars, to four places below a cent.
pub(super) fn dollars(amount: f64) -> String {
    match amount < 0.01 {
        true => format!("${amount:.4}"),
        false => format!("${amount:.2}"),
    }
}

/// What `tally` cost: in dollars, as a lower bound when some answers had
/// no price, or a dash when none had one.
pub(super) fn cost(tally: &Tally) -> String {
    let answered = tally.requests - tally.failures;
    match (answered, tally.unpriced) {
        (0, _) => "$0.00".to_string(),
        (answered, unpriced) if unpriced == answered => "—".to_string(),
        (_, 0) => dollars(tally.cost_usd),
        (_, _) => format!("≥ {}", dollars(tally.cost_usd)),
    }
}

/// `count` in thousands or millions, to one decimal.
fn compact(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}K", count as f64 / 1e3),
        _ => format!("{:.1}M", count as f64 / 1e6),
    }
}
