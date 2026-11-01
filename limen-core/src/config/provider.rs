//! Writing `provider.url` back from the GUI.
//!
//! Like the `[shortcut]` writer, only the value changes: every other byte of
//! the file, comments included, stays as it was, and a symlinked or
//! read-only config is never replaced.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{ImDocument, Item};

use super::Config;
use super::write::{check_writable, lock_dir, read_current, replace};

/// Set `provider.url` in the config file at `path`, under the same lock as
/// shortcut writes. A missing file is created from the annotated default.
/// Nothing is written when the URL stays the same, or when the edited file
/// would no longer be a valid config.
pub(crate) fn update_provider_url(path: &Path, url: &str) -> Result<()> {
    let _lock = lock_dir(path)?;
    let (text, mode, current) = read_current(path)?;
    check_writable(path)
        .map_err(|refusal| anyhow!("{refusal}; set provider.url = {url:?} there instead"))?;
    // A missing config is written even unchanged, like shortcut setup does.
    if current.provider.url == url && mode.is_some() {
        return Ok(());
    }
    let document = ImDocument::parse(text.as_str()).context("the config is not valid TOML")?;
    let span = document
        .get("provider")
        .and_then(|provider| provider.get("url"))
        .and_then(Item::span)
        .context("the config has no provider.url")?;
    let mut edited = text.clone();
    edited.replace_range(span, &toml::Value::String(url.to_string()).to_string());
    let config = Config::parse(&edited)?;
    match config.provider.url == url {
        true => replace(path, &edited, mode),
        false => bail!("provider.url of {} could not be edited", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::update_provider_url;
    use crate::config::DEFAULT_CONFIG;

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_the_url_value_changes() {
        let path = scratch("provider-edit").join("config.toml");
        fs::write(
            &path,
            "# mine\r\n[provider]\r\nurl = 'http://127.0.0.1:8000/v1'  # local\r\nmodel = \"m\"\r\n",
        )
        .unwrap();

        update_provider_url(&path, "https://asr.example/v1/audio/transcriptions").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# mine\r\n[provider]\r\nurl = \"https://asr.example/v1/audio/transcriptions\"  # local\r\nmodel = \"m\"\r\n"
        );
    }

    #[test]
    fn a_missing_config_starts_from_the_default() {
        let path = scratch("provider-missing").join("config.toml");

        update_provider_url(&path, "http://10.0.0.2:9000/asr").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            DEFAULT_CONFIG.replace(
                "http://127.0.0.1:8000/v1/audio/transcriptions",
                "http://10.0.0.2:9000/asr"
            )
        );
    }

    #[test]
    fn an_invalid_url_leaves_the_file_alone() {
        let path = scratch("provider-invalid").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        for url in ["", "127.0.0.1:8000", "ftp://host/asr", "http://"] {
            assert!(update_provider_url(&path, url).is_err(), "{url}");
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
    }
}
