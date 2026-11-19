//! Sidebar navigation, its collapse and resize, the settings subpages, and
//! keyboard traversal. The Linux desktop suite keeps the tooltips these
//! controls show, which the test observer does not see.

use gpui_kit::{TestAppContext, point, px};

use super::Ui;

/// The sidebar's page tabs, top to bottom.
const TABS: [&str; 4] = [
    "dictation-page",
    "shortcuts-page",
    "providers-page",
    "usage-page",
];

/// The rendered width of the element with this ID.
fn width(ui: &mut Ui, id: &'static str) -> f32 {
    f32::from(ui.find(id).bounds().size.width).round()
}

/// Each sidebar tab selects itself alone and names its page in the title
/// bar.
#[gpui_kit::test]
fn sidebar_switches_pages(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    for (index, tab) in TABS.into_iter().enumerate().rev() {
        ui.click(tab);
        let selected = TABS.map(|tab| ui.find(tab).selected());
        let expected = std::array::from_fn(|other| Some(other == index));
        assert_eq!(selected, expected, "after clicking {tab}");
        let named = ui.find(tab).label().map(str::to_string);
        assert_eq!(
            ui.find("current-page").label().map(str::to_string),
            named,
            "the title bar does not name the page of {tab}"
        );
    }
}

/// Collapsing narrows the sidebar to its icons, which still navigate;
/// expanding restores the width.
#[gpui_kit::test]
fn sidebar_collapses_to_icons_and_back(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    let expanded = width(&mut ui, "shortcuts-page");
    ui.click("toggle-sidebar");
    let collapsed = width(&mut ui, "shortcuts-page");
    ui.click("shortcuts-page");
    let navigated = ui.find("shortcuts-page").selected();
    ui.click("toggle-sidebar");
    assert_eq!(
        (
            collapsed * 3. < expanded,
            navigated,
            width(&mut ui, "shortcuts-page")
        ),
        (true, Some(true), expanded),
        "the tab was {expanded}px wide expanded and {collapsed}px collapsed"
    );
}

/// Dragging the sidebar's edge resizes it; dragged far enough in, it
/// collapses to its icons. The width it was dragged to is kept: the toggle
/// and a drag back out both reopen it that wide, and a drag from there
/// resizes it again.
#[gpui_kit::test]
fn dragging_the_sidebar_edge_resizes_and_collapses_it(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    // A tab spans the sidebar but its 12px padding on each side; the edge
    // lies over the right padding, its middle 8px past the tab.
    let edge = |ui: &mut Ui| {
        let bounds = ui.find("shortcuts-page").bounds();
        (bounds.right() + px(8.), bounds.center().y)
    };
    let start = width(&mut ui, "shortcuts-page");
    let (x, y) = edge(&mut ui);
    ui.drag(point(x, y), point(x + px(60.), y));
    let wider = width(&mut ui, "shortcuts-page");
    let (x, _) = edge(&mut ui);
    ui.drag(point(x, y), point(px(40.), y));
    let collapsed = width(&mut ui, "shortcuts-page");
    ui.click("toggle-sidebar");
    let toggled = width(&mut ui, "shortcuts-page");
    ui.click("toggle-sidebar");
    let (x, _) = edge(&mut ui);
    ui.drag(point(x, y), point(x + px(120.), y));
    let reopened = width(&mut ui, "shortcuts-page");
    let (x, _) = edge(&mut ui);
    ui.drag(point(x, y), point(x + px(30.), y));
    let widened = width(&mut ui, "shortcuts-page");
    assert_eq!(
        (
            wider - start,
            collapsed * 3. < start,
            toggled,
            reopened,
            widened - wider
        ),
        (60., true, wider, wider, 30.),
        "the tab was {start}px, then {wider}px, {collapsed}px, {toggled}px toggled open, \
         {reopened}px dragged open, and {widened}px"
    );
}

/// Settings, at the sidebar's foot, opens General settings from any page
/// and unfolds its subpages below itself; leaving settings folds them away.
#[gpui_kit::test]
fn settings_unfold_their_subpages_in_the_sidebar(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    let shortcuts = ui.find("shortcuts-page").bounds();
    let settings = ui.find("settings-page").bounds();
    assert!(
        settings.top() > shortcuts.top() + px(200.),
        "Settings at {settings:?} is not at the sidebar's foot below {shortcuts:?}"
    );
    assert!(ui.try_find("settings-tab-general").is_none());
    ui.click("shortcuts-page");
    ui.click("settings-page");
    let general = ui.find("settings-tab-general");
    assert_eq!(
        (
            general.selected(),
            ui.find("shortcuts-page").selected(),
            ui.find("current-page").label().map(str::to_string),
        ),
        (Some(true), Some(false), general.label().map(str::to_string)),
        "Settings did not open General settings"
    );
    let general = general.bounds();
    assert_eq!(
        (general.top() > settings.top() - px(100.), general.left()),
        (true, shortcuts.left()),
        "the subpage at {general:?} is not under Settings in the sidebar"
    );
    ui.click("dictation-page");
    assert!(ui.try_find("settings-tab-general").is_none());
}

/// While the sidebar is collapsed, the settings subpages move to the top
/// of the settings page, and still navigate.
#[gpui_kit::test]
fn a_collapsed_sidebar_moves_the_settings_subpages_atop_the_page(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    ui.click("settings-page");
    let unfolded = ui.find("settings-tab-about").bounds();
    ui.click("toggle-sidebar");
    let title = ui.find("page-title").bounds();
    let atop = ui.find("settings-tab-about").bounds();
    assert_eq!(
        (
            atop.left() > unfolded.left(),
            atop.top() > title.top(),
            atop.top() < title.top() + px(100.)
        ),
        (true, true, true),
        "the tab at {atop:?} is not below the title at {title:?}"
    );
    ui.click("settings-tab-about");
    let about = ui.find("settings-tab-about");
    assert_eq!(
        (
            ui.find("current-page").label().map(str::to_string),
            ui.find("settings-page").selected(),
        ),
        (about.label().map(str::to_string), Some(true)),
    );
}

/// Tab walks the focus through the controls, and Enter presses the focused
/// one. A fresh window holds no focus and buttons take none when clicked,
/// so the walk starts from a text field.
#[gpui_kit::test]
fn the_keyboard_reaches_and_presses_controls(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    ui.click("shortcuts-page");
    ui.click("toggle-input");
    let expanded = width(&mut ui, "shortcuts-page");
    for _ in 0..30 {
        ui.press("tab");
        if ui.find("toggle-sidebar").focused() == Some(true) {
            ui.press("enter");
            let collapsed = width(&mut ui, "shortcuts-page");
            assert!(
                collapsed * 3. < expanded,
                "Enter left the tab {collapsed}px wide, from {expanded}px"
            );
            return;
        }
    }
    panic!("Tab never reached the sidebar toggle");
}
