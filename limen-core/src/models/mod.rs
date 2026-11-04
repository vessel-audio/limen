//! The ASR and polishing model providers: their settings as saved, saving
//! them for a running daemon to apply, and the models their servers offer.
//!
//! Servers are asked for their models through the backend, and a model can
//! be tried with one request before it is saved. Servers the backend does
//! not reach, such as the whisper.cpp server, list nothing, and any model
//! name may still be set.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::backend::ModelBackend;

use crate::config::{self, ApiKey, Config, KeyChange, Paths, ProviderOptions, Service};
use crate::connection::{self, AsrRoute};

mod check;
mod settings;

pub use check::check_model;
pub use settings::{KeySource, KeyState, Polishing, Provider, load_provider, save_provider};

/// How long a server may take to list its models.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Which provider: the one that transcribes or the one that polishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    /// `[provider]`, which transcribes each take.
    Asr,
    /// `[polish]`, which corrects each transcript.
    Polish,
}

/// Whether `provider` is the one `config` saved for its role, so the key
/// stored for the role belongs to it: a custom server speaking the same
/// format at the same URL, or the same vendor.
pub(crate) fn is_saved(config: &Config, provider: &Provider) -> bool {
    // The page writes a saved URL back in its own form, such as without a
    // default port, so compare URLs as parsed.
    let normalized = |url: &str| {
        reqwest::Url::parse(url.trim())
            .map(String::from)
            .unwrap_or_else(|_| url.trim().to_string())
            .trim_end_matches('/')
            .to_string()
    };
    // A vendor's endpoint: the base URL its settings name, else its own;
    // its own written out is the same endpoint as none.
    let vendor_url = |options: &BTreeMap<Service, ProviderOptions>, service: Service| {
        options
            .get(&service)
            .and_then(|options| options.url.as_deref())
            .map(normalized)
            .filter(|url| Some(url.as_str()) != service.default_url())
    };
    let (saved, chosen) = match provider {
        Provider::Asr {
            service,
            format,
            url,
            options,
            ..
        } => (
            (
                config.provider.service(),
                config.provider.format(),
                config.provider.url.as_str(),
                vendor_url(&config.provider.options, *service),
            ),
            (
                *service,
                *format,
                url.as_str(),
                vendor_url(options, *service),
            ),
        ),
        Provider::Polish {
            service,
            format,
            url,
            options,
            ..
        } => (
            (
                config.polish.service(),
                config.polish.format(),
                config.polish.url.as_str(),
                vendor_url(&config.polish.options, *service),
            ),
            (
                *service,
                *format,
                url.as_str(),
                vendor_url(options, *service),
            ),
        ),
    };
    // A custom server is the same one at the same URL speaking the same
    // format; a vendor, the same vendor at the same endpoint.
    chosen.0 == saved.0
        && match chosen.0.uses_url() {
            true => chosen.1 == saved.1 && normalized(saved.2) == normalized(chosen.2),
            false => chosen.3 == saved.3,
        }
}

/// The config as the page's unsaved fields would leave it for `provider`'s
/// role: its service, URL, and model, and which key it sends. A key the
/// user typed is sent wherever they point it; a stored one, saved or named
/// by `api_key_env`, only to the service and server it was saved for, so a
/// half-typed address never receives it. Elsewhere only the service's own
/// variable, such as OPENAI_API_KEY, is read.
fn edited(paths: &Paths, provider: &Provider, key: &KeyChange) -> Result<Config> {
    let (mut config, keys) = {
        let _lock = config::lock_dir(&paths.config_file)?;
        config::recover_provider_save(&paths.config_file)?;
        (
            Config::read_unchecked(&paths.config_file)?,
            config::saved_keys(&paths.config_file)?,
        )
    };
    let stored = |saved: Option<String>, same: bool| match (key, same) {
        (KeyChange::Set(key), _) => Some(key.clone()),
        (KeyChange::Keep, true) => saved.map(ApiKey::new),
        (KeyChange::Clear, _) | (KeyChange::Keep, false) => None,
    };
    let same = is_saved(&config, provider);
    match provider {
        Provider::Asr {
            service,
            format,
            url,
            model,
            options,
        } => {
            // A key goes out before anything is saved, so the settings are
            // checked as saving would, a vendor URL among them.
            for (service, options) in options {
                options.check(&format!("provider.options.{}", service.as_str()))?;
            }
            config.provider.options = options.clone();
            config.provider.saved_key = stored(keys.asr, same);
            config.provider.model = model.trim().to_string();
            match same {
                true => {}
                false => {
                    config.provider.service = Some(*service);
                    config.provider.format = Some(*format);
                    config.provider.url = url.trim().to_string();
                    config.provider.api_key_env = None;
                }
            }
        }
        Provider::Polish {
            service,
            format,
            url,
            model,
            options,
            ..
        } => {
            for (service, options) in options {
                options.check(&format!("polish.options.{}", service.as_str()))?;
            }
            config.polish.options = options.clone();
            config.polish.saved_key = stored(keys.polish, same);
            config.polish.model = model.trim().to_string();
            match same {
                true => {}
                false => {
                    config.polish.service = Some(*service);
                    config.polish.format = Some(*format);
                    config.polish.url = url.trim().to_string();
                    config.polish.api_key_env = None;
                }
            }
            // The key goes out before anything is saved: guard it as saving
            // and the polisher would.
            if service.uses_url() {
                config::check_polish_url(url.trim())?;
                config::check_polish_root(url.trim(), *format)?;
            }
        }
    }
    Ok(config)
}

