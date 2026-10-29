//! `voice-input shortcut` for the portal backend. The daemon holds the
//! portal session; these commands configure it and read its status. Once
//! GNOME has saved a binding, it decides the keys: `edit` opens its settings.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::process::Command;

use super::{Failure, check_config, check_distinct, gnome, rebind};
use crate::config::{self, Paths, ShortcutConfig};
use crate::dictation::MIN_HOLD;
use crate::ipc::{self, v1};
use crate::shortcut::desktop_entry::{self, APP_ID, Install};
use crate::shortcut::program::Program;
use crate::shortcut::status::{SessionState, ShortcutStatus, Trigger};
use crate::shortcut::{Action, Backend, Shortcut};

/// How long setup waits for GNOME, which may be waiting for the user.
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);

const POLL_INTERVAL: Duration = Duration::from_millis(200);

pub(super) async fn status(paths: &Paths, configured: ShortcutConfig) -> Result<(), Failure> {
    match daemon_status(paths).await? {
        Some(status) => print_status(&status),
        None => {
            println!("backend:  portal ({APP_ID})");
            println!("session:  the daemon is not running, so no shortcut is active");
            println!("toggle:   asks for {}", configured.toggle);
            println!("hold:     asks for {}", configured.hold);
        }
    }
    Ok(())
}

/// `setup portal`: record the backend and the keys GNOME should suggest,
/// make the app known to the portal, and wait for GNOME to bind.
pub(super) async fn setup(
    paths: &Paths,
    current: ShortcutConfig,
    toggle: Option<Shortcut>,
    hold: Option<Shortcut>,
) -> Result<(), Failure> {
    // Keys left out keep what the config says when it is written.
    let keys = move |config: ShortcutConfig| ShortcutConfig {
        backend: Backend::Portal,
        toggle: toggle.unwrap_or(config.toggle),
        hold: hold.unwrap_or(config.hold),
    };
    let wanted = keys(current);
    check_distinct(wanted.toggle, wanted.hold)?;
    check_config(paths, &wanted)?;
    let program = Program::find()?;
    match desktop_entry::ensure_installed(program.path())? {
        Install::Installed(path) => {
            println!("Installed {}.", path.display());
            if let Program::Unstable(program) = &program {
                eprintln!(
                    "warning: {} is in the Nix store and stops working after an upgrade or garbage \
                     collection; install voice-input into your profile and run setup again",
                    program.display()
                );
            }
        }
        Install::Present(_) => {}
    }
    // A GNOME custom shortcut on the same key would fire next to the portal's.
    gnome::clear_ours().await?;
    config::update_shortcut(&paths.config_file, keys)?;
    match apply(paths).await? {
        Some(status) => {
            print_status(&status);
            check_bound(&status)
        }
        None => Ok(()),
    }
}

/// `set <action> <KEYS>`: suggest new keys and bind again. GNOME offers
/// a suggestion only for a shortcut it has not saved yet; otherwise it keeps
/// what it saved, which `edit` changes.
pub(super) async fn set(
    paths: &Paths,
    current: ShortcutConfig,
    action: Action,
    keys: Shortcut,
) -> Result<(), Failure> {
    let wanted = match action {
        Action::Toggle => ShortcutConfig {
            toggle: keys,
            ..current
        },
        Action::Hold => ShortcutConfig {
            hold: keys,
            ..current
        },
    };
    check_distinct(wanted.toggle, wanted.hold)?;
    check_config(paths, &wanted)?;
    config::update_shortcut(&paths.config_file, |latest| match action {
        Action::Toggle => ShortcutConfig {
            toggle: keys,
            ..latest
        },
        Action::Hold => ShortcutConfig {
            hold: keys,
            ..latest
        },
    })?;
    let Some(status) = apply(paths).await? else {
        return Ok(());
    };
    check_bound(&status)?;
    let actual = match action {
        Action::Toggle => status.actual.toggle,
        Action::Hold => status.actual.hold,
    };
    confirm_binding(action, keys, actual)?;
    println!("{}: {keys}", action.id());
    Ok(())
}

