use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use limen_core::{Api, Connection, Exchange, Finish, ModelBackend, Reply, Take};

use super::Rig;

/// One request as the mock server read it.
struct Request {
    head: String,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<String> {
        header(&self.head, name)
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        match key.eq_ignore_ascii_case(name) {
            true => Some(value.trim().to_string()),
            false => None,
        }
    })
}

/// A server that answers one request with `reply` and returns the request
/// it read, and its address, which is the base URL to connect to.
async fn mock_server(reply: Value) -> (String, JoinHandle<Request>) {
    answering("200 OK", "", reply).await
}

/// A server that answers one request with `status`, the extra `headers`,
/// and `reply`.
async fn answering(
    status: &'static str,
    headers: &'static str,
    reply: Value,
) -> (String, JoinHandle<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reply = reply.to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut read = Vec::new();
        let mut chunk = [0_u8; 4096];
        let request = loop {
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0, "the client closed before sending a request");
            read.extend_from_slice(&chunk[..count]);
            let Some(end) = read.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&read[..end]).into_owned();
            let length = header(&head, "content-length")
                .map_or(0, |length| length.parse::<usize>().unwrap());
            if read.len() >= end + 4 + length {
                break Request {
                    head,
                    body: read[end + 4..end + 4 + length].to_vec(),
                };
            }
        };
        let response = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        request
    });
    (format!("http://{address}"), task)
}

fn connection(api: Api, base_url: String, api_key: Option<&str>) -> Connection {
    Connection {
        api,
        base_url: Some(base_url),
        api_key: api_key.map(str::to_string),
    }
}

const EXCHANGE: Exchange<'static> = Exchange {
    instructions: "Correct it.",
    message: "原始 文本",
    max_tokens: 4096,
};

