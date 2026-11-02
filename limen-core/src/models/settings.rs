//! A provider's settings as the GUI edits them: its service, URL, model,
//! and key, and for polishing whether it is on.

use anyhow::{Context, Result, bail};

use super::ModelRole;
use crate::config::{self, Config, KeyChange, Paths, Service};
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
    /// The service that transcribes, its server's transcription URL, and
    /// its model.
    Asr {
        service: Service,
        url: String,
        model: String,
    },
    /// The service that polishes transcripts, its server's
    /// chat-completions URL, and its model. The URL of a vendor's own API
    /// is kept but unused.
    Polish {
        polishing: Polishing,
        service: Service,
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
    // Read the config and its keys as one snapshot relative to GUI saves.
    let _lock = config::lock_dir(&paths.config_file)?;
    config::recover_provider_save(&paths.config_file)?;
    let config = Config::read_unchecked(&paths.config_file)?;
    let keys = config::saved_keys(&paths.config_file)?;
    let (provider, saved, variable) = match role {
        ModelRole::Asr => (
            Provider::Asr {
                service: config.provider.service(),
                url: config.provider.url.clone(),
                model: config.provider.model.clone(),
            },
            keys.asr,
            config.provider.key_env().map(str::to_string),
        ),
        ModelRole::Polish => (
            Provider::Polish {
                polishing: match config.polish.enabled {
                    true => Polishing::On,
                    false => Polishing::Off,
                },
                service: config.polish.service(),
                url: config.polish.url.clone(),
                model: config.polish.model.clone(),
            },
            keys.polish,
            config.polish.key_env().map(str::to_string),
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
        Provider::Polish {
            polishing: Polishing::On,
            service,
            url,
            ..
        } if service.uses_url() => config::check_polish_url(url.trim())?,
        Provider::Polish {
            polishing: Polishing::On,
            ..
        } => {}
        Provider::Polish {
            polishing: Polishing::Off,
            url,
            ..
        } => {
            // A broken endpoint must not prevent turning polishing off, but
            // a parsable one still may not persist credentials.
            if let Ok(url) = reqwest::Url::parse(url.trim()) {
                config::check_url_parameters(&url, "polish.url")?;
            }
        }
    }
    let blank = |text: &str| text.trim().is_empty();
    match &provider {
        Provider::Asr { model, .. }
        | Provider::Polish {
            polishing: Polishing::On,
            model,
            ..
        } if blank(model) => bail!("enter a model name"),
        Provider::Asr { .. } | Provider::Polish { .. } => {}
    }
    let text = |text: String| config::Value::Text(text.trim().to_string());
    let service = |service: Service| config::Value::Text(service.as_str().to_string());
    let (role, values, request) = match provider {
        Provider::Asr {
            service: chosen,
            url,
            model,
        } => (
            ModelRole::Asr,
            [
                Some((config::Key::ProviderService, service(chosen))),
                // A vendor's own API leaves the server's URL as it was.
                chosen
                    .uses_url()
                    .then(|| (config::Key::ProviderUrl, text(url))),
                Some((config::Key::ProviderModel, text(model))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
            v1::Request::ProviderReload,
        ),
        Provider::Polish {
            polishing,
            service: chosen,
            url,
            model,
        } => (
            ModelRole::Polish,
            [
                Some((
                    config::Key::PolishEnabled,
                    config::Value::Switch(polishing == Polishing::On),
                )),
                Some((config::Key::PolishService, service(chosen))),
                chosen
                    .uses_url()
                    .then(|| (config::Key::PolishUrl, text(url))),
                Some((config::Key::PolishModel, text(model))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
            v1::Request::PolishReload,
        ),
    };
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
    use crate::config::{ApiKey, Config, KeyChange, Paths, Service};
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
            service: Service::OpenAiCompatible,
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
                        service: Service::OpenAiCompatible,
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
            service: Service::WhisperCpp,
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

    #[tokio::test]
    async fn a_broken_endpoint_does_not_prevent_disabling_polishing() {
        let paths = scratch("provider-disable");
        let provider = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::OpenAiCompatible,
            url: "http://".to_string(),
            model: "m".to_string(),
        };

        let change = save_provider(&paths, provider.clone(), KeyChange::Keep)
            .await
            .unwrap();

        assert_eq!(
            (change, load_provider(&paths, ModelRole::Polish).unwrap()),
            (ConfigChange::Saved, (provider, KeyState::Missing))
        );
    }

    #[tokio::test]
    async fn a_vendor_keeps_the_server_url_and_polishing_off_needs_no_model() {
        let paths = scratch("provider-vendor");
        let polish = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Anthropic,
            url: "https://ignored.example/v1/chat/completions".to_string(),
            model: String::new(),
        };

        save_provider(&paths, polish, KeyChange::Keep)
            .await
            .unwrap();

        // The key state depends on ANTHROPIC_API_KEY in the environment.
        assert_eq!(
            (
                fs::read_to_string(&paths.config_file).unwrap(),
                load_provider(&paths, ModelRole::Polish).unwrap().0
            ),
            (
                format!(
                    "{CONFIG}\n[polish]\nenabled = false\nservice = \"anthropic\"\nmodel = \"\"\n"
                ),
                Provider::Polish {
                    polishing: Polishing::Off,
                    service: Service::Anthropic,
                    url: String::new(),
                    model: String::new(),
                }
            )
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
                service: Service::WhisperCpp,
                url: "http://".to_string(),
                model: "m".to_string()
            }
        );
    }
}