/// Check that the binding GNOME actually kept is the one just requested.
fn confirm_binding(action: Action, keys: Shortcut, actual: Option<Trigger>) -> Result<(), Failure> {
    match actual {
        Some(Trigger { shortcuts, .. }) if shortcuts.contains(&keys) => Ok(()),
        Some(Trigger {
            description,
            shortcuts,
        }) => Err(match shortcuts.as_slice() {
            [] => anyhow!(
                "GNOME bound {} as \"{description}\", which could not be confirmed as {keys}; \
                 check it with `voice-input shortcut edit`",
                action.id()
            ),
            kept => anyhow!(
                "GNOME kept the saved binding {}; change it with `voice-input shortcut edit`",
                either(kept)
            ),
        }
        .into()),
        None => Err(anyhow!(
            "GNOME did not bind {}; check it with `voice-input shortcut edit`",
            action.id()
        )
        .into()),
    }
}

/// `edit`: open the app's page in GNOME Settings, where its keys change.
pub(super) async fn edit() -> Result<(), Failure> {
    let mut child = Command::new("gnome-control-center")
        .args(["applications", APP_ID])
        .kill_on_drop(true)
        .spawn()
        .context("cannot open GNOME Settings; change the keys under Apps → voice-input")?;
    let status = child
        .wait()
        .await
        .context("cannot wait for GNOME Settings")?;
    if !status.success() {
        return Err(anyhow!("gnome-control-center exited with {status}").into());
    }
    Ok(())
}

pub(super) fn clear() -> Result<(), Failure> {
    Err(anyhow!(
        "GNOME decides the portal's keys; clear them in GNOME Settings with `voice-input shortcut edit`"
    )
    .into())
}

/// Have the daemon bind again and wait for its answer: the status once it
/// is no longer binding, or `None` when no daemon runs.
async fn apply(paths: &Paths) -> Result<Option<ShortcutStatus>> {
    rebind(paths).await?;
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
                "GNOME has not answered in {}s; if a dialog is open, answer it, then check \
                 `voice-input shortcut status`",
                SETUP_TIMEOUT.as_secs()
            )
        })?
}

