//! The services a provider page offers, as a grid of cards: subscriptions,
//! signed in to, apart from the APIs a key or a server reaches. The saved
//! one is marked; any card opens the dialog that sets its service up.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt};
use gpui_kit::*;
use limen_core::{ModelRole, Service};

use super::choices::{logo, service_label};
use super::dialog;
use crate::app::AppView;
use crate::motion;

/// The space inside a card around its logo, name, and chevron.
const CARD_PADDING: Pixels = px(20.);

pub fn grid(view: &AppView, role: ModelRole, cx: &mut Context<AppView>) -> Div {
    let (form, prefix, services) = match role {
        ModelRole::Asr => (&view.models.asr, "asr", Service::TRANSCRIBING.to_vec()),
        ModelRole::Polish => (&view.models.polish, "polish", Service::POLISHING.to_vec()),
    };
    let active = form.active();
    // Cards open nothing until the settings they fill the dialog from load.
    let loaded = form.loaded.is_some();
    let theme = cx.theme().clone();
    let (subscriptions, apis): (Vec<_>, Vec<_>) = services
        .into_iter()
        .enumerate()
        .partition(|(_, service)| service.subscription().is_some());
    // A page with only one kind needs no headings.
    let headed = !subscriptions.is_empty() && !apis.is_empty();
    let group = |title: SharedString, cards: Vec<AnimationElement<Stateful<Div>>>| {
        div()
            .v_flex()
            .gap_3()
            .children(headed.then(|| {
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .child(title)
            }))
            // Cards show no edges until lifted, so their contents line up
            // with the heading, the cards reaching into the page's margin.
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .gap_3()
                    .ml(-CARD_PADDING)
                    .children(cards),
            )
    };
    let card =
        |(index, service): (usize, Service)| {
            let id = SharedString::from(format!("{prefix}-provider-{}", service.as_str()));
            let label = service_label(service);
            let marked = active == Some(service);
            div()
                .id(id.clone())
                .accessibility_id(id.clone())
                .role(Role::Button)
                .aria_label(label.clone())
                .w(px(200.))
                .h(px(60.))
                .px(CARD_PADDING)
                .h_flex()
                .items_center()
                .gap_3()
                .rounded_xl()
                // Borderless and on the page's own background: a card shows
                // its edges only as it lifts under the pointer.
                .bg(theme.background)
                .cursor(match loaded {
                    true => CursorStyle::PointingHand,
                    false => CursorStyle::Arrow,
                })
                .hover(move |style| match loaded {
                    true => style.shadow_md(),
                    false => style,
                })
                .active(move |style| match loaded {
                    true => style.opacity(0.75),
                    false => style,
                })
                .child(logo(service, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .child(label),
                )
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(theme.primary)
                        .with_spring(
                            SharedString::from(format!("{id}-marked")),
                            SpringAnimation::new(motion::SPRING).to(match marked {
                                true => 1.,
                                false => 0.,
                            }),
                            |dot, shown| {
                                let shown = shown.clamp(0., 1.);
                                dot.opacity(shown).size(px(8. * shown))
                            },
                        ),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    dialog::open(this, role, service, window, cx)
                }))
                .with_animation(
                    SharedString::from(format!("{id}-enter")),
                    motion::stagger(index),
                    move |card, t| {
                        let rest = match loaded {
                            true => 1.,
                            false => 0.55,
                        };
                        card.opacity(rest * t).top(px(6. * (1. - t)))
                    },
                )
        };
    let [subscriptions, apis] =
        [subscriptions, apis].map(|services| services.into_iter().map(&card).collect::<Vec<_>>());
    div()
        .v_flex()
        .gap_5()
        .children(
            (!subscriptions.is_empty())
                .then(|| group(rust_i18n::t!("models.subscriptions").into(), subscriptions)),
        )
        .child(group(rust_i18n::t!("models.apis").into(), apis))
}
