//! The lifecycle cases that need a real daemon process or desktop: stopping
//! the daemon and starting it again, restarting it into a new process, and
//! a start that fails on a broken
//! config, whose log then opens in the desktop's handler, which the test
//! platform of limen-gui's UI tests cannot do. The rest is tested in-process
//! in those tests, which replay what the bridge reports: the confirmations,
//! the commands the window sends, the overlays and banners, and the forms
//! reset when a daemon is replaced.

use anyhow::{Result, ensure};

use crate::session::{Daemon, Launch, Session};

const READY: &str = "Service ready";

/// Stopping asks first; cancelling keeps the daemon, confirming stops it,
/// and the start button brings a new one up.
pub fn stopping_asks_first_and_start_brings_it_back() -> Result<()> {
    let session = Session::real()?;
    session.named("connection-status", READY)?;
    session.click("settings-page")?;
    session.click("stop-daemon")?;
    session.node("shutdown-confirmation")?;
    session.click("cancel-shutdown")?;
    session.absent("shutdown-confirmation")?;
    session.named("connection-status", READY)?;
    session.click("stop-daemon")?;
    session.click("confirm-shutdown")?;
    session.named("connection-overlay", "Background service is not running")?;
    ensure!(
        !session.paths().socket.exists(),
        "the daemon kept its socket"
    );
    session.click("start-daemon")?;
    session.absent("connection-overlay")?;
    session.named("connection-status", READY)?;
    Ok(())
}

/// Restarting replaces the daemon process and reconnects to the new one.
pub fn restarting_replaces_the_daemon() -> Result<()> {
    let session = Session::real()?;
    session.named("connection-status", READY)?;
    let before = session.daemon_pid()?;
    session.click("settings-page")?;
    session.click("restart-daemon")?;
    session.until("a new daemon", || {
        Ok(session.daemon_pid().is_ok_and(|pid| pid != before))
    })?;
    session.named("connection-status", READY)?;
    Ok(())
}

/// A daemon that cannot start shows its log tail and offers the full log,
/// which opens in the desktop's handler; starting again fails the same way.
pub fn a_failed_start_offers_the_log() -> Result<()> {
    let session = Session::launch(Launch {
        daemon: Daemon::Real,
        config: Some("this is not toml ["),
        ..Launch::default()
    })?;
    let overlay = session.named("connection-overlay", "Background service failed to start")?;
    ensure!(
        !overlay.description.is_empty(),
        "the overlay shows no log tail"
    );
    session.click("open-startup-log")?;
    let log = session.paths().log_file.display().to_string();
    session.until("the log to open", || {
        Ok(session
            .launched()?
            .contains(&vec!["xdg-open".to_string(), log.clone()]))
    })?;
    session.click("start-daemon")?;
    session.named("connection-overlay", "Background service failed to start")?;
    Ok(())
}
