//! General and About settings: appearance, language, typography, and
//! lightweight mode.

use anyhow::{Context, Result, ensure};
use atspi::{Role, State};

use crate::daemon::Scenario;
use crate::desktop::{Key, SystemScheme, viewport};
use crate::session::{Launch, Session};

/// The tabs switch between the General controls and the About card.
pub fn settings_tabs_switch_between_general_and_about() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.click("settings-tab-about")?;
    session.state("settings-tab-about", State::Selected, true)?;
    session.named("current-page", "About")?;
    session.named(
        "about-version",
        &format!("Version {}", env!("CARGO_PKG_VERSION")),
    )?;
    session.absent("appearance-dark")?;
    session.click("settings-tab-general")?;
    session.named("current-page", "General")?;
    session.node("appearance-dark")?;
    session.absent("about-version")?;
    Ok(())
}

/// Whether the content panel is painted dark, sampled beside the title.
fn dark(session: &Session) -> Result<bool> {
    let title = session.settled("page-title")?.bounds.context("no title")?;
    let [red, green, blue] = session.x11().pixel(title.x - 12, title.center().1)?;
    Ok(u32::from(red) + u32::from(green) + u32::from(blue) < 300)
}

/// Light and Dark override the desktop's preference; Follow system, the
/// default, takes the desktop's dark preference from its settings portal.
pub fn appearance_follows_the_choice_and_the_system() -> Result<()> {
    let session = Session::launch(Launch {
        scheme: SystemScheme::PreferDark,
        ..Launch::default()
    })?;
    session.click("settings-page")?;
    session.until("the system's dark scheme", || dark(&session))?;
    for (choice, wanted) in [
        ("appearance-light", false),
        ("appearance-dark", true),
        ("appearance-light", false),
        ("appearance-system", true),
    ] {
        session.click(choice)?;
        session.until(&format!("{choice} to paint"), || {
            Ok(dark(&session)? == wanted)
        })?;
    }
    Ok(())
}

/// English and Chinese override the system language; Follow system goes
/// back to the locale the session started with.
pub fn language_choice_renames_the_interface() -> Result<()> {
    let session = Session::launch(Launch {
        lang: "zh_CN.UTF-8",
        ..Launch::default()
    })?;
    session.named("settings-page", "设置")?;
    session.click("settings-page")?;
    for (choice, settings, dictation) in [
        ("language-english", "Settings", "Dictation"),
        ("language-chinese", "设置", "听写"),
        ("language-english", "Settings", "Dictation"),
        ("language-system", "设置", "听写"),
    ] {
        session.click(choice)?;
        session.named("settings-page", settings)?;
        session.named("dictation-page", dictation)?;
    }
    Ok(())
}

/// The text size is one choice among scales: choosing one checks it alone,
/// 125% makes text taller, and 100% restores it.
pub fn the_text_scale_resizes_the_interface() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.state("font-scale-100", State::Checked, true)?;
    let height = || -> Result<i32> {
        Ok(session
            .settled("page-title")?
            .bounds
            .context("no title")?
            .height)
    };
    let normal = height()?;
    session.reveal("font-scale-125")?;
    session.click("font-scale-125")?;
    session.state("font-scale-125", State::Checked, true)?;
    session.state("font-scale-100", State::Checked, false)?;
    ensure!(height()? > normal, "text did not grow");
    // Larger text pushes the row below the window's edge.
    session.reveal("font-scale-100")?;
    session.click("font-scale-100")?;
    session.state("font-scale-100", State::Checked, true)?;
    ensure!(height()? == normal, "text did not return to its size");
    Ok(())
}

/// The font select searches installed families and applies the chosen one,
/// which changes how wide text sets; following the system restores it.
pub fn the_font_select_applies_a_family() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    let width = || -> Result<i32> {
        Ok(session
            .settled("current-page")?
            .bounds
            .context("no page name")?
            .width)
    };
    let system = width()?;
    for (search, changed) in [("DejaVu Serif", true), ("Follow system", false)] {
        session.click(Role::ComboBox)?;
        session.node(Role::List)?;
        session.x11().type_text(search)?;
        session.wait(&format!("one match for {search}"), |nodes| {
            (nodes
                .iter()
                .filter(|node| node.role == Role::ListItem)
                .count()
                == 1)
                .then_some(())
        })?;
        session.chord(&[Key::RETURN])?;
        session.wait("the list to close", |nodes| {
            (!nodes.iter().any(|node| node.role == Role::List)).then_some(())
        })?;
        session.until(&format!("{search} to apply"), || {
            Ok((width()? != system) == changed)
        })?;
    }
    Ok(())
}

/// Lightweight mode turns on and off, and while on, motion is skipped: the
/// sidebar collapses at once instead of springing.
pub fn lightweight_mode_skips_motion() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.reveal("lightweight-on")?;
    session.click("lightweight-on")?;
    session.state("lightweight-on", State::Checked, true)?;
    let expanded = session.settled("history-page")?.bounds.context("no tab")?;
    session.click("toggle-sidebar")?;
    // The first width drawn once the click is handled is already the last.
    let first = session.wait("the sidebar to change", |nodes| {
        nodes
            .iter()
            .find(|node| node.id == "history-page")?
            .bounds
            .map(|bounds| bounds.width)
            .filter(|width| *width != expanded.width)
    })?;
    ensure!(
        first * 3 < expanded.width,
        "the sidebar animated: {} then {first}",
        expanded.width
    );
    session.reveal("lightweight-off")?;
    session.click("lightweight-off")?;
    session.state("lightweight-off", State::Checked, true)?;
    Ok(())
}

/// Dragging the always-visible scrollbar scrolls General settings to the
/// service controls at the bottom. Larger text pushes them below the view.
pub fn the_scrollbar_drags_the_settings_page() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.click("font-scale-150")?;
    session.state("font-scale-150", State::Checked, true)?;
    let view = session.wait("the settings scroll view", |nodes| {
        let index = nodes.iter().position(|node| node.id == "appearance-dark")?;
        viewport(nodes, index)
    })?;
    let before = session.settled("page-title")?.bounds.context("no title")?;
    let x = view.x + view.width - 4;
    session
        .x11()
        .drag((x, view.y + 12), (x, view.bottom() - 4))?;
    let after = session.settled("page-title")?.bounds.context("no title")?;
    ensure!(
        after.y + 50 < before.y,
        "the page did not scroll: {before:?} then {after:?}"
    );
    let stop = session
        .settled("stop-daemon")?
        .bounds
        .context("no button")?;
    ensure!(
        stop.bottom() <= view.bottom(),
        "the service controls stay below the view"
    );
    Ok(())
}
