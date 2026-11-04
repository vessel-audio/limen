//! Rollback for a provider save, which changes the config and its key file.
//!
//! The journal is written privately and durably before either file changes.
//! Ordinary errors roll back immediately; after a crash, the next locked
//! provider read restores the original pair before using it.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::keys;
use super::write::replace;

/// The original contents and permissions of a file, or that it was absent.
/// No `Debug`: key-file backups contain API keys.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Backup {
    present: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    mode: u32,
}

impl Backup {
    pub(super) fn missing() -> Self {
        Self {
            present: false,
            text: String::new(),
            mode: 0,
        }
    }

    pub(super) fn present(text: String, mode: u32) -> Self {
        Self {
            present: true,
            text,
            mode,
        }
    }

    /// Restore this backup without following a symlink. Restoring a missing
    /// original removes the new path, not anything it may point to.
    pub(super) fn restore(&self, path: &Path) -> Result<()> {
        match self.present {
            false => match fs::remove_file(path) {
                Ok(()) => sync_parent(path),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("failed to remove {}", path.display()))
                }
            },
            true => {
                let mut options = OpenOptions::new();
                options.write(true).custom_flags(libc::O_NOFOLLOW);
                let mut file = match options.open(path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == ErrorKind::NotFound => options
                        .create_new(true)
                        .mode(self.mode)
                        .open(path)
                        .with_context(|| format!("failed to recreate {}", path.display()))?,
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("failed to open {}", path.display()));
                    }
                };
                write_contents(&mut file, path, &self.text, self.mode)
            }
        }
    }
}

/// Replace a file's contents and mode through an already opened descriptor,
/// then make them and the directory entry durable. Readers hold the config
/// directory lock, so they cannot observe the intermediate contents.
pub(super) fn write_contents(file: &mut File, path: &Path, text: &str, mode: u32) -> Result<()> {
    (|| -> Result<()> {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(text.as_bytes())?;
        // Remove any old suffix only after the new contents are written.
        file.set_len(text.len() as u64)?;
        file.sync_all()?;
        sync_parent(path)
    })()
    .with_context(|| format!("failed to write {}", path.display()))
}

fn sync_parent(path: &Path) -> Result<()> {
    let dir = path.parent().context("file path has no parent directory")?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<Backup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keys: Option<Backup>,
}

fn journal_path(config_file: &Path) -> PathBuf {
    config_file.with_file_name(".provider-rollback.toml")
}

fn remove_journal(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// A journal on disk. Dropping it without finishing or rolling back leaves
/// the journal for the next provider read to recover.
pub(crate) struct Transaction {
    config_file: PathBuf,
}

impl Transaction {
    /// Persist the originals before changing either file. `None` marks a file
    /// that the save does not change.
    pub(crate) fn start(
        config_file: &Path,
        config: Option<Backup>,
        keys: Option<Backup>,
    ) -> Result<Self> {
        let journal = Journal { config, keys };
        let text = toml::to_string(&journal).context("failed to encode the provider rollback")?;
        replace(&journal_path(config_file), &text, Some(0o600))
            .context("failed to start the provider save")?;
        Ok(Self {
            config_file: config_file.to_path_buf(),
        })
    }

    /// The save completed; its originals are no longer needed.
    pub(crate) fn finish(self) -> Result<()> {
        remove_journal(&journal_path(&self.config_file))
    }

    /// Restore both originals now.
    pub(crate) fn rollback(self) -> Result<()> {
        recover(&self.config_file)
    }
}

/// Recover an interrupted save. The caller holds the config directory lock,
/// so no Limen reader can use the mixed pair before this returns.
pub(crate) fn recover(config_file: &Path) -> Result<()> {
    let journal_path = journal_path(config_file);
    let text = match fs::read_to_string(&journal_path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", journal_path.display()));
        }
    };
    let journal: Journal = toml::from_str(&text)
        .map_err(|_| anyhow!("invalid provider rollback {}", journal_path.display()))?;
    if let Some(backup) = journal.keys {
        backup.restore(&keys::path(config_file))?;
    }
    if let Some(backup) = journal.config {
        backup.restore(config_file)?;
    }
    remove_journal(&journal_path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::{Backup, Transaction, journal_path, recover};
    use crate::config::keys;
    use crate::config::{ApiKey, KeyChange};
    use crate::models::ModelRole;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn an_interrupted_save_restores_the_original_pair_before_the_next_read() {
        let config = scratch("provider-recover");
        let keys_path = keys::path(&config);
        let original_config =
            "[provider]\nurl = \"http://old/v1/audio/transcriptions\"\nmodel = \"old\"\n";
        fs::write(&config, original_config).unwrap();
        fs::write(&keys_path, "asr = \"old\"\npolish = \"keep\"\n").unwrap();
        let _lock = super::super::write::lock_dir(&config).unwrap();
        let transaction = Transaction::start(
            &config,
            Some(Backup::present(original_config.to_string(), 0o644)),
            Some(Backup::present(
                "asr = \"old\"\npolish = \"keep\"\n".to_string(),
                0o600,
            )),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(journal_path(&config))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let edit = keys::edit_key(
            &config,
            keys::Secret::Key(ModelRole::Asr),
            &KeyChange::Set(ApiKey::new("new".to_string())),
        )
        .unwrap()
        .unwrap();
        keys::write_key(&keys_path, edit).unwrap();
        fs::write(
            &config,
            "[provider]\nurl = \"http://new/v1\"\nmodel = \"new\"\n",
        )
        .unwrap();
        // A crash before finish leaves both the new pair and the journal.
        drop(transaction);
        assert_eq!(
            fs::read_to_string(&keys_path).unwrap(),
            "asr = \"new\"\npolish = \"keep\"\n"
        );

        recover(&config).unwrap();

        assert_eq!(
            (
                fs::read_to_string(&config).unwrap(),
                fs::read_to_string(&keys_path).unwrap(),
                journal_path(&config).exists(),
                fs::metadata(&keys_path).unwrap().permissions().mode() & 0o777,
            ),
            (
                original_config.to_string(),
                "asr = \"old\"\npolish = \"keep\"\n".to_string(),
                false,
                0o600
            )
        );
    }

    #[test]
    fn recovery_removes_a_key_file_created_by_an_interrupted_save() {
        let config = scratch("provider-recover-create");
        let keys_path = keys::path(&config);
        let _lock = super::super::write::lock_dir(&config).unwrap();
        let transaction = Transaction::start(&config, None, Some(Backup::missing())).unwrap();
        fs::write(&keys_path, "asr = \"new\"\n").unwrap();
        drop(transaction);

        recover(&config).unwrap();

        assert_eq!(
            (keys_path.exists(), journal_path(&config).exists(),),
            (false, false)
        );
    }
}
