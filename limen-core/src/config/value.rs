//! Writing values back from the GUI: the ASR and polishing providers'
//! services, URLs, and models, whether polishing is on, and the proxy.
//!
//! Like the `[shortcut]` writer, only the values change: every other byte of
//! the file, comments included, stays as it was, and a symlinked or
//! read-only config is never replaced.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{DocumentMut, ImDocument, Item};

use super::keys::{self, KeyChange, Secret};
use super::transaction::{Backup, Transaction, recover};
use super::write::{check_writable, lock_dir, read_text, replace};
use super::{Config, ProviderOptions, Service};

/// A value of the config that the GUI edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    ProviderService,
    ProviderFormat,
    ProviderUrl,
    ProviderModel,
    PolishEnabled,
    PolishService,
    PolishFormat,
    ProviderKeyEnv,
    PolishKeyEnv,
    PolishUrl,
    PolishModel,
    /// The settings of one provider, a table of their own.
    ProviderOptions(Service),
    PolishOptions(Service),
    NetworkProxy,
    NetworkProxyUrl,
    NetworkProxyUsername,
    NetworkNoProxy,
}

/// A value to write: text or a switch, or no value, which removes the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Text(String),
    Switch(bool),
    Unset,
    /// A provider's settings; the defaults remove its table.
    Options(ProviderOptions),
}

impl Key {
    /// The section and the key within it.
    fn path(self) -> (&'static str, &'static str) {
        match self {
            Self::ProviderService => ("provider", "service"),
            Self::ProviderFormat => ("provider", "format"),
            Self::ProviderUrl => ("provider", "url"),
            Self::ProviderModel => ("provider", "model"),
            Self::PolishEnabled => ("polish", "enabled"),
            Self::PolishService => ("polish", "service"),
            Self::PolishFormat => ("polish", "format"),
            Self::ProviderKeyEnv => ("provider", "api_key_env"),
            Self::PolishKeyEnv => ("polish", "api_key_env"),
            Self::ProviderOptions(_) => ("provider", "options"),
            Self::PolishOptions(_) => ("polish", "options"),
            Self::PolishUrl => ("polish", "url"),
            Self::PolishModel => ("polish", "model"),
            Self::NetworkProxy => ("network", "proxy"),
            Self::NetworkProxyUrl => ("network", "proxy_url"),
            Self::NetworkProxyUsername => ("network", "proxy_username"),
            Self::NetworkNoProxy => ("network", "no_proxy"),
        }
    }

    /// The value as the config means it: a service or format left out reads
    /// as the one inferred, so saving that one again changes nothing.
    fn read(self, config: &Config) -> Value {
        match self {
            Self::ProviderService => Value::Text(config.provider.service().as_str().to_string()),
            Self::PolishService => Value::Text(config.polish.service().as_str().to_string()),
            Self::ProviderFormat => Value::Text(config.provider.format().as_str().to_string()),
            Self::PolishFormat => Value::Text(config.polish.format().as_str().to_string()),
            Self::ProviderUrl => Value::Text(config.provider.url.clone()),
            Self::ProviderModel => Value::Text(config.provider.model.clone()),
            Self::PolishEnabled => Value::Switch(config.polish.enabled),
            Self::PolishUrl => Value::Text(config.polish.url.clone()),
            Self::PolishModel => Value::Text(config.polish.model.clone()),
            Self::ProviderKeyEnv => config
                .provider
                .api_key_env
                .clone()
                .map_or(Value::Unset, Value::Text),
            Self::PolishKeyEnv => config
                .polish
                .api_key_env
                .clone()
                .map_or(Value::Unset, Value::Text),
            Self::ProviderOptions(service) => Value::Options(
                config
                    .provider
                    .options
                    .get(&service)
                    .cloned()
                    .unwrap_or_default(),
            ),
            Self::PolishOptions(service) => Value::Options(
                config
                    .polish
                    .options
                    .get(&service)
                    .cloned()
                    .unwrap_or_default(),
            ),
            Self::NetworkProxy => Value::Text(config.network.proxy.as_str().to_string()),
            Self::NetworkProxyUrl => Value::Text(config.network.proxy_url.clone()),
            Self::NetworkProxyUsername => config
                .network
                .proxy_username
                .clone()
                .map_or(Value::Unset, Value::Text),
            Self::NetworkNoProxy => Value::Text(config.network.no_proxy.clone()),
        }
    }
}

