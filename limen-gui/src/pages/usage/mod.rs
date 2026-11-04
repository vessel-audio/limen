//! The Usage page under Models: one dashboard card per provider, holding
//! what it served over the chosen days as the daemon's ledger counts it,
//! and what it says is left to spend where it reports that. Cards are
//! dragged into the order the user wants, which is kept across launches.

use gpui_kit::component::{
    ActiveTheme, Selectable, StyledExt, button::ButtonVariants, scroll::ScrollbarMode,
};
use gpui_kit::*;
use limen_core::{Balance, HistoryOutcome, Ledger, ModelRole, ProviderUsage, Service, Tally};

use super::models::{logo, service_label};
use crate::a11y;
use crate::app::{AppView, Page};
use crate::bridge::{BridgeCommand, ModelRequest};

mod card;
mod chart;

/// The days the page sums, ending today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Period {
    #[default]
    Week,
    Month,
    Quarter,
}

impl Period {
    fn days(self) -> i64 {
        match self {
            Self::Week => 7,
            Self::Month => 30,
            Self::Quarter => 90,
        }
    }
}

/// The page's state: the ledger, card order, and balances as last read,
/// and when to read them again.
#[derive(Default)]
pub struct UsageState {
    ledger: Option<Result<Ledger, String>>,
    /// The providers in the order the user dragged them into; ones never
    /// dragged follow, costliest first.
    order: Vec<Service>,
    /// The order has been read from its file; from then on the page's own
    /// order is the one kept, so a read in flight during a drag cannot
    /// undo it.
    order_read: bool,
    balances: Vec<(ModelRole, Service, Result<Balance, String>)>,
    period: Period,
    /// A read of the ledger is in flight.
    reading: bool,
    /// The newest finished take when the ledger was last asked for; one
    /// that finishes while the page shows asks again.
    asked_after: Option<Option<u64>>,
    /// The page showed on the last frame.
    visible: bool,
}

impl UsageState {
    pub fn read(&mut self, read: Result<(Ledger, Vec<Service>), String>) {
        self.reading = false;
        self.ledger = Some(read.map(|(ledger, order)| {
            if !self.order_read {
                self.order = order;
                self.order_read = true;
            }
            ledger
        }));
    }

    pub fn balances(&mut self, balances: Vec<(ModelRole, Service, Result<Balance, String>)>) {
        self.balances = balances;
    }
}

/// Keep the page's numbers current; runs every frame. Opening the page asks
/// for the balances, and the ledger is read again on opening and whenever
/// a take finishes while the page shows.
pub fn follow(view: &mut AppView) {
    let visible = view.page == Page::Usage;
    let newest = view.model.snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .state
            .history
            .iter()
            .filter(|entry| !matches!(entry.outcome, HistoryOutcome::Pending))
            .map(|entry| entry.id)
            .max()
    });
    let usage = &mut view.usage;
    let opened = visible && !usage.visible;
    usage.visible = visible;
    let send = |request| {
        let _ = view.commands.try_send(BridgeCommand::Model(request));
    };
    if opened {
        send(ModelRequest::AskBalances);
    }
    if visible && !usage.reading && (opened || usage.asked_after != Some(newest)) {
        usage.reading = true;
        usage.asked_after = Some(newest);
        send(ModelRequest::ReadUsage);
    }
}

/// A provider's card being dragged to a new place.
#[derive(Clone)]
struct DraggedCard(Service);

/// What follows the pointer while a card is dragged: its provider.
struct CardPreview(Service);

impl Render for CardPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_md()
            .child(logo(self.0, cx))
            .child(div().text_sm().font_medium().child(service_label(self.0)))
    }
}

/// `shown` with `dragged` moved to where `onto` is, and the rest of
/// `order` after it, so providers out of view keep their places.
fn moved(shown: &[Service], order: &[Service], dragged: Service, onto: Service) -> Vec<Service> {
    let target = shown.iter().position(|service| *service == onto);
    let mut moved = shown
        .iter()
        .copied()
        .filter(|service| *service != dragged)
        .collect::<Vec<_>>();
    if let Some(target) = target {
        moved.insert(target.min(moved.len()), dragged);
    }
    let rest = order
        .iter()
        .copied()
        .filter(|service| !moved.contains(service))
        .collect::<Vec<_>>();
    moved.into_iter().chain(rest).collect()
}

