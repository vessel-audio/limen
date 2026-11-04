//! Collapsible sidebar navigation with a spring-driven selection card.
//!
//! Horizontal rhythm shared with the title bar: navigation icons sit 22px from
//! the window edge and labels start at 48px. Collapsing narrows the sidebar to
//! its icon column, which keeps every icon in place. The expanded sidebar is
//! as wide as its edge is dragged; dragged narrower still, it collapses, and
//! dragged back out, it expands again. Collapsing keeps the width it was
//! dragged to, which the toggle and a drag back out both reopen it at. Settings sit at the sidebar's foot.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, StyledExt, tooltip::Tooltip};
use gpui_kit::*;

use crate::app::{AppView, Page};
use crate::motion;
use crate::settings::SettingsPage;

const ROW_HEIGHT: f32 = 34.;
const ROW_GAP: f32 = 4.;
/// How wide the expanded sidebar starts, and the widths its edge can be
/// dragged between.
pub const DEFAULT_WIDTH: Pixels = px(168.);
const MIN_WIDTH: f32 = 140.;
const MAX_WIDTH: f32 = 280.;
/// The width below which a dragged sidebar collapses, between the
/// collapsed width and the narrowest expanded one.
const COLLAPSE_BELOW: f32 = 100.;
/// How wide the draggable edge is.
const EDGE: f32 = 8.;

/// Whether the sidebar shows labels or only icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMode {
    Expanded,
    Collapsed,
}

