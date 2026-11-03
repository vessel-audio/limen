//! A provider's settings as the GUI edits them: its service, URL, model,
//! and key, and for polishing whether it is on.

use anyhow::{Context, Result, bail};

use super::ModelRole;
use crate::config::{self, Config, Format, KeyChange, Paths, Service};
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
    /// The service that transcribes, and its model. A custom server also
    /// has the format it speaks and its URL, which a vendor's own API keeps
    /// but does not use.
    Asr {
        service: Service,
        format: Format,
        url: String,
        model: String,
    },
    /// The service that polishes transcripts, and its model, with a custom
    /// server's format and URL as for [`Provider::Asr`].
    Polish {
        polishing: Polishing,
        service: Service,
        format: Format,
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

/// Where a provider's key may come from, whichever service the page
/// chooses: the key saved for its role, `api_key_env`, or the service's own
/// variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySource {
    saved: bool,
    /// `api_key_env` as the config names it.
    variable: Option<String>,
    /// The saved service and the variable the config reads for it, which
    /// for a config from before services may be neither of the above.
    loaded: (Service, Option<String>),
}

impl KeySource {
    /// Whether a key is saved for the role.
    pub fn is_saved(&self) -> bool {
        self.saved
    }

    /// Where the key comes from once `service` is chosen.
    pub fn state(&self, service: Service) -> KeyState {
        let variable = match &self.loaded {
            (loaded, variable) if *loaded == service => variable.clone(),
            _ => self
                .variable
                .clone()
                .or_else(|| service.key_env().map(str::to_string)),
        };
        let set = |variable: &String| std::env::var(variable).is_ok_and(|key| !key.is_empty());
        match (self.saved, variable.filter(set)) {
            (true, _) => KeyState::Saved,
            (false, Some(variable)) => KeyState::Environment(variable),
            (false, None) => KeyState::Missing,
        }
    }
}

