//! `limen shortcut` for the Carbon backend (macOS). The daemon registers the
//! configured keys with Carbon directly; there is no desktop dialog, so
//! finishing a setup or set is a short poll for the daemon's status.

#![cfg(target_os = "macos")]

use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use super::SetupError;
use limen_core::{
    Action, MIN_HOLD, Paths, SessionState, Shortcut, ShortcutConfig, ShortcutStatus, Trigger,
    shortcut_status as daemon_status,
};

/// Registration is instant when nothing conflicts; the poll exists only to
/// span the IPC round trip.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Carbon registers fine but macOS never dispatches hotkey events to a plain
/// CLI process, so bound keys never fire. Keep the path for a future fix
/// (CGEventTap or an .app bundle), but warn at every touch point.
const BROKEN_WARNING: &str = "warning: the carbon backend is broken on macOS: hotkeys register but never \
     fire from a CLI process; bind `limen toggle` in a launcher (Raycast, Hammerspoon, …) instead";

const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) async fn status(paths: &Paths, configured: ShortcutConfig) -> Result<(), SetupError> {
    eprintln!("{BROKEN_WARNING}");
    match daemon_status(paths).await? {
        Some(status) => print_status(&status),
        None => {
            println!("backend:  carbon");
            println!("session:  the daemon is not running, so no shortcut is active");
            println!("toggle:   asks for {}", configured.toggle);
            println!("hold:     asks for {}", configured.hold);
        }
    }
    Ok(())
}

/// Check the daemon bound the keys the operation layer just asked for.
pub(super) async fn finish_setup(paths: &Paths) -> Result<(), SetupError> {
    eprintln!("{BROKEN_WARNING}");
    match wait_for_status(paths).await? {
        Some(status) => {
            print_status(&status);
            check_bound(&status)
        }
        None => Ok(()),
    }
}

/// Check the daemon registered the key just requested.
pub(super) async fn finish_set(
    paths: &Paths,
    action: Action,
    keys: Shortcut,
) -> Result<(), SetupError> {
    let Some(status) = wait_for_status(paths).await? else {
        return Ok(());
    };
    check_bound(&status)?;
    let actual = match action {
        Action::Toggle => status.actual.toggle,
        Action::Hold => status.actual.hold,
    };
    match actual {
        Some(Trigger { shortcuts, .. }) if shortcuts.contains(&keys) => {
            println!("{}: {keys}", action.id());
            Ok(())
        }
        _ => Err(anyhow!(
            "the daemon did not bind {keys} for {}; retry with `limen shortcut set {} {keys}`",
            action.id(),
            action.id(),
        )
        .into()),
    }
}

/// Poll the daemon until registration settles.
async fn wait_for_status(paths: &Paths) -> Result<Option<ShortcutStatus>, SetupError> {
    let wait = async {
        loop {
            match daemon_status(paths).await? {
                Some(status) if status.session == SessionState::Binding => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                other => return Ok(other),
            }
        }
    };
    tokio::time::timeout(SETUP_TIMEOUT, wait)
        .await
        .with_context(|| {
            format!(
                "the daemon has not answered in {}s; check `limen shortcut status`",
                SETUP_TIMEOUT.as_secs()
            )
        })?
}

/// Fail unless both actions are bound, saying what to do.
fn check_bound(status: &ShortcutStatus) -> Result<(), SetupError> {
    match status.session {
        SessionState::Bound if status.actual.toggle.is_some() && status.actual.hold.is_some() => {
            Ok(())
        }
        SessionState::Failed => Err(anyhow!(
            "{}; another app may own the key — pick another with `limen shortcut set`",
            status
                .last_error
                .as_deref()
                .unwrap_or("registration failed")
        )
        .into()),
        SessionState::Bound
        | SessionState::Cancelled
        | SessionState::Disabled
        | SessionState::Binding => {
            Err(anyhow!("the daemon did not bind both keys; check `limen shortcut status`").into())
        }
    }
}

fn print_status(status: &ShortcutStatus) {
    let session = match status.session {
        SessionState::Disabled => "disabled",
        SessionState::Binding => "binding",
        SessionState::Bound => "bound",
        SessionState::Cancelled => "cancelled",
        SessionState::Failed => "failed",
    };
    println!("backend:  {}", status.backend);
    println!("session:  {session}");
    if let Some(error) = &status.last_error {
        println!("error:    {error}");
    }
    let key = |actual: &Option<Trigger>, preferred: Shortcut| match actual {
        Some(Trigger { shortcuts, .. }) if !shortcuts.is_empty() => shortcuts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" or "),
        Some(Trigger { description, .. }) => description.clone(),
        None => format!("not bound (asks for {preferred})"),
    };
    println!(
        "toggle:   {}",
        key(&status.actual.toggle, status.preferred.toggle)
    );
    println!(
        "hold:     {}   (holds shorter than {}ms are ignored as accidental)",
        key(&status.actual.hold, status.preferred.hold),
        MIN_HOLD.as_millis()
    );
    println!("change keys: limen shortcut set <toggle|hold> <KEYS>");
}