impl SidebarMode {
    /// The sidebar's width, `expanded` while it shows its labels, which the
    /// title bar's brand area follows.
    pub fn width(self, expanded: Pixels) -> Pixels {
        match self {
            Self::Expanded => expanded,
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

/// A press on the sidebar's edge, and the drag that may follow.
#[derive(Debug, Clone, Copy)]
pub struct EdgePress {
    /// Where the edge was pressed.
    at: Pixels,
    /// How wide the sidebar was drawn when the drag started, which the
    /// toggle's spring may not have reached yet.
    drawn: Option<Pixels>,
    /// The width the user had set before the press.
    kept: Pixels,
    mode: SidebarMode,
}

/// The sidebar's edge being dragged; it draws nothing under the pointer.
#[derive(Clone)]
struct Resizing;

impl Render for Resizing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// One navigable sidebar destination.
struct Destination {
    id: &'static str,
    lead: Lead,
    label: SharedString,
    page: Page,
}

/// What a row shows in the icon column.
enum Lead {
    Icon(IconName),
    /// Nothing, so a subpage's label lines up under its parent's.
    Indent,
}

impl AppView {
    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = [
            Destination {
                id: "dictation-page",
                lead: Lead::Icon(IconName::Mic),
                label: rust_i18n::t!("nav.dictation").into(),
                page: Page::Dictation,
            },
            Destination {
                id: "shortcuts-page",
                lead: Lead::Icon(IconName::Keyboard),
                label: rust_i18n::t!("nav.shortcuts").into(),
                page: Page::Shortcuts,
            },
        ];
        let models = [
            Destination {
                id: "providers-page",
                lead: Lead::Icon(IconName::Boxes),
                label: rust_i18n::t!("nav.providers").into(),
                page: Page::Providers,
            },
            Destination {
                id: "usage-page",
                lead: Lead::Icon(IconName::ChartColumn),
                label: rust_i18n::t!("nav.usage").into(),
                page: Page::Usage,
            },
        ];
        div()
            .v_flex()
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .gap_4()
            .child(self.group(
                "navigation",
                rust_i18n::t!("nav.section").into(),
                workspace,
                cx,
            ))
            .child(self.group(
                "model-navigation",
                rust_i18n::t!("nav.models").into(),
                models,
                cx,
            ))
            .child(self.settings(cx))
            .with_spring(
                "sidebar-width",
                SpringAnimation::new(motion::SPRING)
                    .to(self.sidebar_mode.width(self.sidebar_width)),
                |sidebar, width| sidebar.w(width),
            )
    }

    /// The sidebar's edge, dragged to set its width, or below
    /// [`COLLAPSE_BELOW`] to collapse it; a collapsed one dragged back out
    /// expands. The expanded width kept is the last one dragged to, so the
    /// toggle opens the sidebar as wide as it was. The edge lies over the
    /// sidebar's own right padding, so it moves nothing; the sidebar starts
    /// at the window's left edge.
    pub(crate) fn sidebar_edge(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-edge")
            .h_full()
            .w(px(EDGE))
            .ml(px(-EDGE))
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeLeftRight)
            // The press, not where the drag starts a few pixels on, is what
            // the edge follows, so it neither jumps nor lags the pointer.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.sidebar_press = Some(EdgePress {
                        at: event.position.x,
                        drawn: None,
                        kept: this.sidebar_width,
                        mode: this.sidebar_mode,
                    });
                }),
            )
            .on_drag(Resizing, |drag, _, _, cx| cx.new(|_| drag.clone()))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<Resizing>, _, cx| {
                let Some(press) = this.sidebar_press else {
                    return;
                };
                let drawn = press.drawn.unwrap_or(event.bounds.right());
                this.sidebar_press = Some(EdgePress {
                    drawn: Some(drawn),
                    ..press
                });
                let wanted = f32::from(drawn + event.event.position.x - press.at);
                // A collapsed sidebar dragged out opens as wide as it was
                // kept; only a drag past that widens it.
                let narrowest = match press.mode {
                    SidebarMode::Expanded => MIN_WIDTH,
                    SidebarMode::Collapsed => f32::from(press.kept),
                };
                match wanted < COLLAPSE_BELOW {
                    // Collapsing keeps the width set before this drag, not
                    // the one it passed on its way in.
                    true => {
                        this.sidebar_mode = SidebarMode::Collapsed;
                        this.sidebar_width = press.kept;
                    }
                    false => {
                        this.sidebar_mode = SidebarMode::Expanded;
                        this.sidebar_width = px(wanted.clamp(narrowest, MAX_WIDTH));
                    }
                }
                cx.notify();
            }))
    }

    /// One labelled group of destinations, a tab list named `id`.
    fn group<const N: usize>(
        &self,
        id: &'static str,
        title: SharedString,
        destinations: [Destination; N],
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = destinations
            .iter()
            .position(|destination| destination.page == self.page);
        div()
            .v_flex()
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
                    .child(title.clone())
                    .with_spring(
                        SharedString::from(format!("{id}-label")),
                        SpringAnimation::new(motion::SPRING).to(self.sidebar_mode.label_opacity()),
                        |label, opacity| label.opacity(opacity.clamp(0., 1.)),
                    ),
            )
            .child(
                div()
                    .id(id)
                    .accessibility_id(id)
                    .role(Role::TabList)
                    .aria_label(title)
                    .relative()
                    .v_flex()
                    .gap(px(ROW_GAP))
                    .child(highlight(id, selected, cx))
                    .children(
                        destinations
                            .into_iter()
                            .map(|destination| self.row(destination, cx)),
                    ),
            )
    }

    /// Settings at the foot of the sidebar. While a settings page is shown,
    /// its subpages unfold below it in the expanded sidebar; the collapsed
    /// one has no room for their labels, so the settings page lists them
    /// at its top instead.
    fn settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let title: SharedString = rust_i18n::t!("settings.title").into();
        let subpages = [
            Destination {
                id: "settings-tab-general",
                lead: Lead::Indent,
                label: rust_i18n::t!("settings.general").into(),
                page: Page::Settings(SettingsPage::General),
            },
            Destination {
                id: "settings-tab-network",
                lead: Lead::Indent,
                label: rust_i18n::t!("settings.network").into(),
                page: Page::Settings(SettingsPage::Network),
            },
            Destination {
                id: "settings-tab-about",
                lead: Lead::Indent,
                label: rust_i18n::t!("about.menu").into(),
                page: Page::Settings(SettingsPage::About),
            },
        ];
        let (open, current) = match (self.sidebar_mode, self.page) {
            (SidebarMode::Expanded, Page::Settings(page)) => (true, page),
            (SidebarMode::Collapsed, Page::Settings(page)) => (false, page),
            (
                SidebarMode::Expanded | SidebarMode::Collapsed,
                Page::Dictation | Page::Shortcuts | Page::Providers | Page::Usage,
            ) => (false, SettingsPage::General),
        };
        // The highlight rests on the open subpage, or on Settings itself
        // while its subpages are folded away.
        let selected = match (open, self.page) {
            (true, page) => subpages
                .iter()
                .position(|subpage| subpage.page == page)
                .map(|index| index + 1),
            (false, Page::Settings(_)) => Some(0),
            (false, Page::Dictation | Page::Shortcuts | Page::Providers | Page::Usage) => None,
        };
        let unfolded = match open {
            true => subpages.len() as f32 * (ROW_HEIGHT + ROW_GAP),
            false => 0.,
        };
        // Rows exist only while unfolded, so a folded list leaves no tabs
        // behind for assistive technology to find.
        let rows = match open {
            true => subpages
                .into_iter()
                .map(|subpage| self.row(subpage, cx).mt(px(ROW_GAP)))
                .collect(),
            false => Vec::new(),
        };
        div()
            .id("settings-navigation")
            .accessibility_id("settings-navigation")
            .role(Role::TabList)
            .aria_label(title.clone())
            .relative()
            .mt_auto()
            .pb_1()
            .v_flex()
            .child(highlight("settings-navigation", selected, cx))
            .child(self.row(
                Destination {
                    id: "settings-page",
                    lead: Lead::Icon(IconName::Settings),
                    label: title.clone(),
                    page: Page::Settings(current),
                },
                cx,
            ))
            .child(
                div()
                    .id("settings-tabs")
                    .accessibility_id("settings-tabs")
                    .role(Role::TabList)
                    .aria_label(title)
                    .v_flex()
                    .overflow_hidden()
                    .children(rows)
                    .with_spring(
                        "settings-tabs-height",
                        SpringAnimation::new(motion::SPRING).to(px(unfolded)),
                        |list, height| list.h(height.max(px(0.))),
                    ),
            )
    }

    /// One destination's row, selected while its page is shown.
    fn row(&self, destination: Destination, cx: &mut Context<Self>) -> Stateful<Div> {
        let page = destination.page;
        let label = destination.label.clone();
        let lead = match destination.lead {
            Lead::Icon(icon) => Icon::new(icon)
                .flex_shrink_0()
                .size(px(16.))
                .into_any_element(),
            Lead::Indent => div().flex_shrink_0().size(px(16.)).into_any_element(),
        };
        let row = div()
            .id(destination.id)
            .role(Role::Tab)
            .accessibility_id(destination.id)
            .aria_label(label.clone())
            .aria_selected(self.page == page)
            .h_flex()
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex_shrink_0()
            .items_center()
            .gap(px(10.))
            .rounded_lg()
            .px(px(10.))
            .overflow_hidden()
            .text_sm()
            .cursor_pointer()
            .child(lead)
            // Fade as well as clip, so no glyph edge peeks past the icon
            // column while collapsed.
            .child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(destination.label)
                    .with_spring(
                        SharedString::from(format!("{}-label", destination.id)),
                        SpringAnimation::new(motion::SPRING).to(self.sidebar_mode.label_opacity()),
                        |label, opacity| label.opacity(opacity.clamp(0., 1.)),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, _| this.page = page));
        let row = match self.sidebar_mode {
            SidebarMode::Expanded => row,
            SidebarMode::Collapsed => {
                row.tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            }
        };
        match self.page == page {
            true => row.text_color(cx.theme().foreground).font_medium(),
            false => row
                .text_color(cx.theme().muted_foreground)
                .hover(|style| style.bg(cx.theme().accent))
                .active(|style| style.bg(cx.theme().border)),
        }
    }
}

/// The card behind the `selected`th row of the tab list named `id`, which
/// springs from row to row and fades out while none of its rows is selected.
fn highlight(id: &str, selected: Option<usize>, cx: &App) -> impl IntoElement {
    div()
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
        )
}
