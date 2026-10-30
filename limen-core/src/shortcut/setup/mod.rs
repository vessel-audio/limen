//! Shortcut operations shared by interactive frontends.
//!
//! These functions change configuration and desktop state without printing.
//! Reports describe user-visible side effects for the caller to render.

mod gnome;
mod portal;

pub use gnome::{GnomeBinding, GnomeStatus, gnome_status};

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use super::desktop_entry::Launch;
use super::program::Program;
use super::status::ShortcutStatus;
use super::{Action, Backend, Shortcut};
use crate::config::{self, Config, Paths, ShortcutConfig};
use crate::ipc::{self, v1};

/// Keys optionally supplied while choosing a backend. Omitted keys retain
/// their configured values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys {
    /// A new toggle key, or `None` to retain the configured key.
    pub toggle: Option<Shortcut>,
    /// A new hold-to-talk key, or `None` to retain the configured key.
    pub hold: Option<Shortcut>,
}

/// A user-visible effect of a shortcut operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// A desktop entry was installed at this path.
    Installed(PathBuf),
    /// The selected executable will not survive a Nix upgrade.
    UnstableLauncher(PathBuf),
    /// A GNOME schema was unavailable during conflict checking.
    NotChecked(String),
    /// The daemon will apply the configuration when it next starts.
    DaemonNotRunning,
    /// GNOME now runs this command for the toggle action.
    GnomeBinding {
        keys: Shortcut,
        command: String,
        program: Program,
    },
    /// No managed GNOME custom shortcut existed.
    NoCustomShortcut,
    /// The managed GNOME custom shortcut was removed.
    CustomShortcutRemoved,
    /// An adopted custom shortcut was disabled but otherwise preserved.
    CustomShortcutDisabled(String),
}

/// The effects of a completed or partially completed operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Side effects in the order the operation produced them.
    pub notes: Vec<Note>,
    pub(super) backend: Backend,
}

/// Why a shortcut operation failed, together with effects that happened
/// before the failure.
#[derive(Debug)]
pub enum SetupError {
    /// The request or configuration can never work.
    Usage { message: String, report: Report },
    /// An external operation failed.
    Runtime {
        error: anyhow::Error,
        report: Report,
    },
}

impl Report {
    /// The backend whose live status the caller should observe after `set`.
    pub fn backend(&self) -> Backend {
        self.backend
    }
}

impl SetupError {
    fn usage(message: impl Into<String>) -> Self {
        Self::Usage {
            message: message.into(),
            report: Report::default(),
        }
    }

    fn with_report(self, report: Report) -> Self {
        match self {
            Self::Usage { message, .. } => Self::Usage { message, report },
            Self::Runtime { error, .. } => Self::Runtime { error, report },
        }
    }

    /// Effects produced before the operation succeeded or failed.
    pub fn report(&self) -> &Report {
        match self {
            Self::Usage { report, .. } | Self::Runtime { report, .. } => report,
        }
    }
}

impl From<anyhow::Error> for SetupError {
    fn from(error: anyhow::Error) -> Self {
        Self::Runtime {
            error,
            report: Report::default(),
        }
    }
}

/// Choose a shortcut backend and optionally replace its preferred keys.
pub async fn setup(
    paths: &Paths,
    backend: Backend,
    keys: Keys,
    launch: Launch,
) -> Result<Report, SetupError> {
    let mut report = Report::default();
    let result = match configured(paths) {
        Err(error) => Err(error),
        Ok(current) => match (backend, keys.hold) {
            (Backend::GnomeKeybinding, Some(_)) => Err(SetupError::usage(
                "--hold is only used by the portal backend",
            )),
            (Backend::Portal, _) => portal::setup(paths, current, keys, &mut report, launch).await,
            (Backend::GnomeKeybinding, None) => {
                let toggle = keys.toggle.unwrap_or(current.toggle);
                match check_distinct(toggle, current.hold) {
                    Ok(()) => gnome::bind(paths, current, toggle, &mut report).await,
                    Err(error) => Err(error),
                }
            }
            (Backend::None, _) => match (keys.toggle, keys.hold) {
                (None, None) => gnome::setup_none(paths, current, &mut report).await,
                (Some(_), _) | (_, Some(_)) => Err(SetupError::usage("`setup none` binds no keys")),
            },
        },
    };
    complete(result, report, backend)
}

