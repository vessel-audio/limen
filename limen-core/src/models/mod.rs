//! The ASR and polishing model providers: their settings as saved, saving
//! them for a running daemon to apply, and the models their servers offer.
//!
//! Servers are asked for their models through the backend. Servers it does
//! not reach, such as the whisper.cpp server, list nothing, and any model
//! name may still be set.

use std::time::Duration;

use anyhow::Result;

use crate::backend::ModelBackend;
use crate::config::{self, Config, Paths};
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

/// The models the configured server for `role` offers, in its order, as
/// `backend` lists them.
pub async fn list_models(
    paths: &Paths,
    role: ModelRole,
    backend: &dyn ModelBackend,
) -> Result<Vec<String>> {
    let config = Config::load(&paths.config_file)
        .map_err(|_| anyhow::anyhow!("cannot load the provider settings"))?;
    let (service, endpoint, connection) = match role {
        ModelRole::Asr => match connection::asr(&config)? {
            AsrRoute::Api(connection) => {
                (config.provider.service(), &config.provider.url, connection)
            }
            // Limen's own upload has no models route to ask.
            AsrRoute::Upload(url) => {
                anyhow::bail!("{} lists no models", config::display_url(&url))
            }
        },
        ModelRole::Polish => {
            let service = config.polish.service();
            // The key goes out even while polishing is disabled, when the
            // config does not check the URL: guard it as the polisher would.
            if service.uses_url() {
                config::check_polish_url(&config.polish.url)?;
            }
            (
                service,
                &config.polish.url,
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

    use super::{ModelRole, list_models};
    use crate::backend::testing::{Built, Recording, Refusing};
    use crate::backend::{Api, Connection};
    use crate::config::Paths;

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

    #[tokio::test]
    async fn a_server_lists_each_model_once_from_its_api_root() {
        let paths = scratch(
            "models-list",
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n",
        );
        let backend = Recording {
            models: ["b", "a", "b"].map(String::from).to_vec(),
            ..Recording::default()
        };

        let models = list_models(&paths, ModelRole::Asr, &backend).await.unwrap();

        assert_eq!(
            (models, backend.built.lock().unwrap().clone()),
            (
                vec!["b".to_string(), "a".to_string()],
                vec![Built::Models(Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    api_key: None,
                })]
            )
        );
    }

    #[tokio::test]
    async fn an_upload_route_lists_no_models() {
        let paths = scratch(
            "models-upload",
            "[provider]\nurl = \"http://127.0.0.1:8080/inference\"\nmodel = \"m\"\n",
        );

        let error = list_models(&paths, ModelRole::Asr, &Refusing)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "http://127.0.0.1:8080/inference lists no models"
        );
    }

    #[tokio::test]
    async fn the_polish_key_never_goes_to_plain_http_elsewhere() {
        let paths = scratch(
            "models-insecure",
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n\n[polish]\nenabled = false\nurl = \"http://192.0.2.1/v1/chat/completions\"\n",
        );

        let error = list_models(&paths, ModelRole::Polish, &Refusing)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "polish.url must use HTTPS, or HTTP on a loopback address"
        );
    }
}
