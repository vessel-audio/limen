//! Tooltips on the sidebar's controls. The rest of navigation (switching,
//! collapsing, resizing, the settings subpages, and keyboard traversal) is
//! covered in process by `limen-gui`'s UI tests, on every platform; GPUI's
//! test observer does not see tooltip popups, so they are checked here.

use anyhow::{Context, Result, ensure};

use crate::daemon::Scenario;
use crate::session::Session;

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

/// Collapsed to its icons, the sidebar names its tabs, Settings included,
/// in tooltips.
pub fn collapsed_tabs_name_themselves_in_tooltips() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("toggle-sidebar")?;
    session.tooltip("shortcuts-page")?;
    session.tooltip("settings-page")?;
    Ok(())
}