/// Change one action's preferred or managed key.
pub async fn set(paths: &Paths, action: Action, keys: Shortcut) -> Result<Report, SetupError> {
    let mut report = Report::default();
    let mut backend = Backend::None;
    let result = match configured(paths) {
        Err(error) => Err(error),
        Ok(current) => {
            backend = current.backend;
            match (current.backend, action) {
                (Backend::Portal, _) => {
                    portal::set(paths, current, action, keys, &mut report).await
                }
                (Backend::GnomeKeybinding, Action::Toggle) => {
                    match check_distinct(keys, current.hold) {
                        Ok(()) => gnome::bind(paths, current, keys, &mut report).await,
                        Err(error) => Err(error),
                    }
                }
                (Backend::GnomeKeybinding, Action::Hold) => Err(anyhow!(
                    "the gnome-keybinding backend cannot hold to talk; \
                     for hold-to-talk run `limen shortcut setup portal`"
                )
                .into()),
                (Backend::None, _) => Err(not_set_up()),
            }
        }
    };
    complete(result, report, backend)
}

/// Remove one action's managed key where the backend supports doing so.
pub async fn clear(paths: &Paths, action: Action) -> Result<Report, SetupError> {
    let mut report = Report::default();
    let mut backend = Backend::None;
    let result = match configured(paths) {
        Err(error) => Err(error),
        Ok(current) => {
            backend = current.backend;
            match (current.backend, action) {
                (Backend::Portal, _) => Err(anyhow!(
                    "GNOME decides the portal's keys; clear them in GNOME Settings with `limen shortcut edit`"
                )
                .into()),
                (Backend::GnomeKeybinding, Action::Toggle) => gnome::clear(&mut report).await,
                (Backend::GnomeKeybinding, Action::Hold) => {
                    Err(anyhow!("the gnome-keybinding backend has no hold-to-talk key").into())
                }
                (Backend::None, _) => Err(not_set_up()),
            }
        }
    };
    complete(result, report, backend)
}

/// Open the backend's native key editor.
pub async fn edit(paths: &Paths) -> Result<Report, SetupError> {
    let mut backend = Backend::None;
    let result = match configured(paths) {
        Err(error) => Err(error),
        Ok(current) => {
            backend = current.backend;
            match current.backend {
                Backend::Portal => portal::edit().await,
                Backend::GnomeKeybinding => {
                    Err(anyhow!("change the key with `limen shortcut set toggle <KEYS>`").into())
                }
                Backend::None => Err(not_set_up()),
            }
        }
    };
    complete(result, Report::default(), backend)
}

fn complete(
    result: Result<(), SetupError>,
    mut report: Report,
    backend: Backend,
) -> Result<Report, SetupError> {
    report.backend = backend;
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(error.with_report(report)),
    }
}

fn not_set_up() -> SetupError {
    anyhow!(
        "no shortcut backend is set up; run `limen shortcut setup <portal|gnome-keybinding>` first"
    )
    .into()
}

/// Read only the shortcut section, retaining defaults when no config exists.
pub fn configured(paths: &Paths) -> Result<ShortcutConfig, SetupError> {
    let path = &paths.config_file;
    match fs::read_to_string(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ShortcutConfig::default()),
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("failed to read config: {}", path.display()))
            .into()),
        Ok(text) => Config::parse(&text)
            .map(|config| config.shortcut)
            .map_err(|error| {
                SetupError::usage(format!("invalid config {}: {error:#}", path.display()))
            }),
    }
}

