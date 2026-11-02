//! A provider's settings as the GUI edits them: its URL, model, and key,
//! and for polishing whether it is on.

use anyhow::{Context, Result, bail};

use super::ModelRole;
use crate::config::{self, Config, KeyChange, Paths};
use crate::ipc::v1;
use crate::reload::{ConfigChange, reload};

/// Whether transcripts are polished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polishing {
    Off,
    On,
}

/// A model provider as saved in the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// The transcription URL of the ASR server and its model.
    Asr { url: String, model: String },
    /// The chat-completions URL that polishes transcripts and its model.
    Polish {
        polishing: Polishing,
        url: String,
        model: String,
    },
}

/// Where a provider's API key comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyState {
    /// Saved from the GUI.
    Saved,
    /// Read from this environment variable, which holds one.
    Environment(String),
    /// Nowhere; a local server may not need one.
    Missing,
}

/// The settings of the provider for `role` as saved, read without
/// validating the config so that a broken one can be shown and repaired. A
/// missing config reads as the annotated default.
pub fn load_provider(paths: &Paths, role: ModelRole) -> Result<(Provider, KeyState)> {
    let config = Config::read_unchecked(&paths.config_file)?;
    let keys = config::saved_keys(&paths.config_file)?;
    let (provider, saved, variable) = match role {
        ModelRole::Asr => (
            Provider::Asr {
                url: config.provider.url,
                model: config.provider.model,
            },
            keys.asr,
            config.provider.api_key_env,
        ),
        ModelRole::Polish => (
            Provider::Polish {
                polishing: match config.polish.enabled {
                    true => Polishing::On,
                    false => Polishing::Off,
                },
                url: config.polish.url,
                model: config.polish.model,
            },
            keys.polish,
            Some(config.polish.api_key_env),
        ),
    };
    let set = |variable: &String| std::env::var(variable).is_ok_and(|key| !key.is_empty());
    let key = match (saved, variable.filter(set)) {
        (Some(_), _) => KeyState::Saved,
        (None, Some(variable)) => KeyState::Environment(variable),
        (None, None) => KeyState::Missing,
    };
    Ok((provider, key))
}

/// Save `provider` and the `key` change, then have a running daemon apply
/// them. Both files are edited and checked under one lock before either is
/// written, so a refused value or key changes neither.
pub async fn save_provider(
    paths: &Paths,
    provider: Provider,
    key: KeyChange,
) -> Result<ConfigChange> {
    match &provider {
        Provider::Asr { .. } => {}
        // The key may be listed even while polishing is off, so a provider
        // saved through the GUI always gets the polisher's URL checks.
        Provider::Polish { url, .. } => config::check_polish_url(url.trim())?,
    }
    let text = |text: String| config::Value::Text(text.trim().to_string());
    let (role, values, request) = match provider {
        Provider::Asr { url, model } => (
            ModelRole::Asr,
            vec![
                (config::Key::ProviderUrl, text(url)),
                (config::Key::ProviderModel, text(model)),
            ],
            v1::Request::ProviderReload,
        ),
        Provider::Polish {
            polishing,
            url,
            model,
        } => (
            ModelRole::Polish,
            vec![
                (
                    config::Key::PolishEnabled,
                    config::Value::Switch(polishing == Polishing::On),
                ),
                (config::Key::PolishUrl, text(url)),
                (config::Key::PolishModel, text(model)),
            ],
            v1::Request::PolishReload,
        ),
    };
    if values.contains(&(config::Key::ProviderModel, text(String::new())))
        || values.contains(&(config::Key::PolishModel, text(String::new())))
    {
        bail!("enter a model name");
    }
    let file = paths.config_file.clone();
    // The writes wait on a file lock; keep them off the caller's runtime thread.
    tokio::task::spawn_blocking(move || config::update_provider(&file, &values, role, &key))
        .await
        .context("the config writer stopped")??;
    reload(paths, &request).await
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{KeyState, Polishing, Provider, load_provider, save_provider};
    use crate::config::{ApiKey, Config, KeyChange, Paths};
    use crate::models::ModelRole;
    use crate::reload::ConfigChange;

    const CONFIG: &str =
        "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n";

    /// Paths in a fresh directory holding `CONFIG`, where no daemon listens.
    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), CONFIG).unwrap();
        Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        }
    }

    #[tokio::test]
    async fn a_saved_provider_loads_back_with_its_key() {
        let paths = scratch("provider-save");
        let polish = Provider::Polish {
            polishing: Polishing::On,
            url: " http://127.0.0.1:3425/v1/chat/completions ".to_string(),
            model: "qwen3".to_string(),
        };

        let change = save_provider(
            &paths,
            polish,
            KeyChange::Set(ApiKey::new("sk-polish".to_string())),
        )
        .await
        .unwrap();

        assert_eq!(
            (
                change,
                load_provider(&paths, ModelRole::Polish).unwrap(),
                Config::load(&paths.config_file).unwrap().polish.api_key(),
            ),
            (
                ConfigChange::Saved,
                (
                    Provider::Polish {
                        polishing: Polishing::On,
                        url: "http://127.0.0.1:3425/v1/chat/completions".to_string(),
                        model: "qwen3".to_string(),
                    },
                    KeyState::Saved
                ),
                Some("sk-polish".to_string()),
            )
        );
    }

    #[tokio::test]
    async fn a_blank_model_is_refused_before_anything_is_written() {
        let paths = scratch("provider-blank");
        let asr = Provider::Asr {
            url: "http://127.0.0.1:8000/inference".to_string(),
            model: "  ".to_string(),
        };

        assert!(
            save_provider(&paths, asr, KeyChange::Set(ApiKey::new("sk".to_string())))
                .await
                .is_err()
        );
        assert_eq!(
            (
                fs::read_to_string(&paths.config_file).unwrap(),
                load_provider(&paths, ModelRole::Asr).unwrap().1
            ),
            (CONFIG.to_string(), KeyState::Missing)
        );
    }

    #[test]
    fn a_broken_config_still_loads_for_repair() {
        let paths = scratch("provider-broken");
        fs::write(
            &paths.config_file,
            "[provider]\nurl = \"http://\"\nmodel = \"m\"\n",
        )
        .unwrap();

        assert_eq!(
            load_provider(&paths, ModelRole::Asr).unwrap().0,
            Provider::Asr {
                url: "http://".to_string(),
                model: "m".to_string()
            }
        );
    }
}
