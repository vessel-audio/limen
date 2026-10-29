//! Operations for the GlobalShortcuts portal backend.

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;

pub(super) use super::daemon_status;
use super::gnome;
use super::{Keys, Note, Report, SetupError, check_config, check_distinct, rebind};
use crate::config::{self, Paths, ShortcutConfig};
use crate::shortcut::desktop_entry::{self, APP_ID, Install};
use crate::shortcut::program::Program;
use crate::shortcut::{Action, Backend, Shortcut};

impl From<(&Program, Install)> for Report {
    fn from((program, install): (&Program, Install)) -> Self {
        let notes = match (install, program) {
            (Install::Installed(path), Program::Unstable(program)) => vec![
                Note::Installed(path),
                Note::UnstableLauncher(program.clone()),
            ],
            (Install::Installed(path), Program::Stable(_)) => vec![Note::Installed(path)],
            (Install::Present(_), Program::Stable(_) | Program::Unstable(_)) => Vec::new(),
        };
        Self {
            notes,
            ..Self::default()
        }
    }
}

/// Record portal preferences, install the app entry, and ask the daemon to rebind.
pub(super) async fn setup(
    paths: &Paths,
    current: ShortcutConfig,
    keys: Keys,
    report: &mut Report,
) -> Result<(), SetupError> {
    let preferred = move |config: ShortcutConfig| ShortcutConfig {
        backend: Backend::Portal,
        toggle: keys.toggle.unwrap_or(config.toggle),
        hold: keys.hold.unwrap_or(config.hold),
    };
    let wanted = preferred(current);
    check_distinct(wanted.toggle, wanted.hold)?;
    check_config(paths, &wanted)?;
    let program = Program::find()?;
    let installation = desktop_entry::ensure_installed(program.path())?;
    report
        .notes
        .extend(Report::from((&program, installation)).notes);
    gnome::clear_ours().await?;
    config::update_shortcut(&paths.config_file, preferred)?;
    rebind(paths, report).await?;
    Ok(())
}

/// Suggest a new key and ask the daemon to bind again. The caller observes
/// GNOME's response separately through shortcut status.
pub(super) async fn set(
    paths: &Paths,
    current: ShortcutConfig,
    action: Action,
    keys: Shortcut,
    report: &mut Report,
) -> Result<(), SetupError> {
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

/// Open this application's page in GNOME Settings.
pub(super) async fn edit() -> Result<(), SetupError> {
    let mut child = Command::new("gnome-control-center")
        .args(["applications", APP_ID])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot open GNOME Settings; change the keys under Apps → voice-input")?;
    let status = child
        .wait()
        .await
        .context("cannot wait for GNOME Settings")?;
    match status.success() {
        true => Ok(()),
        false => Err(anyhow::anyhow!("gnome-control-center exited with {status}").into()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Report;
    use crate::shortcut::desktop_entry::Install;
    use crate::shortcut::program::Program;
    use crate::shortcut::setup::Note;

    #[test]
    fn installation_reports_include_only_relevant_notes() {
        let installed = PathBuf::from("/home/me/.local/share/applications/voice-input.desktop");
        let stable = Program::Stable(PathBuf::from("/home/me/.local/bin/voice-input"));
        let unstable_path = PathBuf::from("/nix/store/voice-input/bin/voice-input");
        let unstable = Program::Unstable(unstable_path.clone());
        assert_eq!(
            [
                Report::from((&stable, Install::Installed(installed.clone()))),
                Report::from((&unstable, Install::Installed(installed.clone()))),
                Report::from((&unstable, Install::Present(installed.clone()))),
            ],
            [
                Report {
                    notes: vec![Note::Installed(installed.clone())],
                    ..Report::default()
                },
                Report {
                    notes: vec![
                        Note::Installed(installed),
                        Note::UnstableLauncher(unstable_path),
                    ],
                    ..Report::default()
                },
                Report::default(),
            ]
        );
    }
}