fn chat_reply(finish_reason: &str) -> Value {
    json!({
        "id": "reply",
        "object": "chat.completion",
        "model": "corrector",
        "choices": [{
            "index": 0,
            "finish_reason": finish_reason,
            "message": {"role": "assistant", "content": "你好，世界。"}
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

#[tokio::test]
async fn a_keyless_compatible_server_gets_the_take_without_a_credential() {
    let (base, server) = mock_server(json!({"text": "完整文本"})).await;
    let transcriber = Rig::new()
        .unwrap()
        .transcriber(
            connection(Api::Compatible, format!("{base}/v1"), None),
            "asr".to_string(),
        )
        .unwrap();

    let text = transcriber
        .transcribe(Take {
            wav: b"RIFF-test-wav".to_vec(),
            language: Some("zh".to_string()),
        })
        .await
        .unwrap();

    let request = server.await.unwrap();
    let body = String::from_utf8_lossy(&request.body);
    assert_eq!(
        (
            text.as_str(),
            request.head.lines().next(),
            request.header("authorization")
        ),
        (
            "完整文本",
            Some("POST /v1/audio/transcriptions HTTP/1.1"),
            None
        )
    );
    for part in [
        "name=\"model\"\r\n\r\nasr",
        "name=\"language\"\r\n\r\nzh",
        "filename=\"audio.wav\"",
        "RIFF-test-wav",
    ] {
        assert!(body.contains(part), "missing {part:?} in {body}");
    }
}

#[tokio::test]
async fn a_compatible_chat_is_asked_deterministically_and_its_reply_reported() {
    let (base, server) = mock_server(chat_reply("stop")).await;
    let model = Rig::new()
        .unwrap()
        .chat(
            connection(Api::Compatible, format!("{base}/v1"), Some("sk-polish")),
            "corrector".to_string(),
        )
        .unwrap();

    let reply = model.reply(EXCHANGE).await.unwrap();

    let request = server.await.unwrap();
    assert_eq!(
        (
            reply,
            request.head.lines().next(),
            request.header("authorization"),
            request.json()
        ),
        (
            Reply {
                text: "你好，世界。".to_string(),
                finish: Some(Finish::Stop),
            },
            Some("POST /v1/chat/completions HTTP/1.1"),
            Some("Bearer sk-polish".to_string()),
            json!({
                "model": "corrector",
                "messages": [
                    {"role": "system", "content": [{"type": "text", "text": "Correct it."}]},
                    {"role": "user", "content": "原始 文本"},
                ],
                "temperature": 0.0,
                "max_tokens": 4096,
            })
        )
    );
}

#[tokio::test]
async fn a_cut_off_reply_reports_why_it_stopped() {
    let (base, server) = mock_server(chat_reply("length")).await;
    let model = Rig::new()
        .unwrap()
        .chat(
            connection(Api::Compatible, format!("{base}/v1"), None),
            "corrector".to_string(),
        )
        .unwrap();

    let reply = model.reply(EXCHANGE).await.unwrap();

    server.await.unwrap();
    assert_eq!(
        reply,
        Reply {
            text: "你好，世界。".to_string(),
            finish: Some(Finish::Length),
        }
    );
}

#[tokio::test]
async fn deepseek_is_asked_not_to_think() {
    let (base, server) = mock_server(chat_reply("stop")).await;
    let model = Rig::new()
        .unwrap()
        .chat(
            connection(Api::DeepSeek, base, Some("sk-deepseek")),
            "deepseek-flash".to_string(),
        )
        .unwrap();

    model.reply(EXCHANGE).await.unwrap();

    let request = server.await.unwrap();
    assert_eq!(
        (request.head.lines().next(), request.json()),
        (
            Some("POST /chat/completions HTTP/1.1"),
            json!({
                "model": "deepseek-flash",
                "messages": [
                    {"role": "system", "content": [{"type": "text", "text": "Correct it."}]},
                    {"role": "user", "content": "原始 文本"},
                ],
                "temperature": 0.0,
                "max_tokens": 4096,
                "thinking": {"type": "disabled"},
            })
        )
    );
}

#[tokio::test]
async fn a_compatible_server_lists_its_models_from_its_api_root() {
    let (base, server) = mock_server(json!({
        "object": "list",
        "data": [{"id": "b", "object": "model"}, {"id": "a", "object": "model"}]
    }))
    .await;

    let models = Rig::new()
        .unwrap()
        .models(connection(Api::Compatible, format!("{base}/v1"), None))
        .await
        .unwrap();

    let request = server.await.unwrap();
    assert_eq!(
        (models, request.head.lines().next()),
        (
            vec!["b".to_string(), "a".to_string()],
            Some("GET /v1/models HTTP/1.1")
        )
    );
}

#[test]
fn chat_only_vendors_refuse_to_transcribe() {
    let rig = Rig::new().unwrap();
    for api in [Api::DeepSeek, Api::Anthropic] {
        let connection = Connection {
            api,
            base_url: None,
            api_key: Some("sk".to_string()),
        };
        assert!(
            rig.transcriber(connection, "m".to_string()).is_err(),
            "{api:?}"
        );
    }
}

#[test]
fn a_compatible_server_needs_its_url() {
    let connection = Connection {
        api: Api::Compatible,
        base_url: None,
        api_key: None,
    };
    assert!(
        Rig::new()
            .unwrap()
            .chat(connection, "m".to_string())
            .is_err()
    );
}

#[tokio::test]
async fn a_rejected_key_is_never_quoted_back() {
    let (base, server) = answering(
        "401 Unauthorized",
        "",
        json!({"error": {"message": "Invalid API key: sk-secret-123", "code": "invalid_api_key"}}),
    )
    .await;
    let model = Rig::new()
        .unwrap()
        .chat(
            connection(Api::Compatible, format!("{base}/v1"), Some("sk-secret-123")),
            "corrector".to_string(),
        )
        .unwrap();

    let error = model.reply(EXCHANGE).await.unwrap_err();

    server.await.unwrap();
    assert_eq!(
        format!("{error:#}"),
        "the service rejected the API key with HTTP 401"
    );
}

#[tokio::test]
async fn a_failed_reply_names_its_status_but_not_its_body() {
    let (base, server) = answering(
        "500 Internal Server Error",
        "",
        json!({"error": {"message": "upstream said sk-secret-123"}}),
    )
    .await;
    let transcriber = Rig::new()
        .unwrap()
        .transcriber(
            connection(Api::Compatible, format!("{base}/v1"), Some("sk-secret-123")),
            "asr".to_string(),
        )
        .unwrap();

    let error = transcriber
        .transcribe(Take {
            wav: b"RIFF".to_vec(),
            language: None,
        })
        .await
        .unwrap_err();

    server.await.unwrap();
    assert_eq!(format!("{error:#}"), "the service returned HTTP 500");
}

#[tokio::test]
async fn a_redirect_is_never_followed_with_the_key() {
    let elsewhere = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let location: &'static str = Box::leak(
        format!(
            "Location: http://{}/v1/chat/completions\r\n",
            elsewhere.local_addr().unwrap()
        )
        .into_boxed_str(),
    );
    let (base, server) = answering("307 Temporary Redirect", location, json!({})).await;
    let model = Rig::new()
        .unwrap()
        .chat(
            connection(Api::Compatible, format!("{base}/v1"), Some("sk-polish")),
            "corrector".to_string(),
        )
        .unwrap();

    assert!(model.reply(EXCHANGE).await.is_err());
    server.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), elsewhere.accept())
            .await
            .is_err(),
        "the redirect was followed"
    );
}
