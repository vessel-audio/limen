//! Rendering and command-line behavior for global shortcuts.
//!
//! Exit codes: 0 on success; 1 for runtime failures; 2 for arguments or
//! configuration that can never work.

#[cfg(target_os = "macos")]
mod carbon;
mod gnome;
#[cfg(target_os = "linux")]
mod portal;

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Subcommand;

use limen_core::{
    Action, Backend, HoldEdge, Keys, Launch, Note, Paths, Report, SetupError, Shortcut,
    ShortcutError, clear, edit, request, set, setup, shortcut_config, v1,
};

#[derive(Debug, Subcommand)]
pub(crate) enum ShortcutCommand {
    /// Show the backend and the keys it binds
    Status,
    /// Choose how shortcuts are bound, and bind them
    Setup {
        #[arg(value_enum)]
        backend: Backend,
        /// Key that starts and stops dictation, e.g. Super+Z or <Super>z
        #[arg(long, value_name = "KEYS", value_parser = parse_keys)]
        toggle: Option<Shortcut>,
        /// Key to hold while talking (portal and carbon backends)
        #[arg(long, value_name = "KEYS", value_parser = parse_keys)]
        hold: Option<Shortcut>,
    },
    /// Change the key of an action
    Set {
        #[arg(value_enum)]
        action: Action,
        #[arg(value_name = "KEYS", value_parser = parse_keys)]
        keys: Shortcut,
    },
    /// Remove the key of an action
    Clear {
        #[arg(value_enum)]
        action: Action,
    },
    /// Change the keys in GNOME Settings (portal backend)
    Edit,
}

fn parse_keys(text: &str) -> Result<Shortcut, ShortcutError> {
    text.parse::<Shortcut>().and_then(Shortcut::validate)
}

/// `limen shortcut ...`
pub(crate) async fn run(paths: &Paths, command: ShortcutCommand) -> Result<ExitCode> {
    match dispatch(paths, command).await {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            render(error.report());
            match error {
                SetupError::Usage { message, .. } => {
                    eprintln!("error: {message}");
                    Ok(ExitCode::from(2))
                }
                SetupError::Runtime { error, .. } => Err(error),
            }
        }
    }
}

async fn dispatch(paths: &Paths, command: ShortcutCommand) -> Result<(), SetupError> {
    let current = shortcut_config(paths)?;
    match command {
        ShortcutCommand::Status => match current.backend {
            #[cfg(target_os = "linux")]
            Backend::Portal => portal::status(paths, current).await,
            #[cfg(not(target_os = "linux"))]
            Backend::Portal => {
                eprintln!("the portal backend is only available on Linux");
                Ok(())
            }
            Backend::GnomeKeybinding => gnome::status().await,
            #[cfg(target_os = "macos")]
            Backend::Carbon => carbon::status(paths, current).await,
            #[cfg(not(target_os = "macos"))]
            Backend::Carbon => {
                eprintln!("the carbon backend is only available on macOS");
                Ok(())
            }
            Backend::None => {
                gnome::status_none();
                Ok(())
            }
        },
        ShortcutCommand::Setup {
            backend,
            toggle,
            hold,
        } => {
            let report = setup(
                paths,
                backend,
                Keys { toggle, hold },
                if cfg!(feature = "gui") {
                    Launch::Gui
                } else {
                    Launch::Daemon
                },
            )
            .await?;
            render(&report);
            match backend {
                #[cfg(target_os = "linux")]
                Backend::Portal => portal::finish_setup(paths).await,
                #[cfg(not(target_os = "linux"))]
                Backend::Portal => Ok(()),
                Backend::GnomeKeybinding => Ok(()),
                #[cfg(target_os = "macos")]
                Backend::Carbon => carbon::finish_setup(paths).await,
                #[cfg(not(target_os = "macos"))]
                Backend::Carbon => Ok(()),
                Backend::None => {
                    gnome::status_none();
                    Ok(())
                }
            }
        }
        ShortcutCommand::Set { action, keys } => {
            let report = set(paths, action, keys).await?;
            render(&report);
            match report.backend() {
                #[cfg(target_os = "linux")]
                Backend::Portal => portal::finish_set(paths, action, keys).await,
                #[cfg(not(target_os = "linux"))]
                Backend::Portal => Ok(()),
                #[cfg(target_os = "macos")]
                Backend::Carbon => carbon::finish_set(paths, action, keys).await,
                #[cfg(not(target_os = "macos"))]
                Backend::Carbon => Ok(()),
                Backend::GnomeKeybinding | Backend::None => Ok(()),
            }
        }
        ShortcutCommand::Clear { action } => {
            let report = clear(paths, action).await?;
            render(&report);
            Ok(())
        }
        ShortcutCommand::Edit => {
            let report = edit(paths).await?;
            render(&report);
            Ok(())
        }
    }
}

fn render(report: &Report) {
    for note in &report.notes {
        match note {
            Note::Installed(path) => println!("Installed {}.", path.display()),
            Note::UnstableLauncher(path) => eprintln!(
                "warning: {} is in the Nix store and stops working after an upgrade or garbage \
                 collection; install limen into your profile and run setup again",
                path.display()
            ),
            Note::NotChecked(schema) => {
                println!("not checked: {schema} (missing from this GNOME)")
            }
            Note::DaemonNotRunning => {
                println!("The daemon is not running; the change takes effect when it starts.")
            }
            Note::GnomeBinding {
                keys,
                command,
                program,
            } => println!(
                "toggle:   {keys} → {command} ({})",
                gnome::stability(Some(program))
            ),
            Note::NoCustomShortcut => {
                println!("No GNOME custom shortcut runs `limen toggle`.")
            }
            Note::CustomShortcutRemoved => println!("Removed the GNOME custom shortcut."),
            Note::CustomShortcutDisabled(path) => {
                println!("Disabled the GNOME custom shortcut at {path}; it stays otherwise.")
            }
        }
    }
}

/// `limen hold <press|release>`: report an edge of the hold-to-talk key.
pub(crate) async fn hold(paths: &Paths, edge: HoldEdge) -> Result<()> {
    match request(paths, &v1::Request::Hold { edge }).await? {
        v1::Reply::Ok => Ok(()),
        v1::Reply::Error { message } => bail!("the daemon refused: {message}"),
        v1::Reply::ShortcutStatus(_) | v1::Reply::Accepted { .. } => {
            bail!("the daemon answered something else")
        }
    }
}
