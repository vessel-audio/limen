//! Sidebar navigation, the sidebar toggle, and the settings subpages.

use anyhow::{Context, Result, ensure};
use atspi::State;

use crate::daemon::Scenario;
use crate::desktop::Key;
use crate::session::Session;

/// Each sidebar tab selects itself and names the page in the title bar.
pub fn sidebar_switches_pages() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.state("dictation-page", State::Selected, true)?;
    for (tab, page) in [
        ("shortcuts-page", "Shortcuts"),
        ("dictation-page", "Dictation"),
    ] {
        session.click(tab)?;
        session.state(tab, State::Selected, true)?;
        session.named("current-page", page)?;
    }
    Ok(())
}

/// Collapsing narrows the sidebar to its icons, which then name themselves
/// in tooltips and still navigate; expanding restores the labels.
pub fn sidebar_collapses_to_icons_and_back() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let width = |id| -> Result<i32> {
        Ok(session
            .settled(id)?
            .bounds
            .context("tab is off screen")?
            .width)
    };
    let expanded = width("shortcuts-page")?;
    session.click("toggle-sidebar")?;
    let collapsed = width("shortcuts-page")?;
    ensure!(
        collapsed * 3 < expanded,
        "collapsed tab is {collapsed}px wide, expanded {expanded}px"
    );
    session.tooltip("shortcuts-page")?;
    session.click("shortcuts-page")?;
    session.named("current-page", "Shortcuts")?;
    session.click("toggle-sidebar")?;
    ensure!(
        width("shortcuts-page")? == expanded,
        "the sidebar did not expand"
    );
    Ok(())
}

/// Dragging the sidebar's edge resizes it; dragged far enough in, it
/// collapses to its icons. The width it was dragged to is kept: the toggle
/// and a drag back out both reopen it that wide, and a drag from there
/// resizes it again.
pub fn dragging_the_sidebar_edge_resizes_and_collapses_it() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    // A tab spans the sidebar but its 12px padding on each side; the edge
    // lies over the right padding, its middle 8px past the tab.
    let tab = || -> Result<(i32, i32, i32)> {
        let bounds = session
            .settled("shortcuts-page")?
            .bounds
            .context("tab is off screen")?;
        Ok((
            bounds.x + bounds.width + 8,
            bounds.y + bounds.height / 2,
            bounds.width,
        ))
    };
    let (edge, y, start) = tab()?;
    session.x11().drag((edge, y), (edge + 60, y))?;
    let (edge, _, wider) = tab()?;
    session.x11().drag((edge, y), (40, y))?;
    let (_, _, collapsed) = tab()?;
    session.click("toggle-sidebar")?;
    let (_, _, toggled) = tab()?;
    session.click("toggle-sidebar")?;
    let (edge, _, _) = tab()?;
    session.x11().drag((edge, y), (edge + 120, y))?;
    let (edge, _, reopened) = tab()?;
    session.x11().drag((edge, y), (edge + 30, y))?;
    let (_, _, widened) = tab()?;
    ensure!(
        (
            wider - start,
            collapsed * 3 < start,
            toggled,
            reopened,
            widened - wider
        ) == (60, true, wider, wider, 30),
        "the tab was {start}px, then {wider}px, {collapsed}px, {toggled}px toggled open, \
         {reopened}px dragged open, and {widened}px"
    );
    Ok(())
}

/// The sidebar toggle explains itself on hover.
pub fn the_sidebar_toggle_shows_a_tooltip() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let (button, tooltip) = session.tooltip("toggle-sidebar")?;
    let (button, tooltip) = (
        button.bounds.context("button is off screen")?,
        tooltip.bounds.context("tooltip is off screen")?,
    );
    ensure!(
        (tooltip.x - button.x).abs() < 200 && tooltip.y > button.y,
        "the tooltip at {tooltip:?} is not beside the button at {button:?}"
    );
    Ok(())
}

/// Settings, at the sidebar's foot, opens General settings from any page
/// and unfolds its subpages below itself; leaving settings folds them away.
pub fn settings_unfold_their_subpages_in_the_sidebar() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let shortcuts = session
        .settled("shortcuts-page")?
        .bounds
        .context("no tab")?;
    let settings = session.settled("settings-page")?.bounds.context("no tab")?;
    ensure!(
        settings.y > shortcuts.y + 200,
        "Settings at {settings:?} is not at the sidebar's foot below {shortcuts:?}"
    );
    session.absent("settings-tab-general")?;
    session.click("shortcuts-page")?;
    session.click("settings-page")?;
    session.named("current-page", "General")?;
    session.state("settings-tab-general", State::Selected, true)?;
    session.state("shortcuts-page", State::Selected, false)?;
    let general = session
        .settled("settings-tab-general")?
        .bounds
        .context("no subpage")?;
    ensure!(
        (general.y > settings.y - 100, general.x == shortcuts.x) == (true, true),
        "the subpage at {general:?} is not under Settings in the sidebar"
    );
    session.click("dictation-page")?;
    session.named("current-page", "Dictation")?;
    session.absent("settings-tab-general")?;
    Ok(())
}

/// While the sidebar is collapsed, the settings subpages move to the top
/// of the settings page, and Settings names itself in a tooltip.
pub fn a_collapsed_sidebar_moves_the_settings_subpages_atop_the_page() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    let unfolded = session
        .settled("settings-tab-about")?
        .bounds
        .context("no subpage")?;
    session.click("toggle-sidebar")?;
    session.tooltip("settings-page")?;
    let title = session.settled("page-title")?.bounds.context("no title")?;
    let atop = session
        .settled("settings-tab-about")?
        .bounds
        .context("no subpage")?;
    ensure!(
        (
            atop.x > unfolded.x,
            atop.y > title.y,
            atop.y < title.y + 100
        ) == (true, true, true),
        "the tab at {atop:?} is not below the title at {title:?}"
    );
    session.click("settings-tab-about")?;
    session.named("current-page", "About")?;
    session.state("settings-page", State::Selected, true)?;
    Ok(())
}

/// Tab walks the focus through the controls, and Enter presses the focused
/// one. A fresh window holds no focus and buttons take none when clicked,
/// so the walk starts from a text field.
pub fn the_keyboard_reaches_and_presses_controls() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    let focus = |previous: Option<&str>| {
        session.wait("the focus to move", |nodes| {
            nodes
                .iter()
                .find(|node| node.has(State::Focused))
                .map(|node| format!("{:?}:{}:{}", node.role, node.id, node.name))
                .filter(|focused| Some(focused.as_str()) != previous)
        })
    };
    let width = || -> Result<i32> {
        Ok(session
            .settled("shortcuts-page")?
            .bounds
            .context("tab is off screen")?
            .width)
    };
    session.click("shortcuts-page")?;
    session.click("backend-portal")?;
    session.click("toggle-input")?;
    let mut seen = vec![focus(None)?];
    for _ in 0..30 {
        session.chord(&[Key::TAB])?;
        // The window may not have handled the key yet; reading the focus
        // before it moves would put Enter on a control one Tab ahead.
        let focused = focus(seen.last().map(String::as_str))?;
        seen.push(focused.clone());
        if focused.contains(":toggle-sidebar:") {
            let expanded = width()?;
            session.chord(&[Key::RETURN])?;
            return session.until("Enter to collapse the sidebar", || {
                Ok(width()? * 3 < expanded)
            });
        }
    }
    anyhow::bail!("Tab never reached the sidebar toggle: {seen:?}")
}