/// `text` with `key` set to `value`, every other byte unchanged; unsetting
/// it removes its line.
fn with_value(text: &str, key: Key, value: &Value) -> Result<String> {
    let (section, name) = key.path();
    let document = ImDocument::parse(text).context("the config is not valid TOML")?;
    let span = document
        .get(section)
        .and_then(|table| table.get(name))
        .and_then(Item::span);
    let replace = |span: std::ops::Range<usize>, written: toml::Value| {
        let mut edited = text.to_string();
        edited.replace_range(span, &written.to_string());
        edited
    };
    // Keys with a default, such as `polish.model`, may be left out.
    let insert = |item: Item| -> Result<String> {
        let mut document: DocumentMut = text.parse().context("the config is not valid TOML")?;
        document
            .entry(section)
            .or_insert(toml_edit::table())
            .as_table_like_mut()
            .with_context(|| format!("`{section}` in the config must be a [{section}] section"))?
            .insert(name, item);
        Ok(document.to_string())
    };
    match (span, value) {
        (_, Value::Options(options)) => match key {
            Key::ProviderOptions(service) | Key::PolishOptions(service) => {
                with_options(text, section, service, options)
            }
            Key::ProviderService
            | Key::ProviderFormat
            | Key::ProviderUrl
            | Key::ProviderModel
            | Key::PolishEnabled
            | Key::PolishService
            | Key::PolishFormat
            | Key::PolishUrl
            | Key::PolishModel
            | Key::ProviderKeyEnv
            | Key::PolishKeyEnv
            | Key::NetworkProxy
            | Key::NetworkProxyUrl
            | Key::NetworkProxyUsername
            | Key::NetworkNoProxy => bail!("{section}.{name} holds no provider settings"),
        },
        (Some(span), Value::Text(text)) => Ok(replace(span, toml::Value::String(text.clone()))),
        (Some(span), Value::Switch(on)) => Ok(replace(span, toml::Value::Boolean(*on))),
        (Some(_), Value::Unset) => {
            let mut document: DocumentMut = text.parse().context("the config is not valid TOML")?;
            if let Some(table) = document.get_mut(section).and_then(Item::as_table_like_mut) {
                table.remove(name);
            }
            Ok(document.to_string())
        }
        (None, Value::Text(text)) => insert(toml_edit::value(text.as_str())),
        (None, Value::Switch(on)) => insert(toml_edit::value(*on)),
        (None, Value::Unset) => Ok(text.to_string()),
    }
}

/// `text` with the table of `service`'s settings under `[section.options]`
/// set to `options`, or removed for the defaults, as is an options table
/// left empty.
fn with_options(
    text: &str,
    section: &str,
    service: Service,
    options: &ProviderOptions,
) -> Result<String> {
    let mut document: DocumentMut = text.parse().context("the config is not valid TOML")?;
    let table = document
        .entry(section)
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .with_context(|| format!("`{section}` in the config must be a [{section}] section"))?;
    match *options == ProviderOptions::default() {
        true => {
            if let Some(all) = table.get_mut("options").and_then(Item::as_table_like_mut) {
                all.remove(service.as_str());
                if all.is_empty() {
                    table.remove("options");
                }
            }
        }
        false => {
            let written: DocumentMut = toml::to_string(options)
                .context("the provider settings cannot be written")?
                .parse()
                .context("the provider settings cannot be written")?;
            let mut all = toml_edit::Table::new();
            all.set_implicit(true);
            table
                .entry("options")
                .or_insert(Item::Table(all))
                .as_table_like_mut()
                .with_context(|| format!("`{section}.options` in the config must be a table"))?
                .insert(service.as_str(), Item::Table(written.as_table().clone()));
        }
    }
    Ok(document.to_string())
}

