//! Collapsible sidebar navigation with a spring-driven selection card.
//!
//! Horizontal rhythm shared with the title bar: navigation icons sit 22px from
//! the window edge and labels start at 48px. Collapsing narrows the sidebar to
//! its icon column, which keeps every icon in place.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, tooltip::Tooltip};
use gpui_kit::*;

use crate::app::{AppView, Page};
use crate::motion;

const ROW_HEIGHT: f32 = 34.;
const ROW_GAP: f32 = 4.;

/// Whether the sidebar shows labels or only icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMode {
    Expanded,
    Collapsed,
}

impl SidebarMode {
    /// The sidebar's width, which the title bar's brand area follows.
    pub fn width(self) -> Pixels {
        match self {
            Self::Expanded => px(192.),
            Self::Collapsed => px(60.),
        }
    }

    /// Opacity of text that only fits the expanded sidebar.
    pub fn label_opacity(self) -> f32 {
        match self {
            Self::Expanded => 1.,
            Self::Collapsed => 0.,
        }
    }
}

/// One navigable sidebar destination.
struct Destination {
    id: &'static str,
    icon: IconName,
    label: SharedString,
    page: Page,
}

impl AppView {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let destinations = [
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
        let labels = self.sidebar_mode.label_opacity();
        let selected = destinations
            .iter()
            .position(|destination| destination.page == self.page);
        // One card springs to the selected row and fades out while a page
        // outside the sidebar, such as Settings, is shown.
        let highlight = div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(ROW_HEIGHT))
            .child(
                div()
                    .size_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_xs()
                    .with_spring(
                        "nav-highlight-opacity",
                        SpringAnimation::new(motion::SPRING).to(match selected {
                            Some(_) => 1.,
                            None => 0.,
                        }),
                        |highlight, opacity| highlight.opacity(opacity.clamp(0., 1.)),
                    ),
            )
            .with_spring(
                "nav-highlight-top",
                SpringAnimation::new(motion::SPRING).to(px(
                    selected.unwrap_or_default() as f32 * (ROW_HEIGHT + ROW_GAP)
                )),
                |highlight, top| highlight.top(top),
            );
        div()
            .v_flex()
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .child(
                div()
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .text_xs()
                    .font_medium()
                    .text_color(cx.theme().muted_foreground)
                    .child(rust_i18n::t!("nav.section").to_string())
                    .with_spring(
                        "section-label",
                        SpringAnimation::new(motion::SPRING).to(labels),
                        |label, opacity| label.opacity(opacity.clamp(0., 1.)),
                    ),
            )
            .child(
                div()
                    .id("navigation")
                    .role(Role::TabList)
                    .aria_label(rust_i18n::t!("nav.section").to_string())
                    .relative()
                    .v_flex()
                    .gap(px(ROW_GAP))
                    .child(highlight)
                    .children(destinations.into_iter().map(|destination| {
                        let page = destination.page;
                        let label = destination.label.clone();
                        let row = div()
                            .id(destination.id)
                            .role(Role::Tab)
                            .accessibility_id(destination.id)
                            .aria_label(label.clone())
                            .aria_selected(self.page == page)
                            .h_flex()
                            .w_full()
                            .h(px(ROW_HEIGHT))
                            .items_center()
                            .gap(px(10.))
                            .rounded_lg()
                            .px(px(10.))
                            .overflow_hidden()
                            .text_sm()
                            .cursor_pointer()
                            .child(Icon::new(destination.icon).flex_shrink_0().size(px(16.)))
                            // Fade as well as clip, so no glyph edge peeks
                            // past the icon column while collapsed.
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .child(destination.label)
                                    .with_spring(
                                        SharedString::from(format!("{}-label", destination.id)),
                                        SpringAnimation::new(motion::SPRING).to(labels),
                                        |label, opacity| label.opacity(opacity.clamp(0., 1.)),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, _, _| this.page = page));
                        let row = match self.sidebar_mode {
                            SidebarMode::Expanded => row,
                            SidebarMode::Collapsed => row.tooltip(move |window, cx| {
                                Tooltip::new(label.clone()).build(window, cx)
                            }),
                        };
                        match self.page == page {
                            true => row.text_color(cx.theme().foreground).font_medium(),
                            false => row
                                .text_color(cx.theme().muted_foreground)
                                .hover(|style| style.bg(cx.theme().accent))
                                .active(|style| style.bg(cx.theme().border)),
                        }
                    })),
            )
            .with_spring(
                "sidebar-width",
                SpringAnimation::new(motion::SPRING).to(self.sidebar_mode.width()),
                |sidebar, width| sidebar.w(width),
            )
    }
}