/// The settings of the provider for `role` as saved, read without
/// validating the config so that a broken one can be shown and repaired. A
/// missing config reads as the annotated default.
pub fn load_provider(paths: &Paths, role: ModelRole) -> Result<(Provider, KeySource)> {
    // Read the config and its keys as one snapshot relative to GUI saves.
    let _lock = config::lock_dir(&paths.config_file)?;
    config::recover_provider_save(&paths.config_file)?;
    let config = Config::read_unchecked(&paths.config_file)?;
    let keys = config::saved_keys(&paths.config_file)?;
    let (provider, saved, variable, resolved) = match role {
        ModelRole::Asr => (
            Provider::Asr {
                service: config.provider.service(),
                format: config.provider.format(),
                url: config.provider.url.clone(),
                model: config.provider.model.clone(),
            },
            keys.asr,
            config.provider.api_key_env.clone(),
            (
                config.provider.service(),
                config.provider.key_env().map(str::to_string),
            ),
        ),
        ModelRole::Polish => (
            Provider::Polish {
                polishing: match config.polish.enabled {
                    true => Polishing::On,
                    false => Polishing::Off,
                },
                service: config.polish.service(),
                format: config.polish.format(),
                // A vendor inferred from its own endpoint, the old default,
                // has no server of the user's to offer when they choose one.
                url: match (config.polish.service, config.polish.service().uses_url()) {
                    (None, false) => String::new(),
                    (Some(_), _) | (None, true) => config.polish.url.clone(),
                },
                model: config.polish.model.clone(),
            },
            keys.polish,
            config.polish.api_key_env.clone(),
            (
                config.polish.service(),
                config.polish.key_env().map(str::to_string),
            ),
        ),
    };
    Ok((
        provider,
        KeySource {
            saved: saved.is_some(),
            variable,
            loaded: resolved,
        },
    ))
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
            format,
            url,
            ..
        } if service.uses_url() => {
            if url.trim().is_empty() {
                bail!("enter the server's URL");
            }
            config::check_polish_url(url.trim())?;
            config::check_polish_root(url.trim(), *format)?;
        }
        Provider::Polish {
            polishing: Polishing::On,
            ..
        } => {}
        // A vendor's own API neither uses nor writes the URL.
        Provider::Polish {
            polishing: Polishing::Off,
            service,
            ..
        } if !service.uses_url() => {}
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
    let format = |format: Format| config::Value::Text(format.as_str().to_string());
    let (role, values, request) = match provider {
        Provider::Asr {
            service: chosen,
            format: spoken,
            url,
            model,
        } => (
            ModelRole::Asr,
            [
                Some((config::Key::ProviderService, service(chosen))),
                // A vendor's own API leaves the custom server as it was.
                chosen
                    .uses_url()
                    .then(|| (config::Key::ProviderFormat, format(spoken))),
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
            format: spoken,
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
                    .then(|| (config::Key::PolishFormat, format(spoken))),
                // A vendor's own API leaves a server's URL for switching
                // back, but an empty field, as for a vendor named by its own
                // endpoint, is written so that endpoint is not offered.
                (chosen.uses_url() || url.trim().is_empty())
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

    use super::{KeySource, KeyState, Polishing, Provider, load_provider, save_provider};
    use crate::config::{ApiKey, Config, Format, KeyChange, Paths, Service};
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
            service: Service::Custom,
            format: Format::OpenAiCompatible,
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

        let (provider, key) = load_provider(&paths, ModelRole::Polish).unwrap();
        assert_eq!(
            (
                change,
                (provider, key.state(Service::Custom)),
                Config::load(&paths.config_file).unwrap().polish.api_key(),
            ),
            (
                ConfigChange::Saved,
                (
                    Provider::Polish {
                        polishing: Polishing::On,
                        service: Service::Custom,
                        format: Format::OpenAiCompatible,
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
            service: Service::Custom,
            format: Format::WhisperCpp,
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
                load_provider(&paths, ModelRole::Asr)
                    .unwrap()
                    .1
                    .state(Service::Custom)
            ),
            (CONFIG.to_string(), KeyState::Missing)
        );
    }

    #[tokio::test]
    async fn polishing_a_custom_server_needs_its_url() {
        let paths = scratch("provider-no-url");
        let provider = Provider::Polish {
            polishing: Polishing::On,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: " ".to_string(),
            model: "m".to_string(),
        };

        let error = save_provider(&paths, provider, KeyChange::Keep)
            .await
            .unwrap_err();

        assert_eq!(
            (
                error.to_string(),
                fs::read_to_string(&paths.config_file).unwrap()
            ),
            ("enter the server's URL".to_string(), CONFIG.to_string())
        );
    }

    #[tokio::test]
    async fn a_broken_endpoint_does_not_prevent_disabling_polishing() {
        let paths = scratch("provider-disable");
        let provider = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: "http://".to_string(),
            model: "m".to_string(),
        };

        let change = save_provider(&paths, provider.clone(), KeyChange::Keep)
            .await
            .unwrap();

        let (loaded, key) = load_provider(&paths, ModelRole::Polish).unwrap();
        assert_eq!(
            (change, loaded, key.state(Service::Custom)),
            (ConfigChange::Saved, provider, KeyState::Missing)
        );
    }

    #[tokio::test]
    async fn a_vendor_is_turned_off_whatever_url_it_left_behind() {
        let paths = scratch("provider-vendor-off");
        fs::write(
            &paths.config_file,
            format!("{CONFIG}\n[polish]\nservice = \"anthropic\"\nurl = \"https://ignored.example/chat/completions?token=unused\"\nmodel = \"m\"\n"),
        )
        .unwrap();
        let provider = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Anthropic,
            format: Format::OpenAiCompatible,
            url: "https://ignored.example/chat/completions?token=unused".to_string(),
            model: "m".to_string(),
        };

        assert_eq!(
            save_provider(&paths, provider, KeyChange::Keep)
                .await
                .unwrap(),
            ConfigChange::Saved
        );
    }

    #[tokio::test]
    async fn a_vendor_saved_with_an_empty_url_clears_the_one_left_behind() {
        let paths = scratch("provider-vendor-empty-url");
        fs::write(
            &paths.config_file,
            format!("{CONFIG}\n[polish]\nservice = \"anthropic\"\nurl = \"https://api.example/chat/completions\"\nmodel = \"m\"\n"),
        )
        .unwrap();
        let provider = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Anthropic,
            format: Format::OpenAiCompatible,
            url: String::new(),
            model: "m".to_string(),
        };

        save_provider(&paths, provider.clone(), KeyChange::Keep)
            .await
            .unwrap();

        assert_eq!(
            load_provider(&paths, ModelRole::Polish).unwrap().0,
            provider
        );
    }

    #[test]
    fn the_key_state_follows_the_chosen_service() {
        let key = KeySource {
            saved: false,
            variable: None,
            loaded: (Service::OpenAi, Some("PATH".to_string())),
        };
        let named = KeySource {
            variable: Some("HOME".to_string()),
            ..key.clone()
        };
        let saved = KeySource {
            saved: true,
            ..key.clone()
        };

        assert_eq!(
            [
                key.state(Service::OpenAi),
                key.state(Service::Custom),
                named.state(Service::Anthropic),
                saved.state(Service::Custom),
            ],
            [
                KeyState::Environment("PATH".to_string()),
                KeyState::Missing,
                KeyState::Environment("HOME".to_string()),
                KeyState::Saved,
            ]
        );
    }

    #[tokio::test]
    async fn a_vendor_keeps_the_server_url_and_polishing_off_needs_no_model() {
        let paths = scratch("provider-vendor");
        let polish = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Anthropic,
            format: Format::OpenAiCompatible,
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
                    format: Format::OpenAiCompatible,
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
                service: Service::Custom,
                format: Format::WhisperCpp,
                url: "http://".to_string(),
                model: "m".to_string()
            }
        );
    }
}
