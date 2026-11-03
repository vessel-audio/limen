//! Sidebar navigation, the sidebar toggle, and the settings button.

use anyhow::{Context, Result, ensure};
use atspi::{Role, State};

use crate::daemon::Scenario;
use crate::desktop::Key;
use crate::session::Session;

/// Each sidebar tab selects itself and names the page in the title bar.
pub fn sidebar_switches_pages() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.state("dictation-page", State::Selected, true)?;
    for (tab, page) in [
        ("history-page", "History"),
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
    let expanded = width("history-page")?;
    session.click("toggle-sidebar")?;
    let collapsed = width("history-page")?;
    ensure!(
        collapsed * 3 < expanded,
        "collapsed tab is {collapsed}px wide, expanded {expanded}px"
    );
    session.tooltip("history-page")?;
    session.click("history-page")?;
    session.named("current-page", "History")?;
    session.click("toggle-sidebar")?;
    ensure!(
        width("history-page")? == expanded,
        "the sidebar did not expand"
    );
    Ok(())
}

/// Dragging the sidebar's edge resizes it; dragged far enough in, it
/// collapses to its icons, and dragged back out it expands again.
pub fn dragging_the_sidebar_edge_resizes_and_collapses_it() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    // A tab spans the sidebar but its 12px padding on each side; the edge
    // lies over the right padding, its middle 8px past the tab.
    let tab = || -> Result<(i32, i32, i32)> {
        let bounds = session
            .settled("history-page")?
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
    let (edge, _, collapsed) = tab()?;
    session.x11().drag((edge, y), (edge + 160, y))?;
    let (_, _, reopened) = tab()?;
    ensure!(
        (wider - start, collapsed * 3 < start, reopened - collapsed) == (60, true, 160),
        "the tab was {start}px, then {wider}px, {collapsed}px, and {reopened}px wide"
    );
    Ok(())
}

/// The title bar's icon buttons explain themselves on hover.
pub fn title_bar_buttons_show_tooltips() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    for id in ["toggle-sidebar", "open-settings"] {
        let (button, tooltip) = session.tooltip(id)?;
        let (button, tooltip) = (
            button.bounds.context("button is off screen")?,
            tooltip.bounds.context("tooltip is off screen")?,
        );
        ensure!(
            (tooltip.x - button.x).abs() < 200 && tooltip.y > button.y,
            "the tooltip at {tooltip:?} is not beside the button at {button:?}"
        );
        session.x11().move_to(400, 400)?;
        session.wait("the tooltip to close", |nodes| {
            (!nodes.iter().any(|node| node.role == Role::ToolTip)).then_some(())
        })?;
    }
    Ok(())
}

/// The settings button opens General settings from any page, and the
/// sidebar then selects nothing until it navigates again.
pub fn settings_button_opens_general_settings() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("history-page")?;
    session.click("open-settings")?;
    session.named("current-page", "General")?;
    session.state("settings-tab-general", State::Selected, true)?;
    session.state("history-page", State::Selected, false)?;
    session.click("dictation-page")?;
    session.named("current-page", "Dictation")?;
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
        if focused.contains(":open-settings:") {
            session.chord(&[Key::RETURN])?;
            session.named("current-page", "General")?;
            return Ok(());
        }
    }
    anyhow::bail!("Tab never reached the settings button: {seen:?}")
}
