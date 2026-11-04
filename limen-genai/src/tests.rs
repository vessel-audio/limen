use std::sync::Arc;

use limen_core::{
    Api, Connection, Exchange, Finish, ModelBackend, PriceList, Reply, Shape, Transient, Usage,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::Genai;

/// A price list that charges a cent per thousand tokens of every model.
struct Flat;

impl PriceList for Flat {
    fn price(&self, _: Api, _: &str, usage: &Usage) -> Option<f64> {
        let tokens = usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
        Some(tokens as f64 / 100_000.)
    }
}

fn genai() -> Genai {
    Genai::new(Arc::new(Flat))
}

/// One request as the mock server read it.
struct Request {
    head: String,
    body: Vec<u8>,
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// A server that answers one request with `status` and `reply`, and
/// returns the request it read; its address is the base URL to use.
async fn answering(status: &'static str, reply: Value) -> (String, JoinHandle<Request>) {
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
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        request
    });
    (format!("http://{address}/v1"), task)
}

fn together(base_url: String, shape: Shape) -> Connection {
    Connection {
        api: Api::Together,
        base_url: Some(base_url),
        api_key: Some("sk-together".to_string()),
        shape,
        proxy: Default::default(),
    }
}

const EXCHANGE: Exchange<'static> = Exchange {
    instructions: "Correct it.",
    message: "原始 文本",
    max_tokens: 4096,
};

#[tokio::test]
async fn a_reply_is_sent_as_asked_and_priced_from_the_list() {
    let (base, server) = answering(
        "200 OK",
        json!({
            "id": "r",
            "object": "chat.completion",
            "model": "m",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "原始文本。"}
            }],
            "usage": {"prompt_tokens": 600, "completion_tokens": 400, "total_tokens": 1000}
        }),
    )
    .await;
    let shape = Shape {
        headers: vec![("X-Team".to_string(), "limen".to_string())],
        body: serde_json::from_value(json!({"top_k": 1})).unwrap(),
        ..Shape::default()
    };
    let reply = genai()
        .chat(together(base, shape), "m".to_string())
        .unwrap()
        .reply(EXCHANGE)
        .await
        .unwrap();
    let request = server.await.unwrap();
    let body: Value = serde_json::from_slice(&request.body).unwrap();

    assert_eq!(
        (
            reply,
            request.head.lines().next(),
            header(&request.head, "authorization"),
            header(&request.head, "x-team"),
            [
                &body["temperature"],
                &body["max_tokens"],
                &body["top_k"],
                &body["messages"][0]["role"],
                &body["messages"][1]["content"],
            ],
        ),
        (
            Reply {
                text: "原始文本。".to_string(),
                finish: Some(Finish::Stop),
                usage: Usage {
                    input_tokens: Some(600),
                    cached_input_tokens: None,
                    output_tokens: Some(400),
                    cost_usd: Some(0.01),
                },
            },
            Some("POST /v1/chat/completions HTTP/1.1"),
            Some("Bearer sk-together".to_string()),
            Some("limen".to_string()),
            [
                &json!(0.0),
                &json!(4096),
                &json!(1),
                &json!("system"),
                &json!("原始 文本"),
            ],
        )
    );
}

#[tokio::test]
async fn a_busy_service_may_be_tried_again_and_its_reply_is_never_quoted() {
    for (status, transient) in [
        ("503 Service Unavailable", true),
        ("401 Unauthorized", false),
    ] {
        let (base, server) = answering(status, json!({"error": "bad key sk-together"})).await;
        let error = genai()
            .chat(together(base, Shape::default()), "m".to_string())
            .unwrap()
            .reply(EXCHANGE)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(
            (
                error.chain().any(|cause| cause.is::<Transient>()),
                format!("{error:#}").contains("sk-together"),
            ),
            (transient, false),
            "{status}: {error:#}"
        );
    }
}

#[test]
fn a_key_in_a_header_of_its_own_and_a_rig_api_are_refused() {
    let named = Shape {
        key_header: Some("X-Key".to_string()),
        ..Shape::default()
    };
    let refused = [
        together("http://127.0.0.1:9/v1".to_string(), named),
        Connection {
            api: Api::OpenAi,
            ..together("http://127.0.0.1:9/v1".to_string(), Shape::default())
        },
    ]
    .map(|connection| {
        genai()
            .chat(connection, "m".to_string())
            .err()
            .map(|error| error.to_string())
    });
    assert_eq!(
        refused,
        [
            Some("Together takes its key only in its own header".to_string()),
            Some("OpenAi is not reached through genai".to_string()),
        ]
    );
}
