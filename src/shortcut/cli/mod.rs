//! The `voice-input shortcut` and `voice-input hold` commands.
//!
//! Exit codes: 0 on success; 1 when something at runtime fails (the daemon
//! is unreachable, an external command fails, the config cannot be written,
//! the backend cannot do what was asked); 2 when the arguments can never
//! work, e.g. invalid keys.

mod gnome;
mod portal;

use std::fs;
use std::io::ErrorKind;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::Subcommand;

use super::{Action, Backend, HoldEdge, Shortcut, ShortcutError};
use crate::config::{self, Config, Paths, ShortcutConfig};
use crate::ipc::{self, v1};

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
        /// Key to hold while talking (portal backend only)
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

/// Why a command failed.
#[derive(Debug)]
enum Failure {
    /// The arguments can never work: exit code 2.
    Usage(String),
    /// Exit code 1.
    Runtime(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Runtime(error)
    }
}

/// `voice-input shortcut ...`
pub(crate) async fn run(paths: &Paths, command: ShortcutCommand) -> Result<ExitCode> {
    match dispatch(paths, command).await {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(Failure::Usage(message)) => {
            eprintln!("error: {message}");
            Ok(ExitCode::from(2))
        }
        Err(Failure::Runtime(error)) => Err(error),
    }
}

async fn dispatch(paths: &Paths, command: ShortcutCommand) -> Result<(), Failure> {
    let current = configured(paths)?;
    match command {
        ShortcutCommand::Status => match current.backend {
            Backend::Portal => portal::status(paths, current).await,
            Backend::GnomeKeybinding => gnome::status().await,
            Backend::None => {
                gnome::status_none();
                Ok(())
            }
        },
        ShortcutCommand::Setup {
            backend,
            toggle,
            hold,
        } => match (backend, hold) {
            (Backend::Portal, _) => portal::setup(paths, current, toggle, hold).await,
            (Backend::GnomeKeybinding, Some(_)) => Err(Failure::Usage(
                "--hold is only used by the portal backend".to_string(),
            )),
            (Backend::GnomeKeybinding, None) => {
                let toggle = toggle.unwrap_or(current.toggle);
                check_distinct(toggle, current.hold)?;
                gnome::bind(paths, current, toggle).await
            }
            (Backend::None, _) => match (toggle, hold) {
                (None, None) => gnome::setup_none(paths, current).await,
                (Some(_), _) | (_, Some(_)) => {
                    Err(Failure::Usage("`setup none` binds no keys".to_string()))
                }
            },
        },
        ShortcutCommand::Set { action, keys } => match (current.backend, action) {
            (Backend::Portal, _) => portal::set(paths, current, action, keys).await,
            (Backend::GnomeKeybinding, Action::Toggle) => {
                check_distinct(keys, current.hold)?;
                gnome::bind(paths, current, keys).await
            }
            (Backend::GnomeKeybinding, Action::Hold) => Err(anyhow!(
                "the gnome-keybinding backend cannot hold to talk; \
                 for hold-to-talk run `voice-input shortcut setup portal`"
            )
            .into()),
            (Backend::None, _) => Err(not_set_up()),
        },
        ShortcutCommand::Clear { action } => match (current.backend, action) {
            (Backend::Portal, _) => portal::clear(),
            (Backend::GnomeKeybinding, Action::Toggle) => gnome::clear().await,
            (Backend::GnomeKeybinding, Action::Hold) => {
                Err(anyhow!("the gnome-keybinding backend has no hold-to-talk key").into())
            }
            (Backend::None, _) => Err(not_set_up()),
        },
        ShortcutCommand::Edit => match current.backend {
            Backend::Portal => portal::edit().await,
            Backend::GnomeKeybinding => {
                Err(anyhow!("change the key with `voice-input shortcut set toggle <KEYS>`").into())
            }
            Backend::None => Err(not_set_up()),
        },
    }
}

fn not_set_up() -> Failure {
    anyhow!(
        "no shortcut backend is set up; run `voice-input shortcut setup <portal|gnome-keybinding>` first"
    )
    .into()
}

/// The `[shortcut]` section of the config, or the defaults while there is no
/// config to read (a dangling link to one included). A config that does not
/// parse or validate can never work: exit code 2.
fn configured(paths: &Paths) -> Result<ShortcutConfig, Failure> {
    let path = &paths.config_file;
    match fs::read_to_string(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ShortcutConfig::default()),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("failed to read config: {}", path.display()))
            .into()),
        Ok(text) => Config::parse(&text)
            .map(|config| config.shortcut)
            .map_err(|error| {
                Failure::Usage(format!("invalid config {}: {error:#}", path.display()))
            }),
    }
}

