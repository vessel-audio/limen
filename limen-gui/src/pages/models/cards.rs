//! The services a provider page offers, as a grid of cards. The saved one is
//! marked; any card opens the dialog that sets its service up.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt};
use gpui_kit::*;
use limen_core::{ModelRole, Service};

use super::choices::service_label;
use super::dialog;
use crate::app::AppView;
use crate::motion;

pub fn grid(view: &AppView, role: ModelRole, cx: &mut Context<AppView>) -> Div {
    let (form, prefix, services) = match role {
        ModelRole::Asr => (&view.models.asr, "asr", Service::TRANSCRIBING.to_vec()),
        ModelRole::Polish => (&view.models.polish, "polish", Service::POLISHING.to_vec()),
    };
    let active = form.active();
    let theme = cx.theme().clone();
    div()
        .h_flex()
        .flex_wrap()
        .gap_3()
        .children(services.into_iter().enumerate().map(|(index, service)| {
            let id = SharedString::from(format!("{prefix}-provider-{}", service.as_str()));
            let label = service_label(service);
            let marked = active == Some(service);
            div()
                .id(id.clone())
                .accessibility_id(id.clone())
                .role(Role::Button)
                .aria_label(label.clone())
                .w(px(172.))
                .h(px(60.))
                .px_5()
                .h_flex()
                .items_center()
                .gap_3()
                .rounded_xl()
                .border_1()
                .border_color(match marked {
                    true => theme.primary.opacity(0.55),
                    false => theme.border,
                })
                .bg(theme.muted.opacity(0.18))
                .cursor_pointer()
                .hover({
                    let hover = theme.muted.opacity(0.4);
                    move |style| style.bg(hover)
                })
                .active(|style| style.opacity(0.75))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
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
                    |card, t| card.opacity(t).top(px(6. * (1. - t))),
                )
        }))
}
