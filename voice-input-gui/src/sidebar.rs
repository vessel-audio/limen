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
use crate::settings::SettingsPage;

const ROW_HEIGHT: f32 = 34.;
const ROW_GAP: f32 = 4.;
/// Sub-rows start in the parent row's label column.
const SUBPAGE_INDENT: f32 = 26.;

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

    pub fn toggled(self) -> Self {
        match self {
            Self::Expanded => Self::Collapsed,
            Self::Collapsed => Self::Expanded,
        }
    }
}

/// The section and page names shown in the title bar for `page`.
pub fn breadcrumb(page: Page) -> (SharedString, SharedString) {
    let (section, name) = match page {
        Page::Dictation => ("nav.section", "nav.dictation"),
        Page::History => ("nav.section", "nav.history"),
        Page::Shortcuts => ("nav.section", "nav.shortcuts"),
        Page::Settings(SettingsPage::General) => ("settings.title", "settings.general"),
        Page::Settings(SettingsPage::About) => ("settings.title", "about.menu"),
    };
    (rust_i18n::t!(section).into(), rust_i18n::t!(name).into())
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
        let (labels, indent) = match self.sidebar_mode {
            SidebarMode::Expanded => (1., SUBPAGE_INDENT),
            SidebarMode::Collapsed => (0., 0.),
        };
        let settings_label: SharedString = rust_i18n::t!("settings.title").into();
        div()
            .v_flex()
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            // Centre the bottom Settings row on the content panel's footer.
            .pb(px(5.))
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
            .child(self.navigation_group("main-nav", main, cx))
            .child(div().flex_1())
            .child(
                self.with_tooltip(
                    row("settings", IconName::Settings2, settings_label.clone()),
                    settings_label,
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
                .child(
                    Icon::new(IconName::ChevronRight)
                        .flex_shrink_0()
                        .size(px(14.))
                        .with_spring(
                            "settings-chevron",
                            SpringAnimation::new(motion::SPRING).to(match self.settings_open {
                                true => std::f32::consts::FRAC_PI_2,
                                false => 0.,
                            }),
                            |icon, angle| icon.rotate(radians(angle)),
                        ),
                )
                .on_click(cx.listener(|this, _, _, _| {
                    this.settings_open = !this.settings_open;
                    if this.settings_open {
                        this.page = Page::Settings(SettingsPage::General);
                    }
                })),
            )
            .children(self.settings_open.then(|| {
                // A guide through the parent icon's centre anchors the
                // indented subpages to their parent.
                let guide = div()
                    .absolute()
                    .top(px(ROW_GAP + 6.))
                    .bottom(px(6.))
                    .left(px(17.5))
                    .w(px(1.))
                    .bg(cx.theme().border)
                    .with_spring(
                        "settings-guide",
                        SpringAnimation::new(motion::SPRING).to(labels),
                        |guide, opacity| guide.opacity(opacity.clamp(0., 1.)),
                    );
                div()
                    .relative()
                    .pt(px(ROW_GAP))
                    .child(guide)
                    .child(
                        div()
                            .child(self.navigation_group("settings-nav", settings, cx))
                            .with_spring(
                                "settings-indent",
                                SpringAnimation::new(motion::SPRING).to(px(indent)),
                                |group, indent| group.pl(indent),
                            ),
                    )
                    .with_animation("settings-nav-enter", motion::enter(), |group, t| {
                        group.opacity(t).top(px(-6. * (1. - t)))
                    })
            }))
            .with_spring(
                "sidebar-width",
                SpringAnimation::new(motion::SPRING).to(self.sidebar_mode.width()),
                |sidebar, width| sidebar.w(width),
            )
    }

    /// Rows sharing one card that springs to the selected row and fades out
    /// while the selection lives in another group.
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
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_xs()
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
                let item = self
                    .with_tooltip(
                        row(destination.id, destination.icon, destination.label.clone()),
                        destination.label,
                    )
                    .on_click(cx.listener(move |this, _, _, _| this.page = page));
                match self.page == page {
                    true => item.text_color(cx.theme().foreground).font_medium(),
                    false => item
                        .text_color(cx.theme().muted_foreground)
                        .hover(|style| style.bg(cx.theme().accent))
                        .active(|style| style.bg(cx.theme().border)),
                }
            }))
    }

    /// Name icon-only rows on hover once their labels are hidden.
    fn with_tooltip(&self, row: Stateful<Div>, label: SharedString) -> Stateful<Div> {
        match self.sidebar_mode {
            SidebarMode::Expanded => row,
            SidebarMode::Collapsed => {
                row.tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            }
        }
    }
}

/// One sidebar row; callers add colours and the click handler.
fn row(id: &'static str, icon: IconName, label: SharedString) -> Stateful<Div> {
    div()
        .id(id)
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
        .child(Icon::new(icon).flex_shrink_0().size(px(16.)))
        .child(div().flex_shrink_0().whitespace_nowrap().child(label))
}