/// The toggle and the hold-to-talk key cannot share one key.
fn check_distinct(toggle: Shortcut, hold: Shortcut) -> Result<(), Failure> {
    match toggle == hold {
        true => Err(Failure::Usage(format!(
            "{toggle} cannot be both the toggle and the hold-to-talk key; choose another key"
        ))),
        false => Ok(()),
    }
}

/// Fail before changing anything else when the config would have to be
/// written but cannot be, showing what to add to it by hand. Setup writes a
/// config that does not exist yet (behind a dangling link, too) even when the
/// section keeps its defaults.
fn check_config(paths: &Paths, current: &ShortcutConfig, wanted: &ShortcutConfig) -> Result<()> {
    let unchanged = current == wanted && paths.config_file.exists();
    match (unchanged, config::check_writable(&paths.config_file)) {
        (true, _) | (false, Ok(())) => Ok(()),
        (false, Err(refusal)) => bail!(
            "{refusal}, so it was left unchanged. Add this to the config instead and \
             run the command again:\n\n{}",
            config::snippet(wanted)
        ),
    }
}

/// Have a running daemon apply the `[shortcut]` section; one that is not
/// running reads it when it starts.
async fn rebind(paths: &Paths) -> Result<()> {
    match ipc::request(paths, &v1::Request::ShortcutRebind).await {
        Ok(v1::Reply::Ok) => Ok(()),
        Ok(v1::Reply::Error { message }) => {
            bail!("the daemon did not apply the change: {message}")
        }
        Ok(v1::Reply::ShortcutStatus(_)) => bail!("the daemon answered something else"),
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => {
            println!("The daemon is not running; the change takes effect when it starts.");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// `voice-input hold <press|release>`: report an edge of the hold-to-talk key,
/// e.g. from a compositor's key-press and key-release bindings.
pub(crate) async fn hold(paths: &Paths, edge: HoldEdge) -> Result<()> {
    match ipc::request(paths, &v1::Request::Hold { edge }).await? {
        v1::Reply::Ok => Ok(()),
        v1::Reply::Error { message } => bail!("the daemon refused: {message}"),
        v1::Reply::ShortcutStatus(_) => bail!("the daemon answered something else"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::{Failure, check_config, configured};
    use crate::config::{Paths, ShortcutConfig};

    fn paths(config_file: PathBuf) -> Paths {
        Paths {
            socket: config_file.with_file_name("voice-input.sock"),
            log_file: config_file.with_file_name("daemon.log"),
            cache_dir: config_file.with_file_name("cache"),
            config_file,
        }
    }

    #[test]
    fn config_problems_map_to_their_exit_codes() {
        let dir = std::env::temp_dir().join(format!("voice-input-cli-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a-directory.toml")).unwrap();
        let invalid = dir.join("invalid.toml");
        fs::write(
            &invalid,
            "[provider]\nurl = \"http://127.0.0.1:8000\"\nmodel = \"m\"\n\n[shortcut]\ntoggle = \"Shift+A\"\n",
        )
        .unwrap();
        let dangling = dir.join("dangling.toml");
        symlink(dir.join("missing.toml"), &dangling).unwrap();

        assert!(matches!(
            configured(&paths(invalid)),
            Err(Failure::Usage(message)) if message.contains("shortcut.toggle: Shift+A")
        ));
        assert!(matches!(
            configured(&paths(dir.join("a-directory.toml"))),
            Err(Failure::Runtime(_))
        ));
        for absent in [dir.join("missing.toml"), dangling] {
            assert!(matches!(
                configured(&paths(absent)),
                Ok(shortcut) if shortcut == ShortcutConfig::default()
            ));
        }
    }

    #[test]
    fn a_dangling_config_is_refused_before_anything_changes() {
        let dir =
            std::env::temp_dir().join(format!("voice-input-cli-check-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dangling = dir.join("config.toml");
        symlink(dir.join("missing.toml"), &dangling).unwrap();
        let defaults = ShortcutConfig::default();

        assert!(check_config(&paths(dangling), &defaults, &defaults).is_err());
        assert!(check_config(&paths(dir.join("new.toml")), &defaults, &defaults).is_ok());
    }
}
