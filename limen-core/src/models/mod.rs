//! The ASR and polishing model providers: their settings as saved, saving
//! them for a running daemon to apply, and the models their servers offer.
//!
//! Servers are asked for their models through the backend. Servers it does
//! not reach, such as the whisper.cpp server, list nothing, and any model
//! name may still be set.

use std::time::Duration;

use anyhow::Result;

use crate::backend::ModelBackend;
use crate::config::{self, ApiKey, Config, KeyChange, Paths, Service};
use crate::connection::{self, AsrRoute};

mod settings;

pub use settings::{KeySource, KeyState, Polishing, Provider, load_provider, save_provider};

/// How long a server may take to list its models.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Which provider: the one that transcribes or the one that polishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    /// `[provider]`, which transcribes each take.
    Asr,
    /// `[polish]`, which corrects each transcript.
    Polish,
}

/// The models the server `provider` names offers, in its order, as
/// `backend` lists them. The page asks while it is being edited, so
/// `provider` and `key` are its unsaved fields; a key not typed comes from
/// wherever saving would leave it.
pub async fn list_models(
    paths: &Paths,
    provider: &Provider,
    key: &KeyChange,
    backend: &dyn ModelBackend,
) -> Result<Vec<String>> {
    let (mut config, keys) = {
        let _lock = config::lock_dir(&paths.config_file)?;
        config::recover_provider_save(&paths.config_file)?;
        (
            Config::read_unchecked(&paths.config_file)?,
            config::saved_keys(&paths.config_file)?,
        )
    };
    let saved_key = |saved: Option<String>| match key {
        KeyChange::Set(key) => Some(key.clone()),
        KeyChange::Clear => None,
        KeyChange::Keep => saved.map(ApiKey::new),
    };
    // A config from before services keeps reading its key as it did while
    // the service it infers is still the chosen one.
    let named = |written: Option<Service>, inferred: Service, chosen: Service| match (
        written,
        inferred == chosen,
    ) {
        (None, true) => None,
        (Some(_), _) | (None, false) => Some(chosen),
    };
    let (service, endpoint, connection) = match provider {
        Provider::Asr { service, url, .. } => {
            config.provider.url = url.trim().to_string();
            config.provider.service =
                named(config.provider.service, config.provider.service(), *service);
            config.provider.saved_key = saved_key(keys.asr);
            match connection::asr(&config)? {
                AsrRoute::Api(connection) => (*service, url, connection),
                // Limen's own upload has no models route to ask.
                AsrRoute::Upload(url) => {
                    anyhow::bail!("{} lists no models", config::display_url(&url))
                }
            }
        }
        Provider::Polish { service, url, .. } => {
            config.polish.url = url.trim().to_string();
            config.polish.service = named(config.polish.service, config.polish.service(), *service);
            config.polish.saved_key = saved_key(keys.polish);
            // The key goes out before anything is saved: guard it as saving
            // and the polisher would.
            if service.uses_url() {
                config::check_polish_url(&config.polish.url)?;
                config::check_polish_route(&config.polish.url)?;
            }
            (
                *service,
                url,
                connection::polish(&config.polish, config.polish.api_key())?,
            )
        }
    };
    // Errors may quote the server; the endpoint alone is safe to show.
    let shown = match service.uses_url() {
        true => config::display_url(endpoint),
        false => service.as_str().to_string(),
    };
    let listed = tokio::time::timeout(LIST_TIMEOUT, backend.models(connection))
        .await
        .map_err(|_| anyhow::anyhow!("{shown} took too long to list its models"))?
        .map_err(|_| anyhow::anyhow!("failed to ask {shown} for its models"))?;
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

    use super::list_models;
    use crate::backend::testing::{Built, Recording, Refusing};
    use crate::backend::{Api, Connection};
    use crate::config::{ApiKey, KeyChange, Paths, Service};
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

    fn asr(service: Service, url: &str) -> Provider {
        Provider::Asr {
            service,
            url: url.to_string(),
            model: String::new(),
        }
    }

    #[tokio::test]
    async fn an_unsaved_server_lists_each_model_once_with_the_saved_key() {
        let paths = scratch("models-list", "[provider]\nurl = \"\"\n");
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
                Service::OpenAiCompatible,
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
                })]
            )
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
            service: Service::OpenAiCompatible,
            url: " http://127.0.0.1:8080/v1/chat/completions ".to_string(),
            model: String::new(),
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
            &asr(Service::WhisperCpp, "http://127.0.0.1:8080/inference"),
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
            service: Service::OpenAiCompatible,
            url: "http://192.0.2.1/v1/chat/completions".to_string(),
            model: String::new(),
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
}