/// Save a provider or the proxy: set each key to its value in the config
/// file at `path` and apply `change` to `secret`, under the same lock as
/// shortcut writes. Both files are edited and checked before either is
/// written, so a refused value or key leaves both as they were. A missing
/// config is created from the annotated default; a config whose values stay
/// the same is never written, so a managed one can still get a new key.
pub(crate) fn update(
    path: &Path,
    values: &[(Key, Value)],
    secret: Secret,
    change: &KeyChange,
) -> Result<()> {
    let _lock = lock_dir(path)?;
    recover(path)?;
    let (text, mode) = read_text(path)?;
    // Only the edited file must be valid, so a broken value can be repaired.
    let current: Config = toml::from_str(&text)
        .with_context(|| format!("failed to parse config: {}", path.display()))?;
    let unchanged = values
        .iter()
        .all(|(key, value)| key.read(&current) == *value);
    let config = match (unchanged, mode) {
        (true, Some(_)) => None,
        (false, _) | (true, None) => {
            check_writable(path).map_err(|refusal| anyhow!("{refusal}; edit it there instead"))?;
            let edited = values.iter().try_fold(text.clone(), |text, (key, value)| {
                with_value(&text, *key, value)
            })?;
            let config = Config::parse(&edited)?;
            if let Some((key, _)) = values
                .iter()
                .find(|(key, value)| key.read(&config) != *value)
            {
                let (section, name) = key.path();
                bail!("{section}.{name} of {} could not be edited", path.display());
            }
            Some(edited)
        }
    };
    let key = keys::edit_key(path, secret, change)?;
    if config.is_none() && key.is_none() {
        return Ok(());
    }
    let config_backup = config.as_ref().map(|_| match mode {
        Some(mode) => Backup::present(text.clone(), mode),
        None => Backup::missing(),
    });
    let key_backup = key.as_ref().map(|key| key.backup.clone());
    let transaction = Transaction::start(path, config_backup, key_backup)?;
    let written = match key {
        Some(key) => keys::write_key(&keys::path(path), key),
        None => Ok(()),
    };
    if let Err(error) = written {
        // write_key restored the key itself, and the config was not touched.
        return match transaction.finish() {
            Ok(()) => Err(error),
            Err(finish) => Err(error.context(format!(
                "removing the provider rollback also failed: {finish:#}"
            ))),
        };
    }
    let written = match config {
        Some(config) => replace(path, &config, mode),
        None => Ok(()),
    };
    match written {
        Ok(()) => transaction.finish(),
        Err(error) => match transaction.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(error.context(format!(
                "rolling back the provider save also failed: {rollback:#}"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{Key, Value, update};
    use crate::config::DEFAULT_CONFIG;
    use crate::config::Secret;
    use crate::config::{ApiKey, KeyChange};
    use crate::models::ModelRole;

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn save(path: &std::path::Path, values: &[(Key, Value)]) -> anyhow::Result<()> {
        update(path, values, Secret::Key(ModelRole::Asr), &KeyChange::Keep)
    }

    fn set(path: &std::path::Path, key: Key, text: &str) -> anyhow::Result<()> {
        save(path, &[(key, Value::Text(text.to_string()))])
    }

    /// The default config with the first `old` after `[section]` replaced,
    /// since both providers start with the same empty values.
    fn default_with(edits: &[(&str, &str, &str)]) -> String {
        edits
            .iter()
            .fold(DEFAULT_CONFIG.to_string(), |text, (section, old, new)| {
                let start = text.find(&format!("[{section}]")).unwrap();
                let at = start + text[start..].find(old).unwrap();
                format!("{}{new}{}", &text[..at], &text[at + old.len()..])
            })
    }

    #[test]
    fn a_whole_provider_is_written_at_once() {
        let path = scratch("provider-values").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        save(
            &path,
            &[
                (Key::PolishEnabled, Value::Switch(true)),
                (
                    Key::PolishUrl,
                    Value::Text("http://127.0.0.1:3425/v1/chat/completions".to_string()),
                ),
                (Key::PolishModel, Value::Text("qwen3".to_string())),
            ],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            default_with(&[
                ("polish", "enabled = false", "enabled = true"),
                (
                    "polish",
                    "url = \"\"",
                    "url = \"http://127.0.0.1:3425/v1/chat/completions\""
                ),
                ("polish", "model = \"\"", "model = \"qwen3\""),
            ])
        );
    }

    #[test]
    fn enabling_polishing_insecurely_leaves_the_file_alone() {
        let path = scratch("provider-insecure").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        let error = save(
            &path,
            &[
                (Key::PolishEnabled, Value::Switch(true)),
                (
                    Key::PolishUrl,
                    Value::Text("http://192.0.2.1/v1/chat/completions".to_string()),
                ),
            ],
        )
        .unwrap_err();

        assert_eq!(
            (error.to_string(), fs::read_to_string(&path).unwrap()),
            (
                "polish.url must use HTTPS, or HTTP on a loopback address".to_string(),
                DEFAULT_CONFIG.to_string()
            )
        );
    }

    #[test]
    fn a_refused_key_leaves_the_config_untouched() {
        let path = scratch("provider-key-refused").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        let error = update(
            &path,
            &[(Key::ProviderModel, Value::Text("new-model".to_string()))],
            Secret::Key(ModelRole::Asr),
            &KeyChange::Set(ApiKey::new("bad\r\nkey".to_string())),
        )
        .unwrap_err();

        assert_eq!(
            (
                error.to_string(),
                fs::read_to_string(&path).unwrap(),
                path.with_file_name("keys.toml").exists(),
            ),
            (
                "an API key cannot contain control characters".to_string(),
                DEFAULT_CONFIG.to_string(),
                false
            )
        );
    }

    #[test]
    fn only_the_url_value_changes() {
        let path = scratch("provider-edit").join("config.toml");
        fs::write(
            &path,
            "# mine\r\n[provider]\r\nurl = 'http://127.0.0.1:8000/v1'  # local\r\nmodel = \"m\"\r\n",
        )
        .unwrap();

        set(
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

        set(
            &path,
            Key::ProviderUrl,
            "http://10.0.0.2:9000/v1/audio/transcriptions",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            default_with(&[(
                "provider",
                "url = \"\"",
                "url = \"http://10.0.0.2:9000/v1/audio/transcriptions\""
            )])
        );
    }

    #[test]
    fn a_broken_url_can_be_repaired() {
        let path = scratch("provider-repair").join("config.toml");
        fs::write(&path, "[provider]\nurl = \"http://\"\nmodel = \"m\"\n").unwrap();

        set(&path, Key::ProviderUrl, "http://127.0.0.1:8000/asr").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[provider]\nurl = \"http://127.0.0.1:8000/asr\"\nmodel = \"m\"\n"
        );
    }

    #[test]
    fn an_invalid_url_leaves_the_file_alone() {
        let path = scratch("provider-invalid").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        for url in ["127.0.0.1:8000", "ftp://host/asr", "http://"] {
            assert!(set(&path, Key::ProviderUrl, url).is_err(), "{url}");
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn models_change_in_their_own_sections() {
        let path = scratch("model-edit").join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();

        set(&path, Key::ProviderModel, "Qwen/Qwen3-ASR-1.7B").unwrap();
        set(&path, Key::PolishModel, "corrector").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            default_with(&[
                (
                    "provider",
                    "model = \"\"",
                    "model = \"Qwen/Qwen3-ASR-1.7B\""
                ),
                ("polish", "model = \"\"", "model = \"corrector\""),
            ])
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

        set(&path, Key::PolishModel, "deepseek-chat").unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[provider]\nurl = \"http://127.0.0.1:8000/asr\"\nmodel = \"m\"\n\n[polish]\nmodel = \"deepseek-chat\"\n"
        );
    }
}