/// The daemon's shortcut status, or `None` when it is not running.
pub(super) async fn daemon_status(paths: &Paths) -> Result<Option<ShortcutStatus>> {
    match ipc::request(paths, &v1::Request::ShortcutStatus).await {
        Ok(v1::Reply::ShortcutStatus(status)) => Ok(Some(status)),
        Ok(v1::Reply::Error { message }) => bail!("the daemon could not tell: {message}"),
        Ok(v1::Reply::Ok) => bail!("the daemon answered something else"),
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Fail unless both actions are bound, saying what to do.
fn check_bound(status: &ShortcutStatus) -> Result<(), Failure> {
    match status.session {
        SessionState::Bound
            if status.actual.toggle.is_some() && status.actual.hold.is_some() =>
        {
            Ok(())
        }
        SessionState::Bound => Err(anyhow!(
            "GNOME did not bind both toggle and hold-to-talk; run `voice-input shortcut setup portal` again or check `voice-input shortcut edit`"
        )
        .into()),
        SessionState::Cancelled => Err(anyhow!(
            "the GNOME dialog was cancelled; run `voice-input shortcut setup portal` to be asked again"
        )
        .into()),
        SessionState::Failed => Err(anyhow!(
            "{}; run `voice-input shortcut setup portal` to try again, or use \
             `voice-input shortcut setup gnome-keybinding` without a GlobalShortcuts portal",
            status.last_error.as_deref().unwrap_or("binding failed")
        )
        .into()),
        SessionState::Disabled | SessionState::Binding => Err(anyhow!(
            "the daemon is not using the portal backend; check `voice-input shortcut status`"
        )
        .into()),
    }
}

fn print_status(status: &ShortcutStatus) {
    let session = match status.session {
        SessionState::Disabled => "disabled",
        SessionState::Binding => "binding (a GNOME dialog may be waiting for you)",
        SessionState::Bound => "bound",
        SessionState::Cancelled => "cancelled; `voice-input shortcut setup portal` asks again",
        SessionState::Failed => "failed",
    };
    println!("backend:  {} ({APP_ID})", status.backend);
    println!("session:  {session}");
    if let Some(error) = &status.last_error {
        println!("error:    {error}");
    }
    match status.session {
        SessionState::Failed => println!(
            "next:     retry with `voice-input shortcut setup portal`, or use \
             `voice-input shortcut setup gnome-keybinding` if GlobalShortcuts is unavailable"
        ),
        SessionState::Disabled
        | SessionState::Binding
        | SessionState::Bound
        | SessionState::Cancelled => {}
    }
    let key = |actual: &Option<Trigger>, preferred: Shortcut| match actual {
        Some(Trigger { shortcuts, .. }) if !shortcuts.is_empty() => either(shortcuts),
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
    println!("change keys: voice-input shortcut edit");
}

fn either(shortcuts: &[Shortcut]) -> String {
    shortcuts
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
mod tests {
    use super::{Failure, check_bound, confirm_binding};
    use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus, Trigger};
    use crate::shortcut::{Action, Backend, Shortcut};

    fn outcome(result: Result<(), Failure>) -> Result<(), (u8, String)> {
        result.map_err(|failure| match failure {
            Failure::Usage(message) => (2, message),
            Failure::Runtime(error) => (1, format!("{error:#}")),
        })
    }

    fn keys(text: &str) -> Shortcut {
        text.parse().unwrap()
    }

    #[test]
    fn set_accepts_only_the_binding_gnome_actually_kept() {
        let requested = keys("Super+Y");
        assert_eq!(
            outcome(confirm_binding(
                Action::Toggle,
                requested,
                Some(Trigger::new("Press <Super>y or <Control>F9".to_string())),
            )),
            Ok(())
        );
        assert_eq!(
            outcome(confirm_binding(
                Action::Toggle,
                requested,
                Some(Trigger::new("Press <Super>z".to_string())),
            )),
            Err((
                1,
                "GNOME kept the saved binding Super+Z; change it with `voice-input shortcut edit`"
                    .to_string(),
            ))
        );
        assert_eq!(
            outcome(confirm_binding(
                Action::Hold,
                requested,
                Some(Trigger::new("<Super>yni bosing".to_string())),
            )),
            Err((
                1,
                "GNOME bound hold as \"<Super>yni bosing\", which could not be confirmed as \
                 Super+Y; check it with `voice-input shortcut edit`"
                    .to_string(),
            ))
        );
        assert_eq!(
            outcome(confirm_binding(Action::Hold, requested, None)),
            Err((
                1,
                "GNOME did not bind hold; check it with `voice-input shortcut edit`".to_string(),
            ))
        );
    }

    fn status(session: SessionState) -> ShortcutStatus {
        ShortcutStatus {
            backend: Backend::Portal,
            session,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction {
                toggle: Some(Trigger::new("Press <Super>z".to_string())),
                hold: Some(Trigger::new("Press <Super>x".to_string())),
            },
            last_error: match session {
                SessionState::Failed => Some("no portal".to_string()),
                _ => None,
            },
        }
    }

    #[test]
    fn setup_result_explains_cancelled_and_failed_binding() {
        assert_eq!(outcome(check_bound(&status(SessionState::Bound))), Ok(()));
        assert_eq!(
            [
                SessionState::Cancelled,
                SessionState::Failed,
                SessionState::Disabled,
                SessionState::Binding,
            ]
            .map(|session| outcome(check_bound(&status(session)))),
            [
                Err((
                    1,
                    "the GNOME dialog was cancelled; run `voice-input shortcut setup portal` to be asked again"
                        .to_string(),
                )),
                Err((
                    1,
                    "no portal; run `voice-input shortcut setup portal` to try again, or use `voice-input shortcut setup gnome-keybinding` without a GlobalShortcuts portal"
                        .to_string(),
                )),
                Err((
                    1,
                    "the daemon is not using the portal backend; check `voice-input shortcut status`"
                        .to_string(),
                )),
                Err((
                    1,
                    "the daemon is not using the portal backend; check `voice-input shortcut status`"
                        .to_string(),
                )),
            ]
        );
    }

    #[test]
    fn setup_rejects_an_incomplete_binding() {
        let incomplete = ShortcutStatus {
            actual: PerAction {
                toggle: None,
                hold: Some(Trigger::new("Press <Super>x".to_string())),
            },
            ..status(SessionState::Bound)
        };
        assert_eq!(
            outcome(check_bound(&incomplete)),
            Err((
                1,
                "GNOME did not bind both toggle and hold-to-talk; run `voice-input shortcut setup portal` again or check `voice-input shortcut edit`"
                    .to_string(),
            ))
        );
    }
}