pub fn usage(view: &AppView, window: &Window, cx: &mut Context<AppView>) -> Div {
    let state = &view.usage;
    let theme = cx.theme().clone();
    let tab = |id: &'static str, key: &str, period: Period| {
        a11y::button(id)
            .ghost()
            .selected(state.period == period)
            .label(rust_i18n::t!(key).to_string())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.usage.period = period;
                cx.notify();
            }))
    };
    let header = div()
        .h_flex()
        .items_center()
        .justify_between()
        .child(super::title(rust_i18n::t!("nav.usage")))
        .child(
            div()
                .h_flex()
                .gap_1()
                .child(tab("usage-week", "usage.week", Period::Week))
                .child(tab("usage-month", "usage.month", Period::Month))
                .child(tab("usage-quarter", "usage.quarter", Period::Quarter)),
        );
    let days = state.period.days();
    let last_day = limen_core::today();
    let first_day = last_day - (days - 1);
    let mut providers = match &state.ledger {
        Some(Ok(ledger)) => ledger.providers(first_day, last_day),
        Some(Err(_)) | None => Vec::new(),
    };
    // A provider that reports a balance has a card, used in these days or not.
    for (_, service, _) in &state.balances {
        if !providers
            .iter()
            .any(|provider| provider.service == *service)
        {
            providers.push(ProviderUsage {
                service: *service,
                models: Vec::new(),
                daily: vec![Tally::default(); days as usize],
                total: Tally::default(),
            });
        }
    }
    providers.sort_by_key(|provider| {
        state
            .order
            .iter()
            .position(|service| *service == provider.service)
            .unwrap_or(usize::MAX)
    });
    let total = providers
        .iter()
        .fold(Tally::default(), |total, provider| total + provider.total);
    let summary = div()
        .h_flex()
        .items_baseline()
        .gap_3()
        .child(
            a11y::status("usage-total", card::cost(&total))
                .text_2xl()
                .font_family("serif")
                .child(card::cost(&total)),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(rust_i18n::t!("usage.requests", count = total.requests).to_string()),
        );
    let shown = providers
        .iter()
        .map(|provider| provider.service)
        .collect::<Vec<_>>();
    // The window less the sidebar, the main area's margin and border, and
    // the page's padding; two cards a row where two fit.
    let width = window.viewport_size().width
        - view.sidebar_mode.width(view.sidebar_width)
        - px(8. + 2. + 64.);
    let columns = match width >= px(2. * 360. + 16.) {
        true => 2,
        false => 1,
    };
    let cards =
        providers
            .iter()
            .map(|provider| {
                let service = provider.service;
                let balance = state
                    .balances
                    .iter()
                    .find(|(_, balanced, _)| *balanced == service)
                    .map(|(_, _, balance)| balance);
                let shown = shown.clone();
                card::card(provider, balance, first_day, cx)
                    .on_drag(DraggedCard(service), |dragged, _, _, cx| {
                        cx.new(|_| CardPreview(dragged.0))
                    })
                    .drag_over::<DraggedCard>({
                        let primary = theme.primary;
                        move |style, _, _, _| style.border_color(primary)
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedCard, _, cx| {
                        if dragged.0 == service {
                            return;
                        }
                        this.usage.order = moved(&shown, &this.usage.order, dragged.0, service);
                        let _ = this.commands.try_send(BridgeCommand::Model(
                            ModelRequest::SaveCardOrder(this.usage.order.clone()),
                        ));
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
    let body = match &state.ledger {
        Some(Err(error)) => Some(
            a11y::alert("usage-error", error.clone())
                .text_sm()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element(),
        ),
        Some(Ok(_)) if cards.is_empty() => Some(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(rust_i18n::t!("usage.empty").to_string())
                .into_any_element(),
        ),
        Some(Ok(_)) => Some(
            div()
                .grid()
                .grid_cols(columns)
                .gap_4()
                .children(cards)
                .into_any_element(),
        ),
        None => None,
    };
    let content = div()
        .v_flex()
        .w_full()
        .gap_6()
        .p_8()
        .on_scroll(view.usage_scroll.on_scroll())
        .child(header)
        .child(summary)
        .children(body);
    view.usage_scroll
        .container(
            div()
                .id("usage-scroll")
                .accessibility_id("usage-scroll")
                .child(content),
            ScrollbarMode::Scrolling,
        )
        .flex_1()
}

#[cfg(test)]
mod tests {
    use limen_core::Service::{DeepSeek, Groq, OpenAi, Together};

    use super::moved;

    #[test]
    fn a_card_takes_the_place_it_is_dropped_on_and_hidden_ones_keep_theirs() {
        let shown = [Groq, DeepSeek, Together];
        assert_eq!(
            [
                moved(&shown, &[OpenAi], Groq, Together),
                moved(&shown, &[], Together, Groq),
            ],
            [
                vec![DeepSeek, Together, Groq, OpenAi],
                vec![Together, Groq, DeepSeek],
            ]
        );
    }
}
