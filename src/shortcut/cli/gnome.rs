//! `voice-input shortcut` for the gnome-keybinding and none backends.

use anyhow::{Context, Result, anyhow, bail};

use super::{Failure, check_config, rebind};
use crate::config::{self, Paths, ShortcutConfig};
use crate::shortcut::gnome::gsettings::{self, Gsettings};
use crate::shortcut::gnome::{self as keybinding, Clear, CustomKeybinding, Managed};
use crate::shortcut::program::Program;
use crate::shortcut::{Backend, Shortcut};

const NO_HOLD: &str =
    "not supported by this backend; for hold-to-talk run `voice-input shortcut setup portal`";

pub(super) fn status_none() {
    println!("backend:  none");
    println!("toggle:   bind `voice-input toggle` to a key in your desktop settings");
    println!("hold:     {NO_HOLD}");
}

pub(super) async fn status() -> Result<(), Failure> {
    let bindings = gsettings::read(&Gsettings).await?;
    println!("backend:  gnome-keybinding");
    match keybinding::managed(&bindings.list, &bindings.entries) {
        Managed::Own(entry) | Managed::Adopted(entry) => println!("toggle:   {}", describe(entry)),
        Managed::Absent => {
            println!("toggle:   not bound; run `voice-input shortcut setup gnome-keybinding`")
        }
    }
    println!("hold:     {NO_HOLD}");
    Ok(())
}

/// The key, the command, and whether the command survives upgrades.
fn describe(entry: &CustomKeybinding) -> String {
    let key = match entry.binding.as_str() {
        "" => "disabled".to_string(),
        binding => Shortcut::parse_accelerator(binding)
            .map_or_else(|_| binding.to_string(), |shortcut| shortcut.to_string()),
    };
    let program = keybinding::program_of(&entry.command).map(Program::of);
    format!(
        "{key} → {} ({})",
        entry.command,
        stability(program.as_ref())
    )
}

fn stability(program: Option<&Program>) -> &'static str {
    match program {
        Some(Program::Stable(_)) => "stable path",
        Some(Program::Unstable(_)) => "Nix store path: breaks after an upgrade; run setup again",
        None => "unrecognized command",
    }
}

/// Bind `toggle` to `voice-input toggle` in GNOME, then record it in the
/// config and tell the daemon.
pub(super) async fn bind(
    paths: &Paths,
    current: ShortcutConfig,
    toggle: Shortcut,
) -> Result<(), Failure> {
    require_gnome()?;
    let wanted = ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        toggle,
        ..current
    };
    check_config(paths, &current, &wanted)?;
    let dconf = Gsettings;
    let before = gsettings::read(&dconf).await?;
    let managed = match keybinding::managed(&before.list, &before.entries) {
        Managed::Own(entry) | Managed::Adopted(entry) => Some(entry.path.clone()),
        Managed::Absent => None,
    };
    let others: Vec<CustomKeybinding> = before
        .entries
        .iter()
        .filter(|entry| Some(&entry.path) != managed.as_ref())
        .cloned()
        .collect();
    let (conflicts, unchecked) = gsettings::find_conflicts(&dconf, toggle, &others).await?;
    for schema in unchecked {
        println!("not checked: {schema} (missing from this GNOME)");
    }
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
    // The running daemon can still hold a portal binding even when the
    // config was already edited to this backend, e.g. by home-manager.
    // Keep its exact config so a failed GNOME write can restore it.
    let previous_portal = match super::portal::daemon_status(paths).await? {
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
        && let Err(error) = apply_backend(paths, toggle).await
    {
        return Err(restore_portal(paths, previous, error).await);
    }
    if let Err(error) = gsettings::apply_setup(&dconf, &before, &plan).await {
        if let Some(previous) = previous_portal {
            return Err(restore_portal(paths, previous, error).await);
        }
        return Err(error.into());
    }
    apply_backend(paths, toggle).await?;
    println!(
        "toggle:   {toggle} → {} ({})",
        plan.command,
        stability(Some(&program))
    );
    if let Program::Unstable(path) = &program {
        eprintln!(
            "warning: {} is in the Nix store and stops working after an upgrade or garbage \
             collection; install voice-input into your profile and run setup again",
            path.display()
        );
    }
    Ok(())
}

/// Record the gnome-keybinding backend in the config and have the daemon
/// apply it. Idempotent, so it can run before and after the dconf change.
async fn apply_backend(paths: &Paths, toggle: Shortcut) -> Result<()> {
    config::update_shortcut(&paths.config_file, |latest| ShortcutConfig {
        backend: Backend::GnomeKeybinding,
        toggle,
        ..latest
    })
    .with_context(|| {
        format!("GNOME now runs voice-input on {toggle}, but the config was not updated; run the command again once it can be")
    })?;
    rebind(paths).await
}

/// Restore `previous` after a failed backend change, keeping the original
/// error unless the restoration itself fails too.
async fn restore_portal(paths: &Paths, previous: ShortcutConfig, error: anyhow::Error) -> Failure {
    let restored = async {
        config::update_shortcut(&paths.config_file, move |_| previous)?;
        rebind(paths).await
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

pub(super) async fn setup_none(paths: &Paths, current: ShortcutConfig) -> Result<(), Failure> {
    let wanted = ShortcutConfig {
        backend: Backend::None,
        ..current
    };
    check_config(paths, &current, &wanted)?;
    clear_ours().await?;
    config::update_shortcut(&paths.config_file, |latest| ShortcutConfig {
        backend: Backend::None,
        ..latest
    })
    .context("the GNOME custom shortcut is cleared, but the config was not updated; run the command again once it can be")?;
    rebind(paths).await?;
    status_none();
    Ok(())
}

/// `voice-input shortcut clear toggle`
pub(super) async fn clear() -> Result<(), Failure> {
    require_gnome()?;
    match clear_ours().await? {
        Clear::Nothing => println!("No GNOME custom shortcut runs `voice-input toggle`."),
        Clear::Remove { .. } => println!("Removed the GNOME custom shortcut."),
        Clear::Disable { path } => {
            println!("Disabled the GNOME custom shortcut at {path}; it stays otherwise.")
        }
    }
    Ok(())
}

/// Clear the GNOME custom shortcut we manage, so that it cannot fire next to
/// another backend. Outside GNOME there is none.
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
        false => bail!(
            "the gnome-keybinding backend needs a GNOME session (XDG_CURRENT_DESKTOP is {:?})",
            std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
        ),
    }
}
