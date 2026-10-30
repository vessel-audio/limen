//! The shortcut page: backend choice, the portal form, GNOME key recording,
//! and the operations they run against the config, GNOME, and the daemon.

use anyhow::{Context, Result, ensure};
use voice_input_core::{
    Backend, PerAction, SessionState, Shortcut, ShortcutConfig, ShortcutStatus, shortcut_config, v1,
};

use crate::daemon::Scenario;
use crate::desktop::Key;
use crate::session::{Session, Target};

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";

fn shortcut(text: &str) -> Result<Shortcut> {
    Ok(text.parse()?)
}

/// Replace a text field's contents by typing, as a user does.
fn retype(session: &Session, id: &str, text: &str) -> Result<()> {
    session.click(id)?;
    session.chord(&[Key::CONTROL, Key::char('a')])?;
    session.x11().type_text(text)
}

fn rebinds(session: &Session) -> Result<usize> {
    Ok(session
        .requests()?
        .iter()
        .filter(|request| **request == v1::Request::ShortcutRebind)
        .count())
}

/// The key GNOME binds to `voice-input toggle`, if a custom shortcut runs it.
fn gnome_binding(session: &Session) -> Result<Option<Shortcut>> {
    let list = session.gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?;
    for path in list.split('\'').skip(1).step_by(2) {
        let schema = format!("{CUSTOM}:{path}");
        let command = session.gsettings(&["get", &schema, "command"])?;
        if command.contains("voice-input") && command.ends_with("toggle'") {
            let binding = session.gsettings(&["get", &schema, "binding"])?;
            return match binding.trim_matches('\'') {
                "" => Ok(None),
                binding => Ok(Some(Shortcut::parse_accelerator(binding)?)),
            };
        }
    }
    Ok(None)
}

/// Each backend shows its own form.
pub fn the_backend_selector_swaps_the_form() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    for (backend, shows, hides) in [
        (
            "backend-portal",
            "edit-portal-shortcuts",
            "copy-toggle-command",
        ),
        ("backend-gnome", "record-toggle", "edit-portal-shortcuts"),
        ("backend-none", "copy-toggle-command", "record-toggle"),
    ] {
        session.click(backend)?;
        session.node(shows)?;
        session.absent(hides)?;
    }
    Ok(())
}

/// Without a backend, the page offers the command to bind by hand.
pub fn copying_the_toggle_command() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    session.click("copy-toggle-command")?;
    session.clipboard("voice-input toggle")
}

/// Applying the portal backend records the typed keys, installs the
/// desktop entry the portal needs, and asks the daemon to bind again.
pub fn applying_the_portal_backend_writes_the_config() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    session.click("backend-portal")?;
    retype(&session, "toggle-input", "Super+Y")?;
    retype(&session, "hold-input", "Super+U")?;
    session.click("apply-shortcut-backend")?;
    let wanted = ShortcutConfig {
        backend: Backend::Portal,
        toggle: shortcut("Super+Y")?,
        hold: shortcut("Super+U")?,
    };
    session.until("the portal config", || {
        Ok(shortcut_config(session.paths()).ok() == Some(wanted))
    })?;
    session.until("a rebind request", || Ok(rebinds(&session)? == 1))?;
    let report = session.node("shortcut-report")?;
    ensure!(
        report.name.contains("Installed"),
        "the report says {:?}",
        report.name
    );
    let entry = session
        .paths()
        .config_file
        .ancestors()
        .nth(3)
        .context("no XDG root")?
        .join("data/applications/io.github.Emin017.VoiceInput.desktop");
    ensure!(entry.exists(), "{} was not installed", entry.display());
    Ok(())
}

/// A shortcut that cannot be parsed is reported instead of applied.
pub fn an_unparsable_portal_shortcut_is_reported() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    session.click("backend-portal")?;
    retype(&session, "toggle-input", "Super+")?;
    session.click("apply-shortcut-backend")?;
    let report = session.node("shortcut-report")?;
    ensure!(!report.name.is_empty(), "the report is empty");
    ensure!(rebinds(&session)? == 0, "an invalid shortcut was applied");
    ensure!(
        !session.paths().config_file.exists(),
        "an invalid shortcut was written"
    );
    Ok(())
}

/// Once the portal is set up, editing its shortcuts opens this app's page
/// in GNOME Settings.
pub fn editing_portal_shortcuts_opens_gnome_settings() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    session.click("shortcuts-page")?;
    session.click("backend-portal")?;
    session.click("apply-shortcut-backend")?;
    session.until("a rebind request", || Ok(rebinds(&session)? == 1))?;
    session.click("edit-portal-shortcuts")?;
    session.until("GNOME Settings to open", || {
        Ok(session.launched()?.contains(&vec![
            "gnome-control-center".to_string(),
            "applications".to_string(),
            "io.github.Emin017.VoiceInput".to_string(),
        ]))
    })
}

/// A failed portal session offers a retry that sets the portal up again.
pub fn retrying_a_failed_portal_session() -> Result<()> {
    let session = Session::fake(Scenario {
        shortcut: ShortcutStatus {
            backend: Backend::Portal,
            session: SessionState::Failed,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction::default(),
            last_error: Some("the portal refused the session".to_string()),
        },
        ..Scenario::default()
    })?;
    session.click("shortcuts-page")?;
    session.click("retry-portal")?;
    session.until("a rebind request", || Ok(rebinds(&session)? == 1))?;
    ensure!(
        shortcut_config(session.paths())
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .backend
            == Backend::Portal,
        "the retry did not select the portal"
    );
    Ok(())
}

/// Select and apply the GNOME backend, which binds the default toggle.
fn apply_gnome(session: &Session) -> Result<()> {
    session.click("shortcuts-page")?;
    session.click("backend-gnome")?;
    session.click("apply-shortcut-backend")?;
    session.until("GNOME to bind Super+Z", || {
        Ok(gnome_binding(session)? == Some(Shortcut::DEFAULT_TOGGLE))
    })
}

/// Recording takes the next chord as the GNOME toggle; Escape cancels a
/// recording and Backspace clears the binding.
pub fn recording_a_gnome_shortcut_with_the_keyboard() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    apply_gnome(&session)?;
    session.click("record-toggle")?;
    session.named(
        "recorder-state",
        "Press a shortcut… (Esc cancels, Backspace clears)",
    )?;
    session.chord(&[Key::CONTROL, Key::ALT, Key::char('v')])?;
    let recorded = shortcut("Ctrl+Alt+V")?;
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

/// A shifted symbol cannot be recorded and asks for manual entry.
pub fn shifted_symbols_need_manual_entry() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    apply_gnome(&session)?;
    session.click("record-toggle")?;
    session.chord(&[Key::SHIFT, Key::char('1')])?;
    session.named("recorder-error", "Shifted symbols need manual entry")?;
    ensure!(
        gnome_binding(&session)? == Some(Shortcut::DEFAULT_TOGGLE),
        "an invalid chord changed the binding"
    );
    Ok(())
}

/// A typed shortcut is set by its button, and Clear removes the binding.
pub fn setting_and_clearing_a_gnome_shortcut_by_hand() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    apply_gnome(&session)?;
    retype(&session, "toggle-input", "Super+J")?;
    session.click("set-toggle-manual")?;
    let typed = shortcut("Super+J")?;
    session.until("GNOME to bind Super+J", || {
        Ok(gnome_binding(&session)? == Some(typed))
    })?;
    session.click(Target::Id("clear-toggle"))?;
    session.until("GNOME to drop the binding", || {
        Ok(gnome_binding(&session)?.is_none())
    })
}
