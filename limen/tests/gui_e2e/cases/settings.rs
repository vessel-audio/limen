//! General settings that need the real desktop: that its dark preference
//! reaches the window and is painted, that its locale picks the interface
//! language at launch, that a chosen font changes how text sets, and that
//! Lightweight mode skips the sidebar's motion. The rest of the settings,
//! including the choices themselves, are tested in-process in limen-gui's UI
//! tests.

use anyhow::{Context, Result, ensure};
use atspi::{Role, State};

use crate::daemon::Scenario;
use crate::desktop::{Key, SystemScheme};
use crate::session::{Launch, Session};

/// Whether the content panel is painted dark, sampled beside the title.
fn dark(session: &Session) -> Result<bool> {
    let title = session.settled("page-title")?.bounds.context("no title")?;
    let [red, green, blue] = session.x11().pixel(title.x - 12, title.center().1)?;
    Ok(u32::from(red) + u32::from(green) + u32::from(blue) < 300)
}

/// The desktop's dark preference, which comes through its settings portal,
/// is painted by default; Light overrides it, and Follow system goes back.
pub fn appearance_follows_the_desktops_dark_preference() -> Result<()> {
    let session = Session::launch(Launch {
        scheme: SystemScheme::PreferDark,
        ..Launch::default()
    })?;
    session.click("settings-page")?;
    session.until("the system's dark scheme", || dark(&session))?;
    for (choice, wanted) in [("appearance-light", false), ("appearance-system", true)] {
        session.click(choice)?;
        session.until(&format!("{choice} to paint"), || {
            Ok(dark(&session)? == wanted)
        })?;
    }
    Ok(())
}

/// The interface opens in the language of the locale the desktop launches
/// it with.
pub fn the_interface_starts_in_the_system_language() -> Result<()> {
    let session = Session::launch(Launch {
        lang: "zh_CN.UTF-8",
        ..Launch::default()
    })?;
    session.named("settings-page", "设置")?;
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

/// While Lightweight mode is on, motion is skipped: the sidebar collapses at
/// once instead of springing.
pub fn lightweight_mode_skips_motion() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("settings-page")?;
    session.reveal("lightweight-on")?;
    session.click("lightweight-on")?;
    session.state("lightweight-on", State::Checked, true)?;
    let expanded = session
        .settled("shortcuts-page")?
        .bounds
        .context("no tab")?;
    session.click("toggle-sidebar")?;
    // The first width drawn once the click is handled is already the last.
    let first = session.wait("the sidebar to change", |nodes| {
        nodes
            .iter()
            .find(|node| node.id == "shortcuts-page")?
            .bounds
            .map(|bounds| bounds.width)
            .filter(|width| *width != expanded.width)
    })?;
    ensure!(
        first * 3 < expanded.width,
        "the sidebar animated: {} then {first}",
        expanded.width
    );
    Ok(())
}
