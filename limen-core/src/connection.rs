//! Where each role's requests go: the [`Connection`] its config names, or,
//! for an ASR URL no backend serves, Limen's own upload to that URL.

use anyhow::{Result, bail};

use crate::backend::{Api, Connection};
use crate::config::{Config, PolishConfig};

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

/// The route of the configured ASR server.
pub(crate) fn asr(config: &Config) -> AsrRoute {
    let url = &config.provider.url;
    match base_url(url, TRANSCRIPTIONS) {
        Some(base_url) => AsrRoute::Api(Connection {
            api: Api::Compatible,
            base_url: Some(base_url),
            api_key: config.api_key(),
        }),
        None => AsrRoute::Upload(url.clone()),
    }
}

/// The connection of the configured polishing server, which sends
/// `api_key`.
pub(crate) fn polish(config: &PolishConfig, api_key: Option<String>) -> Result<Connection> {
    match base_url(&config.url, CHAT_COMPLETIONS) {
        Some(base_url) => Ok(Connection {
            api: Api::Compatible,
            base_url: Some(base_url),
            api_key,
        }),
        None => bail!("polish.url must end in {CHAT_COMPLETIONS}, without a query"),
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
