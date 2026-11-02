//! API keys saved from the GUI, in `keys.toml` beside the config file.
//!
//! Keys never go into the config itself: it may be managed declaratively,
//! readable by others or copied into the Nix store. This file is always
//! written with mode 0600, under the config directory's lock, and a
//! symlinked one is never replaced.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use toml_edit::DocumentMut;

use super::ApiKey;
use super::write::{check_writable, lock_dir, replace};
use crate::models::ModelRole;

/// How saving a provider changes its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyChange {
    Keep,
    Set(ApiKey),
    Clear,
}

/// The keys in the file, by the role that sends them.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedKeys {
    pub(crate) asr: Option<String>,
    pub(crate) polish: Option<String>,
}

fn path(config_file: &Path) -> PathBuf {
    config_file.with_file_name("keys.toml")
}

/// The keys saved beside the config file at `config_file`; none when there
/// is no key file.
pub(crate) fn saved_keys(config_file: &Path) -> Result<SavedKeys> {
    let path = path(config_file);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(SavedKeys::default()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Apply `change` to the key `role` sends, keeping the other key.
pub(crate) fn update_key(config_file: &Path, role: ModelRole, change: &KeyChange) -> Result<()> {
    let name = match role {
        ModelRole::Asr => "asr",
        ModelRole::Polish => "polish",
    };
    let path = path(config_file);
    let _lock = lock_dir(config_file)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let mut document: DocumentMut = text
        .parse()
        .with_context(|| format!("invalid {}", path.display()))?;
    match change {
        KeyChange::Keep => return Ok(()),
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
    replace(&path, &document.to_string(), Some(0o600))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    use super::{KeyChange, saved_keys, update_key};
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
