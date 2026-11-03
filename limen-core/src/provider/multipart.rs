//! A transcription server that takes the WAV as a multipart upload at
//! exactly the configured URL, such as the whisper.cpp server's
//! `/inference`, and answers `{"text": …}`.

use anyhow::{Context, Result, bail};

use crate::backend::{BoxFuture, Shape, Take, Transcriber};

pub struct Server {
    http: reqwest::Client,
    url: String,
    model: String,
    api_key: Option<String>,
    shape: Shape,
}

impl Server {
    /// The server at exactly `url`, its requests shaped as `shape` says.
    /// Fails where no HTTP client can be built, such as on a system without
    /// CA certificates, and for a route, which `url` already names.
    pub fn new(url: String, model: String, api_key: Option<String>, shape: Shape) -> Result<Self> {
        if shape.path.is_some() {
            bail!("the whisper.cpp upload goes to exactly its URL; put the route in the URL");
        }
        let http = reqwest::Client::builder()
            .build()
            .context("failed to build the transcription HTTP client")?;
        Ok(Self {
            http,
            url,
            model,
            api_key,
            shape,
        })
    }
}

impl Transcriber for Server {
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let form = reqwest::multipart::Form::new()
                .text("model", self.model.clone())
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(take.wav)
                        .file_name("audio.wav")
                        .mime_str("audio/wav")?,
                );
            let form = match take.language {
                Some(language) => form.text("language", language),
                None => form,
            };
            // The extra body's fields go as form fields, strings as they are.
            let form = self
                .shape
                .body
                .iter()
                .fold(form, |form, (name, value)| match value {
                    serde_json::Value::String(text) => form.text(name.clone(), text.clone()),
                    value => form.text(name.clone(), value.to_string()),
                });
            let request = self.shape.headers.iter().fold(
                self.http.post(&self.url).multipart(form),
                |request, (name, value)| request.header(name, value),
            );
            let request = match (&self.api_key, &self.shape.key_header) {
                (Some(key), Some(header)) => request.header(header, key),
                (Some(key), None) => request.bearer_auth(key),
                (None, _) => request,
            };
            let response = request.send().await.context("transcription request")?;
            // The body is never quoted: it can echo the request or the key.
            if !response.status().is_success() {
                bail!("the server returned HTTP {}", response.status());
            }
            response
                .json::<TranscriptionResponse>()
                .await
                .context("parse response")
                .map(|body| body.text)
        })
    }
}

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use super::Server;
    use crate::backend::{Shape, Take, Transcriber};

    /// A server that answers one request with `status` and `body`, and
    /// returns the request it read.
    async fn mock_server(status: &'static str, body: String) -> (String, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                let complete = request
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .is_some_and(|header_end| {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                match name.eq_ignore_ascii_case("content-length") {
                                    true => value.trim().parse::<usize>().ok(),
                                    false => None,
                                }
                            })
                            .unwrap_or(0);
                        request.len() >= header_end + 4 + content_length
                    });
                if complete {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            request
        });
        (format!("http://{address}/inference"), task)
    }

    fn take(language: Option<&str>) -> Take {
        Take {
            wav: b"RIFF-test-wav".to_vec(),
            language: language.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn the_take_is_uploaded_to_exactly_the_configured_url() {
        let (url, server) = mock_server("200 OK", json!({"text": "完整文本"}).to_string()).await;
        let transcriber = Server::new(
            url,
            "test-model".to_string(),
            Some("sk-asr".to_string()),
            Default::default(),
        )
        .unwrap();

        let text = transcriber.transcribe(take(Some("zh"))).await.unwrap();

        let request = String::from_utf8_lossy(&server.await.unwrap()).into_owned();
        assert_eq!(text, "完整文本");
        assert!(request.starts_with("POST /inference "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-asr")
        );
        for part in [
            "name=\"model\"\r\n\r\ntest-model",
            "name=\"language\"\r\n\r\nzh",
            "filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF-test-wav",
        ] {
            assert!(request.contains(part), "missing {part:?} in {request}");
        }
    }

    #[tokio::test]
    async fn no_key_and_automatic_language_send_neither() {
        let (url, server) = mock_server("200 OK", json!({"text": ""}).to_string()).await;
        let transcriber =
            Server::new(url, "test-model".to_string(), None, Default::default()).unwrap();

        transcriber.transcribe(take(None)).await.unwrap();

        let request = String::from_utf8_lossy(&server.await.unwrap()).to_ascii_lowercase();
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("name=\"language\""));
    }

    #[tokio::test]
    async fn the_shape_adds_headers_fields_and_the_key_header() {
        let (url, server) = mock_server("200 OK", json!({"text": ""}).to_string()).await;
        let shape = Shape {
            key_header: Some("api-key".to_string()),
            headers: vec![("X-Org".to_string(), "limen".to_string())],
            body: json!({"temperature": 0, "prompt": "names"})
                .as_object()
                .unwrap()
                .clone(),
            ..Shape::default()
        };
        let transcriber = Server::new(
            url,
            "test-model".to_string(),
            Some("sk-asr".to_string()),
            shape,
        )
        .unwrap();

        transcriber.transcribe(take(None)).await.unwrap();

        let request = String::from_utf8_lossy(&server.await.unwrap()).into_owned();
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("authorization:"), "{request}");
        for part in [
            "api-key: sk-asr",
            "x-org: limen",
            "name=\"temperature\"\r\n\r\n0",
            "name=\"prompt\"\r\n\r\nnames",
        ] {
            assert!(lower.contains(part), "missing {part:?} in {request}");
        }
    }

    #[test]
    fn a_route_is_refused_for_an_exact_upload_url() {
        let shape = Shape {
            path: Some("/v2".to_string()),
            ..Shape::default()
        };
        assert!(
            Server::new(
                "http://127.0.0.1:9/inference".to_string(),
                "m".to_string(),
                None,
                shape
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn http_and_json_errors_fail_the_take() {
        for (status, body) in [
            ("500 Internal Server Error", "not-json"),
            ("200 OK", "not-json"),
        ] {
            let (url, server) = mock_server(status, body.to_string()).await;
            let transcriber =
                Server::new(url, "test-model".to_string(), None, Default::default()).unwrap();

            assert!(
                transcriber.transcribe(take(None)).await.is_err(),
                "{status}"
            );
            server.await.unwrap();
        }
    }
}
