//! Starting, stopping, restarting, and replacing the daemon, with the real
//! daemon wherever the control panel starts one.

use std::os::unix::process::ExitStatusExt;

use anyhow::{Result, ensure};

use crate::daemon::Scenario;
use crate::session::{Daemon, Launch, Session};

const READY: &str = "Service ready";

/// Stopping asks first; cancelling keeps the daemon, confirming stops it,
/// and the start button brings a new one up.
pub fn stopping_asks_first_and_start_brings_it_back() -> Result<()> {
    let session = Session::real()?;
    session.named("connection-status", READY)?;
    session.click("open-settings")?;
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
    session.click("open-settings")?;
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

/// An outdated daemon can be terminated after confirmation; cancelling
/// leaves it running.
pub fn an_outdated_daemon_can_be_terminated() -> Result<()> {
    let mut session = Session::fake(Scenario {
        outdated: true,
        ..Scenario::default()
    })?;
    session.node("outdated-banner")?;
    session.click("terminate-daemon")?;
    session.named(
        "shutdown-confirmation",
        "Terminate the outdated background service?",
    )?;
    session.click("cancel-shutdown")?;
    session.absent("shutdown-confirmation")?;
    ensure!(
        session.fake_daemon()?.exit_status()?.is_none(),
        "cancelling terminated the daemon"
    );
    session.click("terminate-daemon")?;
    session.click("confirm-shutdown")?;
    session.named("connection-overlay", "Background service is not running")?;
    let status = session.fake_daemon()?.exit_status()?;
    ensure!(
        status.and_then(|status| status.signal()) == Some(libc::SIGTERM),
        "the daemon ended with {status:?}"
    );
    Ok(())
}

/// A daemon of another version is named in a banner.
pub fn a_mismatched_daemon_version_is_named() -> Result<()> {
    let session = Session::fake(Scenario {
        version: "0.0.1".to_string(),
        ..Scenario::default()
    })?;
    let banner = session.node("version-banner")?;
    ensure!(
        banner.name.contains("0.0.1") && banner.name.contains(env!("CARGO_PKG_VERSION")),
        "the banner says {:?}",
        banner.name
    );
    Ok(())
}
