//! A provider's settings as the GUI edits them: its service, URL, model,
//! and key, and for polishing whether it is on.

use anyhow::{Context, Result, bail};

use super::ModelRole;
use std::collections::BTreeMap;

use crate::config::{self, Config, Format, KeyChange, Paths, ProviderOptions, Service};
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
        /// Every provider's settings for the role; saving writes only the
        /// chosen service's.
        options: BTreeMap<Service, ProviderOptions>,
    },
    /// The service that polishes transcripts, and its model, with a custom
    /// server's format and URL as for [`Provider::Asr`].
    Polish {
        polishing: Polishing,
        service: Service,
        format: Format,
        url: String,
        model: String,
        options: BTreeMap<Service, ProviderOptions>,
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
/// chooses: the key saved for its role and the variable the config reads,
/// for the saved service, or else the chosen service's own variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySource {
    saved: bool,
    /// The saved service and the variable the config reads for it:
    /// `api_key_env`, the service's own, or, for a config from before
    /// services, the old default.
    loaded: (Service, Option<String>),
}

impl KeySource {
    /// Where the key comes from once `service` is chosen. The saved key and
    /// the variable the config names belong to the saved service; another
    /// one has only its own variable, such as OPENAI_API_KEY.
    pub fn state(&self, service: Service) -> KeyState {
        let (saved, variable) = match &self.loaded {
            (loaded, variable) if *loaded == service => (self.saved, variable.clone()),
            _ => (false, service.key_env().map(str::to_string)),
        };
        let set = |variable: &String| std::env::var(variable).is_ok_and(|key| !key.is_empty());
        match (saved, variable.filter(set)) {
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
    let (provider, saved, resolved) = match role {
        ModelRole::Asr => (
            Provider::Asr {
                service: config.provider.service(),
                format: config.provider.format(),
                url: config.provider.url.clone(),
                model: config.provider.model.clone(),
                options: config.provider.options.clone(),
            },
            keys.asr,
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
                options: config.polish.options.clone(),
            },
            keys.polish,
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
    // The key stored for the role, and the variable the config names for
    // it, belong to the provider saved; another one starts without them.
    let current = Config::read_unchecked(&paths.config_file)?;
    let moved = !super::is_saved(&current, &provider);
    let key = match (key, moved) {
        (KeyChange::Keep, true) => KeyChange::Clear,
        (key, _) => key,
    };
    let unset = |name: config::Key| moved.then_some((name, config::Value::Unset));
    let text = |text: String| config::Value::Text(text.trim().to_string());
    let service = |service: Service| config::Value::Text(service.as_str().to_string());
    let format = |format: Format| config::Value::Text(format.as_str().to_string());
    let (role, values, request) = match provider {
        Provider::Asr {
            service: chosen,
            format: spoken,
            url,
            model,
            options,
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
                Some((
                    config::Key::ProviderOptions(chosen),
                    config::Value::Options(options.get(&chosen).cloned().unwrap_or_default()),
                )),
                unset(config::Key::ProviderKeyEnv),
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
            options,
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
                Some((
                    config::Key::PolishOptions(chosen),
                    config::Value::Options(options.get(&chosen).cloned().unwrap_or_default()),
                )),
                unset(config::Key::PolishKeyEnv),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
            v1::Request::PolishReload,
        ),
    };
    let file = paths.config_file.clone();
    // The writes wait on a file lock; keep them off the caller's runtime thread.
    tokio::task::spawn_blocking(move || {
        config::update(&file, &values, config::Secret::Key(role), &key)
    })
    .await
    .context("the config writer stopped")??;
    reload(paths, &request).await
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::collections::BTreeMap;

    use super::{KeySource, KeyState, Polishing, Provider, load_provider, save_provider};
    use crate::config::{ApiKey, Config, Format, KeyChange, Paths, ProviderOptions, Service};
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
            options: Default::default(),
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
                        options: Default::default(),
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
            options: Default::default(),
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
            options: Default::default(),
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
            options: Default::default(),
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
            options: Default::default(),
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
            options: Default::default(),
        };

        save_provider(&paths, provider.clone(), KeyChange::Keep)
            .await
            .unwrap();

        assert_eq!(
            load_provider(&paths, ModelRole::Polish).unwrap().0,
            provider
        );
    }

    #[tokio::test]
    async fn another_provider_starts_without_the_saved_key_or_variable() {
        let paths = scratch("provider-moved");
        fs::write(
            &paths.config_file,
            format!("{CONFIG}api_key_env = \"MY_ASR_KEY\"\n"),
        )
        .unwrap();
        fs::write(
            paths.config_file.with_file_name("keys.toml"),
            "asr = \"sk-server-a\"\n",
        )
        .unwrap();
        let groq = Provider::Asr {
            service: Service::Groq,
            format: Format::OpenAiCompatible,
            url: "http://127.0.0.1:8000/v1/audio/transcriptions".to_string(),
            model: "whisper".to_string(),
            options: Default::default(),
        };

        save_provider(&paths, groq, KeyChange::Keep).await.unwrap();

        let config = Config::load(&paths.config_file).unwrap();
        assert_eq!(
            (config.provider.api_key_env, config.provider.saved_key),
            (None, None)
        );
    }

    #[tokio::test]
    async fn each_provider_s_settings_are_saved_and_cleared_on_their_own() {
        let paths = scratch("provider-options");
        let groq = ProviderOptions {
            timeout_ms: Some(8000),
            ..ProviderOptions::default()
        };
        let custom = ProviderOptions {
            key_header: Some("api-key".to_string()),
            retries: Some(2),
            body: Some(r#"{"prompt": "names"}"#.to_string()),
            headers: [("X-Org".to_string(), "limen".to_string())].into(),
            ..ProviderOptions::default()
        };
        let asr = |options: BTreeMap<Service, ProviderOptions>| Provider::Asr {
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: "http://127.0.0.1:8000/v1".to_string(),
            model: "m".to_string(),
            options,
        };

        // The groq table is not the chosen service's, so it is not written.
        save_provider(
            &paths,
            asr([(Service::Custom, custom.clone()), (Service::Groq, groq)].into()),
            KeyChange::Keep,
        )
        .await
        .unwrap();
        let saved = Config::load(&paths.config_file).unwrap().provider.options;
        save_provider(&paths, asr(BTreeMap::new()), KeyChange::Keep)
            .await
            .unwrap();
        let cleared = Config::load(&paths.config_file).unwrap().provider.options;

        assert_eq!(
            (saved, cleared),
            ([(Service::Custom, custom)].into(), BTreeMap::new())
        );
    }

    #[test]
    fn the_key_state_follows_the_chosen_service() {
        let key = KeySource {
            saved: false,
            loaded: (Service::OpenAi, Some("PATH".to_string())),
        };
        let saved = KeySource {
            saved: true,
            ..key.clone()
        };

        // Another service has neither the saved key nor the saved variable.
        assert_eq!(
            [
                key.state(Service::OpenAi),
                key.state(Service::Custom),
                saved.state(Service::OpenAi),
                saved.state(Service::Custom),
            ],
            [
                KeyState::Environment("PATH".to_string()),
                KeyState::Missing,
                KeyState::Saved,
                KeyState::Missing,
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
            options: Default::default(),
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
                    options: Default::default(),
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
                model: "m".to_string(),
                options: Default::default(),
            }
        );
    }
}
