//! Changing the ASR server: the config file first, then a running daemon.
//!
//! Users choose a server and one of the known transcription routes rather
//! than typing a whole URL; a hand-edited config may still use any URL.

use anyhow::{Context, Result, bail};

use crate::config::{self, Paths};
use crate::ipc::{self, v1};

/// The transcription route on the ASR server, after its address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// OpenAI-compatible servers, such as vLLM.
    Transcriptions,
    /// The whisper.cpp server.
    Inference,
}

impl Endpoint {
    pub const ALL: [Self; 2] = [Self::Transcriptions, Self::Inference];

    pub fn path(self) -> &'static str {
        match self {
            Self::Transcriptions => "/v1/audio/transcriptions",
            Self::Inference => "/inference",
        }
    }

    /// The route whose path is exactly `path`.
    pub fn from_path(path: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|endpoint| endpoint.path() == path)
    }
}

/// Where a new ASR server took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlChange {
    /// The running daemon uses it from its next take on.
    Applied,
    /// No daemon is running; the next one starts with it.
    Saved,
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

/// Point `provider.url` at `endpoint` on `server`, an http(s) origin such as
/// `http://127.0.0.1:8000`, and have a running daemon apply it.
pub async fn set_server(paths: &Paths, server: &str, endpoint: Endpoint) -> Result<UrlChange> {
    let parsed = reqwest::Url::parse(server)
        .with_context(|| format!("{server:?} is not a server address"))?;
    let origin = parsed.origin().ascii_serialization();
    match (
        parsed.scheme(),
        parsed.as_str().trim_end_matches('/') == origin,
    ) {
        ("http" | "https", true) => {}
        _ => bail!("enter only the server's http(s):// address, such as http://127.0.0.1:8000"),
    }
    // The write waits on a file lock; keep it off the caller's runtime thread.
    let (file, url) = (
        paths.config_file.clone(),
        format!("{origin}{}", endpoint.path()),
    );
    tokio::task::spawn_blocking(move || config::update_provider_url(&file, &url))
        .await
        .context("the config writer stopped")??;
    match ipc::request(paths, &v1::Request::ProviderReload).await {
        Ok(v1::Reply::Ok) => Ok(UrlChange::Applied),
        Ok(v1::Reply::Error { message }) => {
            bail!("saved, but the daemon did not apply it: {message}")
        }
        Ok(v1::Reply::ShortcutStatus(_) | v1::Reply::Accepted { .. }) => {
            bail!("the daemon answered something else")
        }
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => Ok(UrlChange::Saved),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Endpoint, UrlChange, set_server, split_url};
    use crate::config::{Config, Paths};

    const CONFIG: &str =
        "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n";

    /// Paths in a fresh directory holding `CONFIG`, where no
    /// daemon listens.
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

    #[tokio::test]
    async fn the_url_joins_the_server_and_the_chosen_endpoint() {
        let paths = scratch("server-join");

        let change = set_server(&paths, "https://asr.example:9000/", Endpoint::Inference)
            .await
            .unwrap();

        assert_eq!(
            (
                change,
                Config::load(&paths.config_file).unwrap().provider.url
            ),
            (
                UrlChange::Saved,
                "https://asr.example:9000/inference".to_string()
            )
        );
    }

    #[tokio::test]
    async fn only_a_bare_http_server_is_accepted() {
        let paths = scratch("server-invalid");
        for server in [
            "",
            "10.0.0.2:9000",
            "ftp://10.0.0.2",
            "http://10.0.0.2:9000/v1/audio/transcriptions",
            "http://10.0.0.2:9000?x=1",
            "http://user:secret@10.0.0.2",
        ] {
            assert!(
                set_server(&paths, server, Endpoint::Transcriptions)
                    .await
                    .is_err(),
                "{server}"
            );
        }
        assert_eq!(fs::read_to_string(&paths.config_file).unwrap(), CONFIG);
    }
}
