//! Sidebar navigation with a spring-driven selection highlight.
//!
//! Horizontal rhythm shared with the title bar: navigation icons sit 22px from
//! the window edge and labels start at 48px.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt};
use gpui_kit::*;

use crate::app::{AppView, Page};
use crate::motion;
use crate::settings::SettingsPage;

const ROW_HEIGHT: f32 = 34.;
const ROW_GAP: f32 = 4.;

/// One navigable sidebar destination.
struct Destination {
    id: &'static str,
    icon: IconName,
    label: SharedString,
    page: Page,
}

impl AppView {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let main = [
            Destination {
                id: "dictation-page",
                icon: IconName::Mic,
                label: rust_i18n::t!("nav.dictation").into(),
                page: Page::Dictation,
            },
            Destination {
                id: "history-page",
                icon: IconName::Clock,
                label: rust_i18n::t!("nav.history").into(),
                page: Page::History,
            },
            Destination {
                id: "shortcuts-page",
                icon: IconName::Keyboard,
                label: rust_i18n::t!("nav.shortcuts").into(),
                page: Page::Shortcuts,
            },
        ];
        let settings = [
            Destination {
                id: "settings-general",
                icon: IconName::SlidersHorizontal,
                label: rust_i18n::t!("settings.general").into(),
                page: Page::Settings(SettingsPage::General),
            },
            Destination {
                id: "settings-about",
                icon: IconName::Info,
                label: rust_i18n::t!("about.menu").into(),
                page: Page::Settings(SettingsPage::About),
            },
        ];
        div()
            .v_flex()
            .w(px(192.))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .px_3()
            // Centre the bottom Settings row on the content panel's footer.
            .pb(px(5.))
            .child(self.navigation_group("main-nav", main, cx))
            .child(div().flex_1())
            .child(
                row(
                    "settings",
                    IconName::Settings2,
                    rust_i18n::t!("settings.title"),
                )
                .text_color(match self.page {
                    Page::Settings(_) => cx.theme().foreground,
                    Page::Dictation | Page::History | Page::Shortcuts => {
                        cx.theme().muted_foreground
                    }
                })
                .hover(|style| style.bg(cx.theme().accent))
                .active(|style| style.bg(cx.theme().border))
                .child(div().flex_1())
                .child(Icon::new(IconName::ChevronRight).size(px(14.)).with_spring(
                    "settings-chevron",
                    SpringAnimation::new(motion::SPRING).to(match self.settings_open {
                        true => std::f32::consts::FRAC_PI_2,
                        false => 0.,
                    }),
                    |icon, angle| icon.rotate(radians(angle)),
                ))
                .on_click(cx.listener(|this, _, _, _| {
                    this.settings_open = !this.settings_open;
                    if this.settings_open {
                        this.page = Page::Settings(SettingsPage::General);
                    }
                })),
            )
            .children(self.settings_open.then(|| {
                // Indent subpage icons into the parent's label column.
                div()
                    .relative()
                    .pt(px(ROW_GAP))
                    .pl(px(26.))
                    .child(self.navigation_group("settings-nav", settings, cx))
                    .with_animation("settings-nav-enter", motion::enter(), |group, t| {
                        group.opacity(t).top(px(-6. * (1. - t)))
                    })
            }))
    }

    /// Rows sharing one highlight that springs to the selected row and fades
    /// out while the selection lives in another group.
    fn navigation_group<const N: usize>(
        &self,
        id: &'static str,
        destinations: [Destination; N],
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = destinations
            .iter()
            .position(|destination| destination.page == self.page);
        let highlight = div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(ROW_HEIGHT))
            .child(
                div()
                    .size_full()
                    .rounded_lg()
                    .bg(cx.theme().primary.opacity(0.12))
                    .with_spring(
                        SharedString::from(format!("{id}-highlight-opacity")),
                        SpringAnimation::new(motion::SPRING).to(match selected {
                            Some(_) => 1.,
                            None => 0.,
                        }),
                        |highlight, opacity| highlight.opacity(opacity.clamp(0., 1.)),
                    ),
            )
            .with_spring(
                SharedString::from(format!("{id}-highlight-top")),
                SpringAnimation::new(motion::SPRING).to(px(
                    selected.unwrap_or_default() as f32 * (ROW_HEIGHT + ROW_GAP)
                )),
                |highlight, top| highlight.top(top),
            );
        div()
            .relative()
            .v_flex()
            .gap(px(ROW_GAP))
            .child(highlight)
            .children(destinations.into_iter().map(|destination| {
                let page = destination.page;
                let item = row(destination.id, destination.icon, destination.label)
                    .on_click(cx.listener(move |this, _, _, _| this.page = page));
                match self.page == page {
                    true => item.text_color(cx.theme().primary).font_medium(),
                    false => item
                        .text_color(cx.theme().muted_foreground)
                        .hover(|style| style.bg(cx.theme().accent))
                        .active(|style| style.bg(cx.theme().border)),
                }
            }))
    }
}

/// One sidebar row; callers add colours and the click handler.
fn row(id: &'static str, icon: IconName, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .h_flex()
        .w_full()
        .h(px(ROW_HEIGHT))
        .items_center()
        .gap(px(10.))
        .rounded_lg()
        .px(px(10.))
        .text_sm()
        .cursor_pointer()
        .child(Icon::new(icon).size(px(16.)))
        .child(label.into())
}