fn check_distinct(toggle: Shortcut, hold: Shortcut) -> Result<(), SetupError> {
    match toggle == hold {
        true => Err(SetupError::usage(format!(
            "{toggle} cannot be both the toggle and the hold-to-talk key; choose another key"
        ))),
        false => Ok(()),
    }
}

/// Refuse to change desktop state behind a config that cannot be replaced.
fn check_config(paths: &Paths, wanted: &ShortcutConfig) -> Result<()> {
    match config::check_writable(&paths.config_file) {
        Ok(()) => Ok(()),
        Err(refusal) => bail!(
            "{refusal}, so it was left unchanged. Add this to the config instead and \
             run the command again:\n\n{}",
            config::snippet(wanted)
        ),
    }
}

/// Apply the latest config in a running daemon, reporting a deferred change.
async fn rebind(paths: &Paths, report: &mut Report) -> Result<()> {
    match ipc::request(paths, &v1::Request::ShortcutRebind).await {
        Ok(v1::Reply::Ok) => Ok(()),
        Ok(v1::Reply::Error { message }) => {
            bail!("the daemon did not apply the change: {message}")
        }
        Ok(v1::Reply::ShortcutStatus(_) | v1::Reply::Accepted { .. }) => {
            bail!("the daemon answered something else")
        }
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => {
            report.notes.push(Note::DaemonNotRunning);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Read shortcut status, treating an absent daemon as having no live status.
pub async fn daemon_status(paths: &Paths) -> Result<Option<ShortcutStatus>> {
    match ipc::request(paths, &v1::Request::ShortcutStatus).await {
        Ok(v1::Reply::ShortcutStatus(status)) => Ok(Some(status)),
        Ok(v1::Reply::Error { message }) => bail!("the daemon could not tell: {message}"),
        Ok(v1::Reply::Ok | v1::Reply::Accepted { .. }) => {
            bail!("the daemon answered something else")
        }
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::{Note, Report, SetupError, check_config, configured, rebind};
    use crate::config::{Paths, ShortcutConfig};

    fn paths(config_file: PathBuf) -> Paths {
        Paths {
            socket: config_file.with_file_name("limen.sock"),
            log_file: config_file.with_file_name("daemon.log"),
            cache_dir: config_file.with_file_name("cache"),
            config_file,
        }
    }

    #[test]
    fn config_problems_map_to_their_error_kinds() {
        let dir = std::env::temp_dir().join(format!("limen-cli-{}", std::process::id()));
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
            Err(SetupError::Usage { message, .. }) if message.contains("shortcut.toggle: Shift+A")
        ));
        assert!(matches!(
            configured(&paths(dir.join("a-directory.toml"))),
            Err(SetupError::Runtime { .. })
        ));
        for absent in [dir.join("missing.toml"), dangling] {
            assert!(matches!(
                configured(&paths(absent)),
                Ok(shortcut) if shortcut == ShortcutConfig::default()
            ));
        }
    }

    #[tokio::test]
    async fn an_absent_daemon_produces_a_deferred_report() {
        let config = std::env::temp_dir().join(format!(
            "limen-offline-rebind-{}/config.toml",
            std::process::id()
        ));
        let mut report = Report::default();

        rebind(&paths(config), &mut report).await.unwrap();

        assert_eq!(
            report,
            Report {
                notes: vec![Note::DaemonNotRunning],
                ..Report::default()
            }
        );
    }

    #[test]
    fn a_dangling_config_is_refused_before_anything_changes() {
        let dir = std::env::temp_dir().join(format!("limen-cli-check-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dangling = dir.join("config.toml");
        symlink(dir.join("missing.toml"), &dangling).unwrap();
        let defaults = ShortcutConfig::default();

        assert!(check_config(&paths(dangling), &defaults).is_err());
        assert!(check_config(&paths(dir.join("new.toml")), &defaults).is_ok());
    }
}
