//! An OpenAI-compatible `chat/completions` endpoint at exactly the
//! configured URL.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use crate::backend::{BoxFuture, ChatModel, Exchange, Finish, Reply};

pub struct Endpoint {
    http: reqwest::Client,
    url: String,
    model: String,
    api_key: String,
}

impl Endpoint {
    pub fn new(url: String, model: String, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build the polishing HTTP client")?;
        Ok(Self {
            http,
            url,
            model,
            api_key,
        })
    }
}

impl ChatModel for Endpoint {
    fn reply<'a>(&'a self, exchange: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
        Box::pin(async move {
            let response = self
                .http
                .post(&self.url)
                .bearer_auth(&self.api_key)
                .json(&json!({
                    "model": self.model,
                    "messages": [
                        {"role": "system", "content": exchange.instructions},
                        {"role": "user", "content": exchange.message},
                    ],
                    "thinking": {"type": "disabled"},
                    "temperature": 0,
                    "max_tokens": exchange.max_tokens,
                    "response_format": {"type": "text"},
                    "stream": false,
                }))
                .send()
                .await
                .context("the request failed")?;
            let status = response.status();
            if !status.is_success() {
                bail!("the server returned HTTP {status}");
            }
            let body = response
                .json::<ChatResponse>()
                .await
                .context("failed to parse the response")?;
            let choice = body
                .choices
                .into_iter()
                .next()
                .context("the server returned no choices")?;
            Ok(Reply {
                text: choice.message.content.unwrap_or_default(),
                finish: choice.finish_reason.map(|reason| match reason.as_str() {
                    "stop" => Finish::Stop,
                    "length" => Finish::Length,
                    "content_filter" => Finish::ContentFilter,
                    "tool_calls" => Finish::ToolCalls,
                    other => Finish::Other(other.to_string()),
                }),
            })
        })
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    finish_reason: Option<String>,
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    use super::Endpoint;
    use crate::backend::{ChatModel, Exchange, Finish, Reply};

    async fn mock_server(status: &'static str, body: String) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_handle = tokio::spawn(async move {
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
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}/chat/completions"), request_handle)
    }

    fn endpoint(url: String) -> Endpoint {
        Endpoint::new(url, "test-model".to_string(), "test-key".to_string()).unwrap()
    }

    const EXCHANGE: Exchange<'static> = Exchange {
        instructions: "Correct it.",
        message: "原始 文本",
        max_tokens: 4096,
    };

    #[tokio::test]
    async fn the_reply_and_its_request_contract() {
        let response = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "你好，世界。"}
            }]
        })
        .to_string();
        let (url, server) = mock_server("200 OK", response).await;

        let reply = endpoint(url).reply(EXCHANGE).await.unwrap();

        let request = server.await.unwrap();
        assert!(request.starts_with("POST /chat/completions"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
        );
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            (reply, serde_json::from_str::<Value>(body).unwrap()),
            (
                Reply {
                    text: "你好，世界。".to_string(),
                    finish: Some(Finish::Stop),
                },
                json!({
                    "model": "test-model",
                    "messages": [
                        {"role": "system", "content": "Correct it."},
                        {"role": "user", "content": "原始 文本"},
                    ],
                    "thinking": {"type": "disabled"},
                    "temperature": 0,
                    "max_tokens": 4096,
                    "response_format": {"type": "text"},
                    "stream": false,
                })
            )
        );
    }

    #[tokio::test]
    async fn finish_reasons_and_missing_content_are_reported_as_sent() {
        let (url, server) = mock_server(
            "200 OK",
            json!({"choices": [{"finish_reason": "content_filter", "message": {}}]}).to_string(),
        )
        .await;

        let reply = endpoint(url).reply(EXCHANGE).await.unwrap();

        server.await.unwrap();
        assert_eq!(
            reply,
            Reply {
                text: String::new(),
                finish: Some(Finish::ContentFilter),
            }
        );
    }

    #[tokio::test]
    async fn http_json_and_empty_choice_errors_fail_the_request() {
        for (status, body) in [
            ("401 Unauthorized", "{}"),
            ("500 Internal Server Error", "not-json"),
            ("200 OK", r#"{"choices": []}"#),
        ] {
            let (url, server) = mock_server(status, body.to_string()).await;
            assert!(
                endpoint(url).reply(EXCHANGE).await.is_err(),
                "{status} {body}"
            );
            server.await.unwrap();
        }
    }
}
