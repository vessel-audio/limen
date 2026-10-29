//! Operations for GNOME custom keybindings and the unconfigured backend.

use anyhow::{Context, Result, anyhow};

use super::portal;
use super::{Note, Report, SetupError, check_config, rebind};
use crate::config::{self, Paths, ShortcutConfig};
use crate::shortcut::gnome::gsettings::{self, Gsettings};
use crate::shortcut::gnome::{self as keybinding, Clear, Managed};
use crate::shortcut::program::Program;
use crate::shortcut::{Backend, Shortcut};

/// Bind `toggle` to `voice-input toggle` in GNOME, then record it in the
/// config and tell the daemon.
pub(super) async fn bind(
    paths: &Paths,
    current: ShortcutConfig,
    toggle: Shortcut,
    report: &mut Report,
) -> Result<(), SetupError> {
    require_gnome()?;
    let wanted = ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        toggle,
        ..current
    };
    check_config(paths, &wanted)?;
    let dconf = Gsettings;
    let before = gsettings::read(&dconf).await?;
    let managed = match keybinding::managed(&before.list, &before.entries) {
        Managed::Own(entry) | Managed::Adopted(entry) => Some(entry.path.clone()),
        Managed::Absent => None,
    };
    let others: Vec<_> = before
        .entries
        .iter()
        .filter(|entry| Some(&entry.path) != managed.as_ref())
        .cloned()
        .collect();
    let (conflicts, unchecked) = gsettings::find_conflicts(&dconf, toggle, &others).await?;
    report.notes.extend(
        unchecked
            .into_iter()
            .map(|schema| Note::NotChecked(schema.to_string())),
    );
    if !conflicts.is_empty() {
        let owners: Vec<String> = conflicts.iter().map(ToString::to_string).collect();
        return Err(anyhow!(
            "{toggle} is already used by {}; choose another key",
            owners.join(", ")
        )
        .into());
    }
    let program = Program::find()?;
    let plan = keybinding::setup(
        &before.list,
        &before.entries,
        toggle,
        keybinding::toggle_command(program.path()),
    );
    // Keep the daemon's exact portal config so failed GNOME writes can restore it.
    let previous_portal = match portal::daemon_status(paths).await? {
        Some(status) if status.backend == Backend::Portal => Some(ShortcutConfig {
            backend: Backend::Portal,
            toggle: status.preferred.toggle,
            hold: status.preferred.hold,
        }),
        Some(_) | None if current.backend == Backend::Portal => Some(current),
        Some(_) | None => None,
    };
    // The portal must let go before GNOME grabs the same key.
    if let Some(previous) = previous_portal
        && let Err(error) = apply_backend(paths, toggle, report).await
    {
        return Err(restore_portal(paths, previous, error, report).await);
    }
    if let Err(error) = gsettings::apply_setup(&dconf, &before, &plan).await {
        if let Some(previous) = previous_portal {
            return Err(restore_portal(paths, previous, error, report).await);
        }
        return Err(error.into());
    }
    apply_backend(paths, toggle, report).await?;
    report.notes.push(Note::GnomeBinding {
        keys: toggle,
        command: plan.command,
        program: program.clone(),
    });
    if let Program::Unstable(path) = program {
        report.notes.push(Note::UnstableLauncher(path));
    }
    Ok(())
}

async fn apply_backend(paths: &Paths, toggle: Shortcut, report: &mut Report) -> Result<()> {
    config::update_shortcut(&paths.config_file, |latest| ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        toggle,
        ..latest
    })
    .with_context(|| {
        format!("GNOME now runs voice-input on {toggle}, but the config was not updated; run the command again once it can be")
    })?;
    rebind(paths, report).await
}

async fn restore_portal(
    paths: &Paths,
    previous: ShortcutConfig,
    error: anyhow::Error,
    report: &mut Report,
) -> SetupError {
    let restored = async {
        config::update_shortcut(&paths.config_file, move |_| previous)?;
        rebind(paths, report).await
    }
    .await;
    match restored {
        Ok(()) => error.into(),
        Err(restoration) => error
            .context(format!(
                "restoring the portal binding also failed: {restoration:#}"
            ))
            .into(),
    }
}

pub(super) async fn setup_none(
    paths: &Paths,
    current: ShortcutConfig,
    report: &mut Report,
) -> Result<(), SetupError> {
    let wanted = ShortcutConfig {
        backend: Backend::None,
        ..current
    };
    check_config(paths, &wanted)?;
    clear_ours().await?;
    config::update_shortcut(&paths.config_file, |latest| ShortcutConfig {
        backend: Backend::None,
        ..latest
    })
    .context("the GNOME custom shortcut is cleared, but the config was not updated; run the command again once it can be")?;
    rebind(paths, report).await?;
    Ok(())
}

impl From<Clear> for Note {
    fn from(clear: Clear) -> Self {
        match clear {
            Clear::Nothing => Self::NoCustomShortcut,
            Clear::Remove { .. } => Self::CustomShortcutRemoved,
            Clear::Disable { path } => Self::CustomShortcutDisabled(path),
        }
    }
}

pub(super) async fn clear(report: &mut Report) -> Result<(), SetupError> {
    require_gnome()?;
    let note = Note::from(clear_ours().await?);
    report.notes.push(note);
    Ok(())
}

/// Clear the GNOME custom shortcut we manage, so it cannot fire beside another backend.
pub(super) async fn clear_ours() -> Result<Clear> {
    match is_gnome() {
        false => Ok(Clear::Nothing),
        true => {
            let dconf = Gsettings;
            let before = gsettings::read(&dconf).await?;
            let plan = keybinding::clear(&before.list, &before.entries);
            gsettings::apply_clear(&dconf, &before, &plan).await?;
            Ok(plan)
        }
    }
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
        desktops
            .split(':')
            .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
    })
}

fn require_gnome() -> Result<()> {
    match is_gnome() {
        true => Ok(()),
        false => Err(anyhow!(
            "the gnome-keybinding backend needs a GNOME session (XDG_CURRENT_DESKTOP is {:?})",
            std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::Note;
    use crate::shortcut::gnome::Clear;

    #[test]
    fn clear_results_become_their_report_notes() {
        let path = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/manual/";
        assert_eq!(
            [
                Note::from(Clear::Nothing),
                Note::from(Clear::Remove {
                    path: path.to_string(),
                    list: Vec::new(),
                }),
                Note::from(Clear::Disable {
                    path: path.to_string(),
                }),
            ],
            [
                Note::NoCustomShortcut,
                Note::CustomShortcutRemoved,
                Note::CustomShortcutDisabled(path.to_string()),
            ]
        );
    }
}
