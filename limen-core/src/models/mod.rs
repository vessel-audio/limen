//! The ASR and polishing model providers: their settings as saved, saving
//! them for a running daemon to apply, and the models their servers offer.
//!
//! Servers are asked for their models through the OpenAI-compatible
//! `GET …/models` route beside the configured one. Servers without it, such
//! as the whisper.cpp server, list nothing, and any model name may still be
//! set.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{self, Config, Paths};

mod settings;

pub use settings::{KeyState, Polishing, Provider, load_provider, save_provider};

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

/// The `…/models` URL beside `endpoint`, whose path ends in `route`, keeping
/// its query, which some servers need, such as `?api-version=`.
fn models_url(endpoint: &str, route: &str) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(endpoint).ok()?;
    let prefix = url
        .path()
        .trim_end_matches('/')
        .strip_suffix(route)
        .filter(|prefix| prefix.ends_with('/'))?
        .to_string();
    url.set_path(&format!("{prefix}models"));
    Some(url)
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ListedModel>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
}

/// The models the configured server for `role` offers, in its order.
pub async fn list_models(paths: &Paths, role: ModelRole) -> Result<Vec<String>> {
    let config = Config::load(&paths.config_file)
        .map_err(|_| anyhow::anyhow!("cannot load the provider settings"))?;
    // The configured endpoint, and the route at its end that `models` replaces.
    let (endpoint, route, api_key) = match role {
        ModelRole::Asr => (
            &config.provider.url,
            "audio/transcriptions",
            config.api_key(),
        ),
        ModelRole::Polish => {
            // The key goes out even while polishing is disabled, when the
            // config does not check the URL: guard it as the polisher would.
            config::check_polish_url(&config.polish.url)?;
            (
                &config.polish.url,
                "chat/completions",
                config.polish.api_key(),
            )
        }
    };
    let url = models_url(endpoint, route).ok_or_else(|| {
        anyhow::anyhow!(
            "{} does not end in /{route}, so it lists no models",
            config::display_url(endpoint)
        )
    })?;
    let shown = config::display_url(url.as_str());
    let http = reqwest::Client::builder()
        .timeout(LIST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build the HTTP client")?;
    let request = match api_key {
        Some(key) => http.get(url.clone()).bearer_auth(key),
        None => http.get(url.clone()),
    };
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("failed to ask {shown} for its models"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{shown} returned HTTP {status}");
    }
    let list = response
        .json::<ModelList>()
        .await
        .map_err(|_| anyhow::anyhow!("{shown} did not return a model list"))?;
    let mut models: Vec<String> = Vec::new();
    for model in list.data {
        if !models.contains(&model.id) {
            models.push(model.id);
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{ModelRole, list_models, models_url};
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

    #[test]
    fn the_models_route_sits_beside_the_configured_one() {
        assert_eq!(
            [
                (
                    "http://127.0.0.1:8000/v1/audio/transcriptions/",
                    "audio/transcriptions"
                ),
                (
                    "https://ai.example/openai/chat/completions?api-version=1",
                    "chat/completions"
                ),
                ("http://127.0.0.1:8080/inference", "audio/transcriptions"),
                (
                    "http://127.0.0.1:8000/v1/xaudio/transcriptions",
                    "audio/transcriptions"
                ),
            ]
            .map(|(endpoint, route)| models_url(endpoint, route).map(String::from)),
            [
                Some("http://127.0.0.1:8000/v1/models".to_string()),
                Some("https://ai.example/openai/models?api-version=1".to_string()),
                None,
                None,
            ]
        );
    }

    #[tokio::test]
    async fn a_server_lists_each_model_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let body = r#"{"object":"list","data":[{"id":"b"},{"id":"a"},{"id":"b"}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..read]).into_owned()
        });
        let paths = scratch(
            "models-list",
            &format!(
                "[provider]\nurl = \"http://{address}/v1/audio/transcriptions\"\nmodel = \"m\"\n"
            ),
        );

        let models = list_models(&paths, ModelRole::Asr).await.unwrap();

        assert_eq!(models, ["b", "a"]);
        assert!(server.await.unwrap().starts_with("GET /v1/models "));
    }

    #[tokio::test]
    async fn the_polish_key_never_goes_to_plain_http_elsewhere() {
        let paths = scratch(
            "models-insecure",
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n\n[polish]\nenabled = false\nurl = \"http://192.0.2.1/v1/chat/completions\"\n",
        );

        let error = list_models(&paths, ModelRole::Polish).await.unwrap_err();

        assert_eq!(
            error.to_string(),
            "polish.url must use HTTPS, or HTTP on a loopback address"
        );
    }
}
