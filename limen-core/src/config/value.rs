//! Writing single values back from the GUI: the ASR server's URL and the
//! ASR and polishing models.
//!
//! Like the `[shortcut]` writer, only the value changes: every other byte of
//! the file, comments included, stays as it was, and a symlinked or
//! read-only config is never replaced.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{DocumentMut, ImDocument, Item};

use super::Config;
use super::write::{check_writable, lock_dir, read_text, replace};

/// A string value of the config that the GUI edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    ProviderUrl,
    ProviderModel,
    PolishModel,
}

impl Key {
    /// The section and the key within it.
    fn path(self) -> (&'static str, &'static str) {
        match self {
            Self::ProviderUrl => ("provider", "url"),
            Self::ProviderModel => ("provider", "model"),
            Self::PolishModel => ("polish", "model"),
        }
    }

    fn read(self, config: &Config) -> &str {
        match self {
            Self::ProviderUrl => &config.provider.url,
            Self::ProviderModel => &config.provider.model,
            Self::PolishModel => &config.polish.model,
        }
    }
}

/// Set `key` to `value` in the config file at `path`, under the same lock as
/// shortcut writes. A missing file is created from the annotated default.
/// Nothing is written when the value stays the same, or when the edited file
/// would no longer be a valid config.
pub(crate) fn update_value(path: &Path, key: Key, value: &str) -> Result<()> {
    let (section, name) = key.path();
    let _lock = lock_dir(path)?;
    let (text, mode) = read_text(path)?;
    // Only the edited file must be valid, so a broken value can be repaired.
    let current: Config = toml::from_str(&text)
        .with_context(|| format!("failed to parse config: {}", path.display()))?;
    check_writable(path)
        .map_err(|refusal| anyhow!("{refusal}; set {section}.{name} = {value:?} there instead"))?;
    // A missing config is written even unchanged, like shortcut setup does.
    if key.read(&current) == value && mode.is_some() {
        return Ok(());
    }
    let document = ImDocument::parse(text.as_str()).context("the config is not valid TOML")?;
    let span = document
        .get(section)
        .and_then(|table| table.get(name))
        .and_then(Item::span);
    let edited = match span {
        Some(span) => {
            let mut edited = text.clone();
            edited.replace_range(span, &toml::Value::String(value.to_string()).to_string());
            edited
        }
        // Keys with a default, such as `polish.model`, may be left out.
        None => {
            let mut document: DocumentMut = text.parse().context("the config is not valid TOML")?;
            document
                .entry(section)
                .or_insert(toml_edit::table())
                .as_table_like_mut()
                .with_context(|| {
                    format!("`{section}` in the config must be a [{section}] section")
                })?
                .insert(name, toml_edit::value(value));
            document.to_string()
        }
    };
    let config = Config::parse(&edited)?;
    match key.read(&config) == value {
        true => replace(path, &edited, mode),
        false => bail!("{section}.{name} of {} could not be edited", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{Key, update_value};
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

        update_value(
            &path,
            Key::ProviderUrl,
            "https://asr.example/v1/audio/transcriptions",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# mine\r\n[provider]\r\nurl = \"https://asr.example/v1/audio/transcriptions\"  # local\r\nmodel = \"m\"\r\n"
        );
    }

    #[test]
    fn a_missing_config_starts_from_the_default() {
        let path = scratch("provider-missing").join("config.toml");

        update_value(&path, Key::ProviderUrl, "http://10.0.0.2:9000/asr").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            DEFAULT_CONFIG.replace(
                "http://127.0.0.1:8000/v1/audio/transcriptions",
                "http://10.0.0.2:9000/asr"
            )
        );
    }

    #[test]
    fn a_broken_url_can_be_repaired() {
        let path = scratch("provider-repair").join("config.toml");
        fs::write(&path, "[provider]\nurl = \"http://\"\nmodel = \"m\"\n").unwrap();

        update_value(&path, Key::ProviderUrl, "http://127.0.0.1:8000/asr").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[provider]\nurl = \"http://127.0.0.1:8000/asr\"\nmodel = \"m\"\n"
        );
    }

    #[test]
    fn an_invalid_url_leaves_the_file_alone() {
        let path = scratch("provider-invalid").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        for url in ["", "127.0.0.1:8000", "ftp://host/asr", "http://"] {
            assert!(update_value(&path, Key::ProviderUrl, url).is_err(), "{url}");
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn models_change_in_their_own_sections() {
        let path = scratch("model-edit").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        update_value(&path, Key::ProviderModel, "Qwen/Qwen3-ASR-1.7B").unwrap();
        update_value(&path, Key::PolishModel, "deepseek-chat").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            DEFAULT_CONFIG
                .replace("Qwen/Qwen3-ASR-0.6B", "Qwen/Qwen3-ASR-1.7B")
                .replace("deepseek-flash", "deepseek-chat")
        );
    }

    #[test]
    fn a_left_out_polish_model_is_added() {
        let path = scratch("model-missing").join("config.toml");
        fs::write(
            &path,
            "[provider]\nurl = \"http://127.0.0.1:8000/asr\"\nmodel = \"m\"\n",
        )
        .unwrap();

        update_value(&path, Key::PolishModel, "deepseek-chat").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[provider]\nurl = \"http://127.0.0.1:8000/asr\"\nmodel = \"m\"\n\n[polish]\nmodel = \"deepseek-chat\"\n"
        );
    }
}
