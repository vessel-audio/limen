use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use limen_core::{
    Api, ChatApi, Connection, Exchange, Finish, ModelBackend, Reply, Shape, Subscription, Take,
    Usage,
};

use super::Rig;

/// The backend under test; no test signs in, so its sign-ins stay empty.
fn rig() -> Rig {
    Rig::new(std::env::temp_dir().join("limen-rig-tests-sign-ins"))
}

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
        shape: Shape::default(),
        proxy: Default::default(),
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
    let transcriber = rig()
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
            text.text.as_str(),
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
    let model = rig()
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
                usage: Usage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    ..Usage::default()
                },
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
    let model = rig()
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
            usage: Usage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                ..Usage::default()
            },
        }
    );
}

#[tokio::test]
async fn an_unknown_finish_reason_keeps_none_of_its_text() {
    let (base, server) = mock_server(chat_reply("sk-secret-in-the-reason")).await;
    let model = rig()
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
            finish: Some(Finish::Other),
            usage: Usage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                ..Usage::default()
            },
        }
    );
}

#[tokio::test]
async fn deepseek_is_asked_not_to_think() {
    let (base, server) = mock_server(chat_reply("stop")).await;
    let model = rig()
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

    let models = rig()
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
    let rig = rig();
    for api in [Api::DeepSeek, Api::Anthropic] {
        let connection = Connection {
            api,
            base_url: None,
            api_key: Some("sk".to_string()),
            shape: Shape::default(),
            proxy: Default::default(),
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
        shape: Shape::default(),
        proxy: Default::default(),
    };
    assert!(rig().chat(connection, "m".to_string()).is_err());
}

#[tokio::test]
async fn a_rejected_key_is_never_quoted_back() {
    let (base, server) = answering(
        "401 Unauthorized",
        "",
        json!({"error": {"message": "Invalid API key: sk-secret-123", "code": "invalid_api_key"}}),
    )
    .await;
    let model = rig()
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
    let transcriber = rig()
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
    let model = rig()
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

#[tokio::test]
async fn an_unreachable_service_never_quotes_the_key_in_its_url() {
    // Nothing listens on the discard port; Gemini sends its key in the URL.
    let model = rig()
        .chat(
            connection(
                Api::Gemini,
                "http://127.0.0.1:9".to_string(),
                Some("sk-secret-123"),
            ),
            "gemini-model".to_string(),
        )
        .unwrap();

    let error = format!("{:#}", model.reply(EXCHANGE).await.unwrap_err());

    assert!(!error.contains("sk-secret-123"), "{error}");
}

#[tokio::test]
async fn a_keyless_anthropic_server_gets_no_credential_header() {
    let (base, server) = mock_server(json!({
        "id": "reply",
        "type": "message",
        "role": "assistant",
        "model": "corrector",
        "content": [{"type": "text", "text": "你好，世界。"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    }))
    .await;
    let model = rig()
        .chat(
            connection(Api::Anthropic, base, None),
            "corrector".to_string(),
        )
        .unwrap();

    let reply = model.reply(EXCHANGE).await.unwrap();

    let request = server.await.unwrap();
    assert_eq!(
        (
            reply,
            request.head.lines().next(),
            request.header("x-api-key"),
            request.header("authorization")
        ),
        (
            Reply {
                text: "你好，世界。".to_string(),
                finish: Some(Finish::Stop),
                usage: Usage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    ..Usage::default()
                },
            },
            Some("POST /v1/messages HTTP/1.1"),
            None,
            None
        )
    );
}

#[test]
fn a_keyless_gemini_server_is_refused() {
    let error = rig()
        .chat(
            connection(Api::Gemini, "http://127.0.0.1:9".to_string(), None),
            "gemini-model".to_string(),
        )
        .err()
        .map(|error| error.to_string());

    assert_eq!(error, Some("a Gemini server needs an API key".to_string()));
}

#[tokio::test]
async fn the_shape_moves_the_key_and_body_and_adds_headers() {
    let (base, server) = mock_server(chat_reply("stop")).await;
    let shaped = Connection {
        shape: Shape {
            chat_api: None,
            key_header: Some("api-key".to_string()),
            headers: vec![("X-Org".to_string(), "limen".to_string())],
            body: json!({"top_p": 0.9}).as_object().unwrap().clone(),
        },
        ..connection(Api::Compatible, format!("{base}/v1"), Some("sk-polish"))
    };
    let model = rig().chat(shaped, "corrector".to_string()).unwrap();

    model.reply(EXCHANGE).await.unwrap();

    let request = server.await.unwrap();
    assert_eq!(
        (
            request.head.lines().next(),
            request.header("authorization"),
            request.header("api-key"),
            request.header("x-org"),
            request.json()["top_p"].clone()
        ),
        (
            Some("POST /v1/chat/completions HTTP/1.1"),
            None,
            Some("sk-polish".to_string()),
            Some("limen".to_string()),
            json!(0.9)
        )
    );
}

#[tokio::test]
async fn the_chat_api_chosen_is_the_one_asked() {
    let mut routes = Vec::new();
    for (api, chat_api) in [
        (Api::OpenAi, ChatApi::ChatCompletions),
        (Api::Compatible, ChatApi::Responses),
    ] {
        let (base, server) = mock_server(chat_reply("stop")).await;
        let shaped = Connection {
            shape: Shape {
                chat_api: Some(chat_api),
                ..Shape::default()
            },
            ..connection(api, format!("{base}/v1"), Some("sk-polish"))
        };
        // Only the route matters: a Chat Completions reply is no Responses one.
        let _ = rig()
            .chat(shaped, "corrector".to_string())
            .unwrap()
            .reply(EXCHANGE)
            .await;
        routes.push(
            server
                .await
                .unwrap()
                .head
                .lines()
                .next()
                .map(str::to_string),
        );
    }

    assert_eq!(
        routes,
        [
            Some("POST /v1/chat/completions HTTP/1.1".to_string()),
            Some("POST /v1/responses HTTP/1.1".to_string()),
        ]
    );
}

#[test]
fn a_chat_api_or_key_header_the_api_cannot_take_is_refused() {
    let shaped = |api, shape| Connection {
        shape,
        ..connection(api, "http://127.0.0.1:9".to_string(), Some("sk"))
    };
    let refused = [
        shaped(
            Api::Anthropic,
            Shape {
                chat_api: Some(ChatApi::Responses),
                ..Shape::default()
            },
        ),
        shaped(
            Api::Gemini,
            Shape {
                key_header: Some("api-key".to_string()),
                ..Shape::default()
            },
        ),
    ]
    .map(|connection| {
        rig()
            .chat(connection, "m".to_string())
            .err()
            .map(|error| error.to_string())
    });

    assert_eq!(
        refused,
        [
            Some("Anthropic has one chat API; only OpenAI's APIs offer a choice".to_string()),
            Some("Gemini sends its key in the URL, not in a header".to_string()),
        ]
    );
}

#[tokio::test]
async fn only_failures_that_may_pass_are_marked_transient() {
    let mut transient = Vec::new();
    for status in [
        "503 Service Unavailable",
        "429 Too Many Requests",
        "401 Unauthorized",
        "400 Bad Request",
    ] {
        let (base, server) = answering(status, "", json!({"error": {"message": "no"}})).await;
        let model = rig()
            .chat(
                connection(Api::Compatible, format!("{base}/v1"), Some("sk")),
                "corrector".to_string(),
            )
            .unwrap();
        let error = model.reply(EXCHANGE).await.unwrap_err();
        server.await.unwrap();
        transient.push(error.is::<limen_core::Transient>());
    }
    let unreachable = rig()
        .chat(
            connection(Api::Compatible, "http://127.0.0.1:9/v1".to_string(), None),
            "corrector".to_string(),
        )
        .unwrap()
        .reply(EXCHANGE)
        .await
        .unwrap_err()
        .is::<limen_core::Transient>();

    assert_eq!(
        (transient, unreachable),
        (vec![true, true, false, false], true)
    );
}

#[tokio::test]
async fn a_subscription_never_signed_in_to_is_refused_without_asking() {
    let empty = std::env::temp_dir().join(format!("limen-rig-no-sign-ins-{}", std::process::id()));
    let backend = Rig::new(empty);
    let signed_out = Connection {
        api: Api::ChatGpt,
        base_url: None,
        api_key: None,
        shape: Shape::default(),
        proxy: Default::default(),
    };

    let error = backend
        .chat(signed_out, "gpt-5.5".to_string())
        .unwrap()
        .reply(EXCHANGE)
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(
        (error, backend.signed_in(Subscription::ChatGpt)),
        (
            "not signed in to ChatGPT, or the sign-in expired: sign in on the Polish page"
                .to_string(),
            false
        )
    );
}
