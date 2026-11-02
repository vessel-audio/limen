//! Where each role's requests go: the [`Connection`] its service names, or,
//! for the whisper.cpp service, Limen's own upload to its URL.

use anyhow::{Context, Result, bail};

use crate::backend::{Api, Connection};
use crate::config::{Config, PolishConfig, Service};

/// The transcription route under an OpenAI-compatible API root.
pub(crate) const TRANSCRIPTIONS: &str = "/audio/transcriptions";
/// The chat route under an OpenAI-compatible API root.
pub(crate) const CHAT_COMPLETIONS: &str = "/chat/completions";

/// How takes are transcribed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AsrRoute {
    /// Uploaded to exactly this URL, such as the whisper.cpp `/inference`.
    Upload(String),
    /// Sent through the backend.
    Api(Connection),
}

/// The API root of `endpoint`, whose path ends in `route`: for example
/// `http://127.0.0.1:8000/v1` for `…/v1/audio/transcriptions`. `None` when
/// the path ends otherwise or the URL has a query, which no route under a
/// root can carry.
pub(crate) fn base_url(endpoint: &str, route: &str) -> Option<String> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    match (url.has_host(), url.query(), url.fragment()) {
        (true, None, None) => url
            .path()
            .trim_end_matches('/')
            .strip_suffix(route)
            .map(|prefix| format!("{}{prefix}", url.origin().ascii_serialization())),
        (false, _, _) | (true, Some(_), _) | (true, None, Some(_)) => None,
    }
}

/// The route of the configured ASR service.
pub(crate) fn asr(config: &Config) -> Result<AsrRoute> {
    let provider = &config.provider;
    let service = provider.service();
    match service.api() {
        None => Ok(AsrRoute::Upload(provider.url.clone())),
        Some(api) => connect(
            service,
            api,
            &provider.url,
            TRANSCRIPTIONS,
            config.api_key(),
            provider.key_env(),
        )
        .map(AsrRoute::Api),
    }
}

/// The connection of the configured polishing service, which sends
/// `api_key`.
pub(crate) fn polish(config: &PolishConfig, api_key: Option<String>) -> Result<Connection> {
    let service = config.service();
    let api = service.api().with_context(|| {
        format!(
            "polish.service \"{}\" does not polish text",
            service.as_str()
        )
    })?;
    connect(
        service,
        api,
        &config.url,
        CHAT_COMPLETIONS,
        api_key,
        config.key_env(),
    )
}

/// The connection to `api`: at the API root of `url`, whose path ends in
/// `route`, for a server Limen is pointed at; a vendor's own endpoint,
/// which needs a key, otherwise.
fn connect(
    service: Service,
    api: Api,
    url: &str,
    route: &str,
    api_key: Option<String>,
    key_env: Option<&str>,
) -> Result<Connection> {
    match (service.uses_url(), api_key) {
        (true, api_key) => match base_url(url, route) {
            Some(base_url) => Ok(Connection {
                api,
                base_url: Some(base_url),
                api_key,
            }),
            None => bail!("the server URL must end in {route}, without a query"),
        },
        (false, Some(api_key)) => Ok(Connection {
            api,
            base_url: None,
            api_key: Some(api_key),
        }),
        (false, None) => bail!(
            "{} needs an API key: save one on its Models page{}",
            service.as_str(),
            key_env
                .map(|variable| format!(" or set {variable}"))
                .unwrap_or_default()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{CHAT_COMPLETIONS, TRANSCRIPTIONS, base_url};

    #[test]
    fn the_api_root_is_the_url_before_its_route() {
        assert_eq!(
            [
                (
                    "http://127.0.0.1:8000/v1/audio/transcriptions/",
                    TRANSCRIPTIONS
                ),
                (
                    "https://ai.example/openai/chat/completions",
                    CHAT_COMPLETIONS
                ),
                (
                    "https://api.deepseek.com/chat/completions",
                    CHAT_COMPLETIONS
                ),
                ("http://127.0.0.1:8080/inference", TRANSCRIPTIONS),
                (
                    "http://127.0.0.1:8000/v1/xaudio/transcriptions",
                    TRANSCRIPTIONS
                ),
                (
                    "https://ai.example/chat/completions?api-version=1",
                    CHAT_COMPLETIONS
                ),
                ("http://", TRANSCRIPTIONS),
            ]
            .map(|(endpoint, route)| base_url(endpoint, route)),
            [
                Some("http://127.0.0.1:8000/v1".to_string()),
                Some("https://ai.example/openai".to_string()),
                Some("https://api.deepseek.com".to_string()),
                None,
                None,
                None,
                None,
            ]
        );
    }
}

#[cfg(test)]
mod routes {
    use super::{AsrRoute, asr, polish};
    use crate::backend::{Api, Connection};
    use crate::config::{ApiKey, Config, PolishConfig, Service};

    fn config(provider: &str) -> Config {
        toml::from_str(&format!("[provider]\n{provider}\nmodel = \"asr\"\n")).unwrap()
    }

    #[test]
    fn a_config_without_a_service_keeps_its_route() {
        assert_eq!(
            [
                "url = \"http://127.0.0.1:8000/v1/audio/transcriptions\"",
                "url = \"http://127.0.0.1:8080/inference\"",
                "url = \"https://asr.example/v1/audio/transcriptions?api-version=1\"",
            ]
            .map(|provider| asr(&config(provider)).unwrap()),
            [
                AsrRoute::Api(Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    api_key: None,
                }),
                AsrRoute::Upload("http://127.0.0.1:8080/inference".to_string()),
                AsrRoute::Upload(
                    "https://asr.example/v1/audio/transcriptions?api-version=1".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_vendor_ignores_the_url_and_needs_a_key() {
        let mut config = config("service = \"groq\"\nurl = \"http://stale.example/inference\"");
        config.provider.api_key_env = Some("LIMEN_TEST_DEFINITELY_MISSING_ASR_KEY".to_string());
        assert_eq!(
            asr(&config).unwrap_err().to_string(),
            "groq needs an API key: save one on its Models page or set LIMEN_TEST_DEFINITELY_MISSING_ASR_KEY"
        );

        config.provider.saved_key = Some(ApiKey::new("sk-groq".to_string()));
        assert_eq!(
            asr(&config).unwrap(),
            AsrRoute::Api(Connection {
                api: Api::Groq,
                base_url: None,
                api_key: Some("sk-groq".to_string()),
            })
        );
    }

    #[test]
    fn polishing_reaches_a_vendor_or_the_api_root_of_its_server() {
        let vendor = PolishConfig {
            service: Some(Service::Anthropic),
            ..PolishConfig::default()
        };
        let server = PolishConfig {
            url: "http://127.0.0.1:8080/v1/chat/completions".to_string(),
            ..PolishConfig::default()
        };
        assert_eq!(
            [
                polish(&vendor, Some("sk-ant".to_string())).unwrap(),
                polish(&server, None).unwrap(),
            ],
            [
                Connection {
                    api: Api::Anthropic,
                    base_url: None,
                    api_key: Some("sk-ant".to_string()),
                },
                Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                    api_key: None,
                },
            ]
        );
    }
}
