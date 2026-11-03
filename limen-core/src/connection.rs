//! Where each role's requests go: a vendor's own endpoint, the API root of
//! a custom server in the format it speaks, or, for a custom whisper.cpp
//! server, Limen's own upload to its URL.

use anyhow::{Context, Result, bail};

use crate::backend::{Connection, Shape};
use crate::config::{Config, Format, PolishConfig, ProviderOptions, Service};

/// The transcription route under an OpenAI-compatible API root.
pub(crate) const TRANSCRIPTIONS: &str = "/audio/transcriptions";
/// The chat route under an OpenAI-compatible API root.
pub(crate) const CHAT_COMPLETIONS: &str = "/chat/completions";

/// How takes are transcribed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AsrRoute {
    /// Uploaded to exactly this URL, such as the whisper.cpp `/inference`,
    /// with the headers and fields its shape adds.
    Upload(String, Shape),
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

/// The API root a custom server's `url` names. An OpenAI-compatible server
/// may also be named by its `route`, as configs from before custom servers
/// did. `None` when the URL has no host, or a query, which no route under a
/// root can carry.
pub(crate) fn custom_root(url: &str, format: Format, route: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    match (parsed.has_host(), parsed.query(), parsed.fragment()) {
        (true, None, None) => {
            let path = parsed.path().trim_end_matches('/');
            let path = match format {
                Format::OpenAiCompatible => path.strip_suffix(route).unwrap_or(path),
                Format::WhisperCpp
                | Format::OpenAi
                | Format::Anthropic
                | Format::Gemini
                | Format::DeepSeek
                | Format::Mistral
                | Format::Groq
                | Format::OpenRouter
                | Format::XAi => path,
            };
            Some(format!("{}{path}", parsed.origin().ascii_serialization()))
        }
        (false, _, _) | (true, Some(_), _) | (true, None, Some(_)) => None,
    }
}

/// How the provider `options` set up shapes its requests.
fn shape(options: Option<&ProviderOptions>) -> Result<Shape> {
    let options = options.cloned().unwrap_or_default();
    Ok(Shape {
        body: options.body_fields()?,
        path: options.path,
        key_header: options.key_header,
        headers: options.headers.into_iter().collect(),
    })
}

/// The route of the configured ASR service.
pub(crate) fn asr(config: &Config) -> Result<AsrRoute> {
    let provider = &config.provider;
    let shape = shape(provider.options.get(&provider.service()))?;
    match (provider.service(), provider.format()) {
        (Service::Custom, Format::WhisperCpp) => Ok(AsrRoute::Upload(provider.url.clone(), shape)),
        (Service::Custom, format) => custom(
            format,
            &provider.url,
            TRANSCRIPTIONS,
            config.api_key(),
            shape,
        )
        .map(AsrRoute::Api),
        (vendor, _) => {
            vendor_endpoint(vendor, config.api_key(), provider.key_env(), shape).map(AsrRoute::Api)
        }
    }
}

/// The connection of the configured polishing service, which sends
/// `api_key`.
pub(crate) fn polish(config: &PolishConfig, api_key: Option<String>) -> Result<Connection> {
    let shape = shape(config.options.get(&config.service()))?;
    match config.service() {
        Service::Custom => custom(
            config.format(),
            &config.url,
            CHAT_COMPLETIONS,
            api_key,
            shape,
        ),
        vendor => vendor_endpoint(vendor, api_key, config.key_env(), shape),
    }
}

/// The connection to a custom server speaking `format` at `url`, which
/// may need no key.
fn custom(
    format: Format,
    url: &str,
    route: &str,
    api_key: Option<String>,
    shape: Shape,
) -> Result<Connection> {
    let api = format
        .api()
        .with_context(|| format!("a {} server cannot do this", format.as_str()))?;
    let base_url = custom_root(url, format, route)
        .context("the server URL must be its API's base URL, without a query")?;
    Ok(Connection {
        api,
        base_url: Some(base_url),
        api_key,
        shape,
    })
}

/// The connection to a vendor's own endpoint, which needs a key.
fn vendor_endpoint(
    service: Service,
    api_key: Option<String>,
    key_env: Option<&str>,
    shape: Shape,
) -> Result<Connection> {
    let api = service
        .vendor_api()
        .context("a custom server has no endpoint of its own")?;
    match api_key {
        Some(api_key) => Ok(Connection {
            api,
            base_url: None,
            api_key: Some(api_key),
            shape,
        }),
        None => bail!(
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
    use crate::config::{ApiKey, Config, Format, PolishConfig, Service};

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
                    shape: Default::default(),
                }),
                AsrRoute::Upload(
                    "http://127.0.0.1:8080/inference".to_string(),
                    Default::default()
                ),
                AsrRoute::Upload(
                    "https://asr.example/v1/audio/transcriptions?api-version=1".to_string(),
                    Default::default()
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
                shape: Default::default(),
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
                    shape: Default::default(),
                },
                Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                    api_key: None,
                    shape: Default::default(),
                },
            ]
        );
    }

    #[test]
    fn a_custom_server_speaks_its_format_at_its_base_url() {
        let anthropic = PolishConfig {
            service: Some(Service::Custom),
            format: Some(Format::Anthropic),
            url: "http://127.0.0.1:4000/".to_string(),
            ..PolishConfig::default()
        };
        let mut gemini = config(
            "service = \"custom\"\nformat = \"gemini\"\nurl = \"http://10.0.0.2:9000/v1beta\"",
        );
        gemini.provider.saved_key = Some(ApiKey::new("sk-gemini".to_string()));
        assert_eq!(
            (polish(&anthropic, None).unwrap(), asr(&gemini).unwrap()),
            (
                Connection {
                    api: Api::Anthropic,
                    base_url: Some("http://127.0.0.1:4000".to_string()),
                    api_key: None,
                    shape: Default::default(),
                },
                AsrRoute::Api(Connection {
                    api: Api::Gemini,
                    base_url: Some("http://10.0.0.2:9000/v1beta".to_string()),
                    api_key: Some("sk-gemini".to_string()),
                    shape: Default::default(),
                })
            )
        );
    }

    #[test]
    fn a_provider_s_options_shape_its_connection() {
        let config: PolishConfig = toml::from_str(
            r#"
            service = "custom"
            url = "http://127.0.0.1:8080/v1"

            [options.custom]
            path = "/v2/chat"
            key_header = "api-key"
            body = '{"top_p": 0.9}'
            headers = { X-Org = "limen" }

            [options.openai]
            path = "/elsewhere"
            "#,
        )
        .unwrap();

        assert_eq!(
            polish(&config, None).unwrap().shape,
            crate::backend::Shape {
                path: Some("/v2/chat".to_string()),
                key_header: Some("api-key".to_string()),
                headers: vec![("X-Org".to_string(), "limen".to_string())],
                body: serde_json::json!({"top_p": 0.9})
                    .as_object()
                    .unwrap()
                    .clone(),
            }
        );
    }
}
