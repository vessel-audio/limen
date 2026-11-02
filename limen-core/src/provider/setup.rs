//! The ASR server's address: a server and the transcription route its
//! service uses, rather than a whole URL; a hand-edited config may still
//! use any URL.

use anyhow::{Result, bail};

use crate::config::Service;

/// The transcription route on the ASR server, after its address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// OpenAI-compatible servers, such as vLLM.
    Transcriptions,
    /// The whisper.cpp server.
    Inference,
}

impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Self::Transcriptions => "/v1/audio/transcriptions",
            Self::Inference => "/inference",
        }
    }

    /// The route `service` takes on its server; `None` for a vendor's own
    /// API, which has no server to name.
    pub fn of(service: Service) -> Option<Self> {
        match service {
            Service::OpenAiCompatible => Some(Self::Transcriptions),
            Service::WhisperCpp => Some(Self::Inference),
            Service::OpenAi
            | Service::Anthropic
            | Service::Gemini
            | Service::DeepSeek
            | Service::Mistral
            | Service::Groq
            | Service::OpenRouter
            | Service::XAi => None,
        }
    }
}

/// Split a provider URL into its server and the rest, e.g.
/// `http://127.0.0.1:8000` and `/v1/audio/transcriptions`. `None` when the
/// URL names no host.
pub fn split_url(url: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(url).ok()?;
    url.has_host().then(|| {
        let query = url
            .query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default();
        (
            url.origin().ascii_serialization(),
            format!("{}{query}", url.path()),
        )
    })
}

/// The transcription URL of `endpoint` on `server`, an http(s) origin such
/// as `http://127.0.0.1:8000`.
pub fn server_url(server: &str, endpoint: Endpoint) -> Result<String> {
    server_url_with_route(server, endpoint.path())
}

/// `route`, including any query, on `server`. Used for a hand-edited route
/// that the known endpoint list does not offer.
pub fn server_url_with_route(server: &str, route: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(server)
        .map_err(|_| anyhow::anyhow!("enter a valid http(s):// server address"))?;
    let origin = parsed.origin().ascii_serialization();
    match (
        parsed.scheme(),
        parsed.as_str().trim_end_matches('/') == origin,
    ) {
        ("http" | "https", true) if route.starts_with('/') => Ok(format!("{origin}{route}")),
        ("http" | "https", true) => bail!("a provider route must start with /"),
        _ => bail!("enter only the server's http(s):// address, such as http://127.0.0.1:8000"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Endpoint, server_url, split_url};

    #[test]
    fn urls_split_into_server_and_route() {
        assert_eq!(
            [
                "http://127.0.0.1:8000/v1/audio/transcriptions",
                "https://asr.example:443/inference?lang=zh",
                "http://",
            ]
            .map(split_url),
            [
                Some((
                    "http://127.0.0.1:8000".to_string(),
                    "/v1/audio/transcriptions".to_string()
                )),
                Some((
                    "https://asr.example".to_string(),
                    "/inference?lang=zh".to_string()
                )),
                None,
            ]
        );
    }

    #[test]
    fn the_url_joins_the_server_and_the_chosen_endpoint() {
        assert_eq!(
            server_url("https://asr.example:9000/", Endpoint::Inference).unwrap(),
            "https://asr.example:9000/inference"
        );
    }

    #[test]
    fn a_custom_route_moves_to_a_new_server() {
        assert_eq!(
            super::server_url_with_route(
                "https://new.example:9000/",
                "/custom-transcribe?api-version=1"
            )
            .unwrap(),
            "https://new.example:9000/custom-transcribe?api-version=1"
        );
        assert!(super::server_url_with_route("https://new.example", "custom").is_err());
    }

    #[test]
    fn only_a_bare_http_server_is_accepted() {
        for server in [
            "",
            "10.0.0.2:9000",
            "ftp://10.0.0.2",
            "http://10.0.0.2:9000/v1/audio/transcriptions",
            "http://10.0.0.2:9000?x=1",
            "http://user:secret@10.0.0.2",
        ] {
            assert!(
                server_url(server, Endpoint::Transcriptions).is_err(),
                "{server}"
            );
        }
    }
}
