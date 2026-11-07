//! Operations for the Carbon hotkey backend (macOS).
//!
//! Unlike the portal there is nothing to install and no desktop dialog: the
//! daemon registers the configured keys with Carbon directly, so setup is
//! write-the-config plus rebind.

#![cfg(target_os = "macos")]

use super::{Keys, Report, SetupError, check_config, check_distinct, rebind};
use crate::config::{self, Paths, ShortcutConfig};
use crate::shortcut::{Action, Backend, Shortcut};

/// Record Carbon preferences and ask the daemon to rebind.
pub(super) async fn setup(
    paths: &Paths,
    current: ShortcutConfig,
    keys: Keys,
    report: &mut Report,
) -> Result<(), SetupError> {
    let preferred = move |config: ShortcutConfig| ShortcutConfig {
        backend: Backend::Carbon,
        toggle: keys.toggle.unwrap_or(config.toggle),
        hold: keys.hold.unwrap_or(config.hold),
    };
    let wanted = preferred(current);
    check_distinct(wanted.toggle, wanted.hold)?;
    check_config(paths, &wanted)?;
    config::update_shortcut(&paths.config_file, preferred)?;
    rebind(paths, report).await?;
    Ok(())
}

/// Change one action's key and ask the daemon to register again.
pub(super) async fn set(
    paths: &Paths,
    action: Action,
    keys: Shortcut,
    report: &mut Report,
) -> Result<(), SetupError> {
    let current = super::configured(paths)?;
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
    rebind(paths, report).await.map_err(Into::into)
}
