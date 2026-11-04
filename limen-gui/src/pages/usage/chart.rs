//! A provider's daily use as a column chart: one coral column a day on a
//! hairline baseline, a hairline at the top tick, which is labelled, and
//! the first and last dates below. Each day is its own hover target,
//! taller than its column, whose tooltip and accessible name give the day's
//! value; there is one series, so no legend.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;
use limen_core::Tally;

use super::card::dollars;

/// What the columns measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Measure {
    Cost,
    Requests,
}

/// The plot's height.
const PLOT: f32 = 96.;

pub(super) fn daily(
    prefix: &str,
    days: &[Tally],
    first_day: i64,
    measure: Measure,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let value = |tally: &Tally| match measure {
        Measure::Cost => tally.cost_usd,
        Measure::Requests => tally.requests as f64,
    };
    let shown = |amount: f64| match measure {
        Measure::Cost => dollars(amount),
        Measure::Requests => rust_i18n::t!("usage.requests", count = amount as u64).to_string(),
    };
    let top = clean_ceiling(days.iter().map(value).fold(0., f64::max), measure);
    let (primary, active) = (theme.primary, theme.button_primary_active);
    let columns = days.iter().enumerate().map(|(index, tally)| {
        let amount = value(tally);
        let label = format!("{} · {}", date(first_day + index as i64), shown(amount));
        let height = match amount > 0. {
            // A day with any use stays visible however small.
            true => (amount / top * f64::from(PLOT)).max(2.) as f32,
            false => 0.,
        };
        let id = SharedString::from(format!("{prefix}-day-{index}"));
        let group = SharedString::from(format!("{prefix}-day-{index}-hover"));
        div()
            .id(id.clone())
            .accessibility_id(id)
            .role(Role::ListItem)
            .aria_label(label.clone())
            .group(group.clone())
            .flex_1()
            .h_full()
            .v_flex()
            .justify_end()
            .items_center()
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .child(
                div()
                    .w_full()
                    .max_w(px(24.))
                    .h(px(height))
                    .rounded_t(px(4_f32.min(height)))
                    .bg(primary)
                    .group_hover(group, move |style| style.bg(active)),
            )
    });
    let tick = |text: String| {
        div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    div()
        .v_flex()
        .gap_1()
        .child(tick(match measure {
            Measure::Cost => dollars(top),
            Measure::Requests => format!("{top}"),
        }))
        .child(
            div()
                .h(px(PLOT))
                .border_t_1()
                .border_b_1()
                .border_color(theme.border)
                .h_flex()
                .items_end()
                .gap(px(2.))
                .children(columns),
        )
        .child(
            div()
                .h_flex()
                .justify_between()
                .child(tick(date(first_day)))
                .child(tick(date(first_day + days.len() as i64 - 1))),
        )
}

/// The least clean number, 1, 2, 2.5, or 5 times a power of ten, at or
/// above `max`; for no use at all, one request or a cent.
fn clean_ceiling(max: f64, measure: Measure) -> f64 {
    let floor = match measure {
        Measure::Cost => 0.01,
        Measure::Requests => 1.,
    };
    match max > 0. {
        false => floor,
        true => {
            let power = 10_f64.powf(max.log10().floor());
            [1., 2., 2.5, 5., 10.]
                .into_iter()
                .map(|step| step * power)
                .find(|candidate| *candidate >= max)
                .unwrap_or(10. * power)
                .max(match measure {
                    // Requests come whole.
                    Measure::Requests => 1.,
                    Measure::Cost => 0.,
                })
        }
    }
}

/// `day`, counted in days since the Unix epoch, as month/day.
fn date(day: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let shifted = day + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day_of_month = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = match shifted_month < 10 {
        true => shifted_month + 3,
        false => shifted_month - 9,
    };
    format!("{month}/{day_of_month}")
}

#[cfg(test)]
mod tests {
    use super::{Measure, clean_ceiling, date};

    #[test]
    fn days_since_the_epoch_read_as_their_dates() {
        // 2026-11-04 is day 20,761.
        assert_eq!(
            [date(0), date(20_761), date(20_820)],
            ["1/1", "11/4", "1/2"]
        );
    }

    #[test]
    fn the_top_tick_is_the_next_clean_number() {
        assert_eq!(
            [
                clean_ceiling(0.0034, Measure::Cost),
                clean_ceiling(3.2, Measure::Requests),
                clean_ceiling(0.4, Measure::Requests),
                clean_ceiling(0., Measure::Cost),
            ],
            [0.005, 5., 1., 0.01]
        );
    }
}
