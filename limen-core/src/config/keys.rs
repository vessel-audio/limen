//! API keys saved from the GUI, in `keys.toml` beside the config file.
//!
//! Keys never go into the config itself: it may be managed declaratively,
//! readable by others or copied into the Nix store. This file is always
//! written with mode 0600, under the config directory's lock, and a
//! symlinked one is never replaced.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use toml_edit::DocumentMut;

use super::ApiKey;
use super::transaction::{Backup, write_contents};
use super::write::check_writable;
use crate::models::ModelRole;

/// How saving a provider changes its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyChange {
    Keep,
    Set(ApiKey),
    Clear,
}

/// The keys in the file, by the role that sends them.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedKeys {
    pub(crate) asr: Option<String>,
    pub(crate) polish: Option<String>,
}

pub(super) fn path(config_file: &Path) -> PathBuf {
    config_file.with_file_name("keys.toml")
}

/// The keys saved beside the config file at `config_file`; none when there
/// is no key file.
pub(crate) fn saved_keys(config_file: &Path) -> Result<SavedKeys> {
    let path = path(config_file);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|_| anyhow!("invalid {}; its keys were not read", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(SavedKeys::default()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// The inode a key file had when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn matches(self, metadata: &fs::Metadata) -> bool {
        self == Self::from_metadata(metadata)
    }
}

/// A key-file edit and the inode it was based on, checked again when the
/// original file is opened for writing.
pub(super) struct KeyEdit {
    pub(super) text: String,
    pub(super) original: Option<FileIdentity>,
    pub(super) backup: Backup,
}

/// The key file's contents after `change` to the key `role` sends, or
/// `None` when the file stays as it is. A regular file that others may read
/// is rewritten even when its keys stay, so it ends up private; a symlinked
/// one is left to whatever manages it, and refused when a key changes.
/// Callers hold the config directory's lock and write the result with mode
/// 0600 to [`path`].
pub(super) fn edit_key(
    config_file: &Path,
    role: ModelRole,
    change: &KeyChange,
) -> Result<Option<KeyEdit>> {
    let name = match role {
        ModelRole::Asr => "asr",
        ModelRole::Polish => "polish",
    };
    let path = path(config_file);
    let read = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path);
    let (read, managed_link) = match read {
        // A declaratively managed key file may be a symlink. It is readable,
        // but check_writable still refuses every change to it below.
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            (OpenOptions::new().read(true).open(&path), true)
        }
        result => (result, false),
    };
    let (text, exposed, original, backup) = match read {
        Ok(mut file) => {
            // Contents and identity come from the same descriptor, so a path
            // replacement cannot mix another inode's keys into this edit.
            let metadata = file.metadata()?;
            let mode = metadata.permissions().mode() & 0o777;
            let mut text = String::new();
            file.read_to_string(&mut text)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let exposed = !managed_link && metadata.is_file() && mode & 0o077 != 0;
            let original = Some(FileIdentity::from_metadata(&metadata));
            let backup = Backup::present(text.clone(), mode);
            (text, exposed, original, backup)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            (String::new(), false, None, Backup::missing())
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| anyhow!("invalid {}; its keys were not changed", path.display()))?;
    match change {
        KeyChange::Keep if !exposed => return Ok(None),
        KeyChange::Keep => {}
        KeyChange::Set(key) => {
            let key = key.expose().trim();
            if key.is_empty() {
                bail!("enter an API key");
            }
            // It goes into an HTTP header, where these would split the request.
            if key.chars().any(char::is_control) {
                bail!("an API key cannot contain control characters");
            }
            document.insert(name, toml_edit::value(key));
        }
        KeyChange::Clear => {
            document.remove(name);
        }
    }
    check_writable(&path).map_err(|refusal| anyhow!("{refusal}; the key was not saved"))?;
    Ok(Some(KeyEdit {
        text: document.to_string(),
        original,
        backup,
    }))
}