/// [`edited`], read on a blocking thread: the config lock may wait on a
/// save, which must not hold up the caller's runtime.
async fn edited_off_thread(paths: &Paths, provider: &Provider, key: &KeyChange) -> Result<Config> {
    let (paths, provider, key) = (paths.clone(), provider.clone(), key.clone());
    tokio::task::spawn_blocking(move || edited(&paths, &provider, &key))
        .await
        .context("the config reader stopped")?
}

/// The models the server `provider` names offers, in its order, as
/// `backend` lists them. The page asks while it is being edited, so
/// `provider` and `key` are its unsaved fields, sent as [`edited`] says.
pub async fn list_models(
    paths: &Paths,
    provider: &Provider,
    key: &KeyChange,
    backend: &dyn ModelBackend,
) -> Result<Vec<String>> {
    // Errors may quote the server; the endpoint alone is safe to show.
    let shown = match provider {
        Provider::Asr { service, url, .. } | Provider::Polish { service, url, .. } => {
            match service.uses_url() {
                true => config::display_url(url.trim()),
                false => service.as_str().to_string(),
            }
        }
    };
    let list = async {
        let config = edited_off_thread(paths, provider, key).await?;
        let connection = match provider {
            Provider::Asr { .. } => match connection::asr(&config)? {
                AsrRoute::Api(connection) => connection,
                // Limen's own upload has no models route to ask.
                AsrRoute::Upload(..) => anyhow::bail!("{shown} lists no models"),
            },
            Provider::Polish { .. } => connection::polish(&config.polish, config.polish.api_key())?,
        };
        backend
            .models(connection)
            .await
            .map_err(|_| anyhow::anyhow!("failed to ask {shown} for its models"))
    };
    let listed = tokio::time::timeout(LIST_TIMEOUT, list)
        .await
        .map_err(|_| anyhow::anyhow!("{shown} took too long to list its models"))??;
    Ok(listed.into_iter().fold(Vec::new(), |mut models, model| {
        if !models.contains(&model) {
            models.push(model);
        }
        models
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{is_saved, list_models};
    use crate::backend::testing::{Built, Recording, Refusing};
    use crate::backend::{Api, Connection, Shape};
    use crate::config::{
        ApiKey, ChatApi, Config, Format, KeyChange, Paths, ProviderOptions, Service,
    };
    use crate::models::{Polishing, Provider};

    /// Paths in a fresh directory holding `config`, where no daemon listens.
    fn scratch(name: &str, config: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), config).unwrap();
        Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        }
    }

    fn asr(format: Format, url: &str) -> Provider {
        Provider::Asr {
            service: Service::Custom,
            format,
            url: url.to_string(),
            model: String::new(),
            options: Default::default(),
        }
    }

    #[tokio::test]
    async fn an_unsaved_server_lists_each_model_once_with_the_saved_key() {
        let paths = scratch(
            "models-list",
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\n",
        );
        fs::write(
            paths.config_file.with_file_name("keys.toml"),
            "asr = \"sk-saved\"\n",
        )
        .unwrap();
        let backend = Recording {
            models: ["b", "a", "b"].map(String::from).to_vec(),
            ..Recording::default()
        };

        let models = list_models(
            &paths,
            &asr(
                Format::OpenAiCompatible,
                "http://127.0.0.1:8000/v1/audio/transcriptions",
            ),
            &KeyChange::Keep,
            &backend,
        )
        .await
        .unwrap();

        assert_eq!(
            (models, backend.built.lock().unwrap().clone()),
            (
                vec!["b".to_string(), "a".to_string()],
                vec![Built::Models(Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    api_key: Some("sk-saved".to_string()),
                    shape: Default::default(),
                })]
            )
        );
    }

    #[tokio::test]
    async fn a_stored_key_never_reaches_another_server_or_service() {
        let paths = scratch(
            "models-stored-key",
            "[provider]\nurl = \"\"\n\n[polish]\nservice = \"custom\"\nurl = \"https://saved.example/v1/chat/completions\"\napi_key_env = \"PATH\"\n",
        );
        fs::write(
            paths.config_file.with_file_name("keys.toml"),
            "polish = \"sk-saved\"\n",
        )
        .unwrap();
        let backend = Recording::default();
        let polish = |service, url: &str| Provider::Polish {
            polishing: Polishing::Off,
            service,
            format: Format::OpenAiCompatible,
            url: url.to_string(),
            model: String::new(),
            options: Default::default(),
        };

        for provider in [
            polish(Service::Custom, "https://other.example/v1/chat/completions"),
            polish(Service::Custom, "https://saved.e"),
        ] {
            let _ = list_models(&paths, &provider, &KeyChange::Keep, &backend).await;
        }

        let keyless = |base_url: &str| {
            Built::Models(Connection {
                api: Api::Compatible,
                base_url: Some(base_url.to_string()),
                api_key: None,
                shape: Shape {
                    chat_api: Some(ChatApi::ChatCompletions),
                    ..Shape::default()
                },
            })
        };
        assert_eq!(
            backend.built.lock().unwrap().clone(),
            [
                keyless("https://other.example/v1"),
                keyless("https://saved.e")
            ]
        );
    }

    #[tokio::test]
    async fn the_saved_server_written_another_way_keeps_its_key() {
        let paths = scratch(
            "models-normalized",
            "[provider]\nurl = \"https://asr.example:443/v1/audio/transcriptions/\"\n",
        );
        fs::write(
            paths.config_file.with_file_name("keys.toml"),
            "asr = \"sk-saved\"\n",
        )
        .unwrap();
        let backend = Recording::default();

        list_models(
            &paths,
            &asr(
                Format::OpenAiCompatible,
                "https://asr.example/v1/audio/transcriptions",
            ),
            &KeyChange::Keep,
            &backend,
        )
        .await
        .unwrap();

        assert_eq!(
            backend.built.lock().unwrap().clone(),
            [Built::Models(Connection {
                api: Api::Compatible,
                base_url: Some("https://asr.example/v1".to_string()),
                api_key: Some("sk-saved".to_string()),
                shape: Default::default(),
            })]
        );
    }

    #[tokio::test]
    async fn a_typed_key_is_asked_with_and_a_removed_one_is_not() {
        let paths = scratch("models-typed-key", "[provider]\nurl = \"\"\n");
        fs::write(
            paths.config_file.with_file_name("keys.toml"),
            "polish = \"sk-saved\"\n",
        )
        .unwrap();
        let backend = Recording::default();
        let polish = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: " http://127.0.0.1:8080/v1/chat/completions ".to_string(),
            model: String::new(),
            options: Default::default(),
        };

        for key in [
            KeyChange::Set(ApiKey::new("sk-typed".to_string())),
            KeyChange::Clear,
        ] {
            list_models(&paths, &polish, &key, &backend).await.unwrap();
        }

        let asked = |api_key: Option<&str>| {
            Built::Models(Connection {
                api: Api::Compatible,
                base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                api_key: api_key.map(str::to_string),
                shape: Shape {
                    chat_api: Some(ChatApi::ChatCompletions),
                    ..Shape::default()
                },
            })
        };
        assert_eq!(
            backend.built.lock().unwrap().clone(),
            [asked(Some("sk-typed")), asked(None)]
        );
    }

    #[tokio::test]
    async fn an_upload_route_lists_no_models() {
        let paths = scratch("models-upload", "[provider]\nurl = \"\"\n");

        let error = list_models(
            &paths,
            &asr(Format::WhisperCpp, "http://127.0.0.1:8080/inference"),
            &KeyChange::Keep,
            &Refusing,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "http://127.0.0.1:8080/inference lists no models"
        );
    }

    #[tokio::test]
    async fn the_polish_key_never_goes_to_plain_http_elsewhere() {
        let paths = scratch("models-insecure", "[provider]\nurl = \"\"\n");
        let polish = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url: "http://192.0.2.1/v1/chat/completions".to_string(),
            model: String::new(),
            options: Default::default(),
        };

        let error = list_models(
            &paths,
            &polish,
            &KeyChange::Set(ApiKey::new("sk".to_string())),
            &Refusing,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "polish.url must use HTTPS, or HTTP on a loopback address"
        );
    }

    #[tokio::test]
    async fn a_vendor_url_is_checked_before_a_key_goes_to_it() {
        let paths = scratch("models-vendor-url", "[provider]\nurl = \"\"\n");
        let polish = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::OpenAi,
            format: Format::OpenAiCompatible,
            url: String::new(),
            model: String::new(),
            options: [(
                Service::OpenAi,
                ProviderOptions {
                    url: Some("http://192.0.2.1/v1".to_string()),
                    ..ProviderOptions::default()
                },
            )]
            .into(),
        };
        let backend = Recording::default();

        let error = list_models(
            &paths,
            &polish,
            &KeyChange::Set(ApiKey::new("sk".to_string())),
            &backend,
        )
        .await
        .unwrap_err();

        assert_eq!(
            (error.to_string(), backend.built.lock().unwrap().clone()),
            (
                "polish.options.openai.url must use HTTPS, or HTTP on a loopback address"
                    .to_string(),
                Vec::new()
            )
        );
    }

    #[test]
    fn a_vendor_s_own_url_written_out_is_its_own_endpoint() {
        let config: Config = toml::from_str(
            "[provider]\nurl = \"\"\n\n[polish]\nservice = \"openai\"\n\n[polish.options.openai]\nurl = \"https://api.openai.com/v1/\"\n",
        )
        .unwrap();
        let unset = Provider::Polish {
            polishing: Polishing::Off,
            service: Service::OpenAi,
            format: Format::OpenAiCompatible,
            url: String::new(),
            model: String::new(),
            options: Default::default(),
        };

        assert!(is_saved(&config, &unset));
    }
}
