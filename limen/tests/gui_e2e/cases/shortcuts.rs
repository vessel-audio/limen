//! The shortcut case that needs a real desktop: recording a GNOME shortcut
//! with the keyboard, then reading GNOME's real settings to see the binding
//! the daemon's operations left there. The page's forms, the keys applying a
//! backend sends, the recorder, and the reports are tested in-process in
//! limen-gui's UI tests.

use anyhow::{Result, ensure};
use limen_core::Shortcut;

use crate::daemon::Scenario;
use crate::desktop::Key;
use crate::session::Session;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";

/// The key GNOME binds to `limen toggle`, if a custom shortcut runs it.
fn gnome_binding(session: &Session) -> Result<Option<Shortcut>> {
    let list = session.gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?;
    for path in list.split('\'').skip(1).step_by(2) {
        let schema = format!("{CUSTOM}:{path}");
        let command = session.gsettings(&["get", &schema, "command"])?;
        if command.contains("limen") && command.ends_with("toggle'") {
            let binding = session.gsettings(&["get", &schema, "binding"])?;
            return match binding.trim_matches('\'') {
                "" => Ok(None),
                binding => Ok(Some(Shortcut::parse_accelerator(binding)?)),
            };
        }
    }
    Ok(None)
}

/// Recording takes the next chord as the GNOME toggle; Escape cancels a
/// recording and Backspace clears the binding.
pub fn recording_a_gnome_shortcut_with_the_keyboard() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    session.click("backend-gnome")?;
    session.click("apply-shortcut-backend")?;
    session.until("GNOME to bind Super+Z", || {
        Ok(gnome_binding(&session)? == Some(Shortcut::DEFAULT_TOGGLE))
    })?;
    session.click("record-toggle")?;
    session.named(
        "recorder-state",
        "Press a shortcut… (Esc cancels, Backspace clears)",
    )?;
    session.chord(&[Key::CONTROL, Key::ALT, Key::char('v')])?;
    let recorded = "Ctrl+Alt+V".parse::<Shortcut>()?;
    session.until("GNOME to bind Ctrl+Alt+V", || {
        Ok(gnome_binding(&session)? == Some(recorded))
    })?;
    session.named("recorder-state", &recorded.to_string())?;
    session.click("record-toggle")?;
    session.chord(&[Key::ESCAPE])?;
    session.named("recorder-state", &recorded.to_string())?;
    ensure!(
        gnome_binding(&session)? == Some(recorded),
        "Escape changed the binding"
    );
    session.click("record-toggle")?;
    session.chord(&[Key::BACKSPACE])?;
    session.until("GNOME to drop the binding", || {
        Ok(gnome_binding(&session)?.is_none())
    })
}