/// Write a prepared key file without following a symlink or replacing its
/// inode. Updating in place means a hard link added at any point sees the new
/// contents and the 0600 mode too, rather than retaining the old key. A file
/// that did not exist is created atomically with `create_new`, so another
/// process's keys are never overwritten. Callers hold the directory lock, so
/// Limen readers never observe the in-place write partway through.
pub(super) fn write_key(path: &Path, edit: KeyEdit) -> Result<()> {
    let KeyEdit {
        text,
        original,
        backup,
    } = edit;
    let mut options = OpenOptions::new();
    options.write(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = match original {
        Some(expected) => {
            let file = options
                .open(path)
                .with_context(|| format!("failed to open {} for writing", path.display()))?;
            if !expected.matches(&file.metadata()?) {
                bail!("{} changed while it was being written", path.display());
            }
            file
        }
        None => options
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?,
    };
    match write_contents(&mut file, path, &text, 0o600) {
        Ok(()) => Ok(()),
        Err(error) => match backup.restore(path) {
            Ok(()) => Err(error),
            Err(rollback) => Err(error.context(format!(
                "rolling back the key file also failed: {rollback:#}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    use super::{KeyChange, edit_key, path as key_path, saved_keys, write_key};
    use crate::config::ApiKey;
    use crate::models::ModelRole;

    /// The config file path in a fresh, empty directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    fn set(key: &str) -> KeyChange {
        KeyChange::Set(ApiKey::new(key.to_string()))
    }

    fn update_key(
        config: &std::path::Path,
        role: ModelRole,
        change: &KeyChange,
    ) -> anyhow::Result<()> {
        match edit_key(config, role, change)? {
            Some(edit) => write_key(&key_path(config), edit),
            None => Ok(()),
        }
    }

    #[test]
    fn an_exposed_key_file_is_made_private_even_when_its_keys_stay() {
        let config = scratch("keys-permissions");
        let path = config.with_file_name("keys.toml");
        fs::write(&path, "asr = \"sk-asr\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        update_key(&config, ModelRole::Asr, &KeyChange::Keep).unwrap();

        assert_eq!(
            (
                fs::read_to_string(&path).unwrap(),
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            ),
            ("asr = \"sk-asr\"\n".to_string(), 0o600)
        );
    }

    #[test]
    fn a_hard_linked_exposed_key_file_is_made_private_in_every_copy() {
        let config = scratch("keys-hard-link");
        let path = config.with_file_name("keys.toml");
        let other = config.with_file_name("other-copy.toml");
        fs::write(&path, "asr = \"sk-asr\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&path, &other).unwrap();

        update_key(&config, ModelRole::Asr, &KeyChange::Keep).unwrap();

        assert_eq!(
            (
                fs::read_to_string(&path).unwrap(),
                fs::read_to_string(&other).unwrap(),
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                fs::metadata(&other).unwrap().permissions().mode() & 0o777,
            ),
            (
                "asr = \"sk-asr\"\n".to_string(),
                "asr = \"sk-asr\"\n".to_string(),
                0o600,
                0o600
            )
        );
    }

    #[test]
    fn a_hard_link_added_before_writing_gets_the_new_key_too() {
        let config = scratch("keys-hard-link-race");
        let path = config.with_file_name("keys.toml");
        let other = config.with_file_name("other-copy.toml");
        fs::write(&path, "asr = \"old\"\n").unwrap();
        let edit = edit_key(&config, ModelRole::Asr, &set("new"))
            .unwrap()
            .unwrap();
        fs::hard_link(&path, &other).unwrap();

        write_key(&path, edit).unwrap();

        assert_eq!(
            (
                fs::read_to_string(&path).unwrap(),
                fs::read_to_string(&other).unwrap(),
            ),
            ("asr = \"new\"\n".to_string(), "asr = \"new\"\n".to_string())
        );
    }

    #[test]
    fn a_key_file_created_after_reading_is_never_overwritten() {
        let config = scratch("keys-create-race");
        let path = config.with_file_name("keys.toml");
        let edit = edit_key(&config, ModelRole::Asr, &set("new"))
            .unwrap()
            .unwrap();
        fs::write(&path, "polish = \"other\"\n").unwrap();

        assert!(write_key(&path, edit).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "polish = \"other\"\n");
    }

    #[test]
    fn a_managed_key_file_is_left_alone_when_its_keys_stay() {
        let config = scratch("keys-managed-keep");
        let target = config.with_file_name("managed.toml");
        fs::write(&target, "asr = \"managed\"\n").unwrap();
        symlink(&target, config.with_file_name("keys.toml")).unwrap();

        update_key(&config, ModelRole::Asr, &KeyChange::Keep).unwrap();

        assert_eq!(
            (
                fs::read_to_string(&target).unwrap(),
                fs::read_link(config.with_file_name("keys.toml")).unwrap(),
            ),
            ("asr = \"managed\"\n".to_string(), target)
        );
    }

    #[test]
    fn keys_are_saved_privately_beside_the_config() {
        let config = scratch("keys-save");
        fs::write(config.with_file_name("keys.toml"), "polish = \"old\"\n").unwrap();
        fs::set_permissions(
            config.with_file_name("keys.toml"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        update_key(&config, ModelRole::Asr, &set(" sk-asr ")).unwrap();
        update_key(&config, ModelRole::Polish, &set("sk-polish")).unwrap();

        let path = config.with_file_name("keys.toml");
        assert_eq!(
            (
                fs::read_to_string(&path).unwrap(),
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                config.exists(),
            ),
            (
                "polish = \"sk-polish\"\nasr = \"sk-asr\"\n".to_string(),
                0o600,
                false
            )
        );
    }

    #[test]
    fn a_cleared_key_leaves_the_other() {
        let config = scratch("keys-clear");
        update_key(&config, ModelRole::Asr, &set("a")).unwrap();
        update_key(&config, ModelRole::Polish, &set("p")).unwrap();

        update_key(&config, ModelRole::Asr, &KeyChange::Clear).unwrap();

        let keys = saved_keys(&config).unwrap();
        assert_eq!((keys.asr, keys.polish), (None, Some("p".to_string())));
    }

    #[test]
    fn a_blank_or_multiline_key_is_refused() {
        let config = scratch("keys-invalid");
        for key in ["  ", "sk\r\nX-Injected: 1"] {
            assert!(
                update_key(&config, ModelRole::Asr, &set(key)).is_err(),
                "{key:?}"
            );
        }
        assert!(!config.with_file_name("keys.toml").exists());
    }

    #[test]
    fn a_symlinked_key_file_is_never_replaced() {
        let config = scratch("keys-symlink");
        let target = config.with_file_name("managed.toml");
        fs::write(&target, "asr = \"managed\"\n").unwrap();
        symlink(&target, config.with_file_name("keys.toml")).unwrap();

        assert!(update_key(&config, ModelRole::Asr, &set("new")).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "asr = \"managed\"\n");
    }

    #[test]
    fn keys_reach_the_loaded_config() {
        let config = scratch("keys-load");
        fs::write(
            &config,
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n",
        )
        .unwrap();
        update_key(&config, ModelRole::Asr, &set("sk-asr")).unwrap();

        let loaded = crate::config::Config::load(&config).unwrap();

        assert_eq!(
            (loaded.api_key(), loaded.polish.saved_key),
            (Some("sk-asr".to_string()), None)
        );
    }
}
