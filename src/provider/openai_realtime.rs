//! Streaming provider speaking the OpenAI Realtime-flavored protocol that
//! sglang-omni serves at `/v1/realtime?intent=transcription`.
//!
//! Wire flow (verified against sglang_omni/serve/realtime/{schema,events}.py):
//! connect -> `session.update` (language, turn_detection) ->
//! `input_audio_buffer.append` chunks carrying `sglang.{seq,t_start_ms}` ->
//! `input_audio_buffer.commit` on finalize. Server answers with
//! `transcription.segment` (is_final=false => provisional, true => stable).
//!
//! Note: every client event requires a non-empty `event_id`, and append
//! events require the `sglang` metadata block — both differ from the upstream
//! OpenAI shape, which is why this is its own provider module.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::config::ProviderConfig;
use crate::provider::{AsrProvider, AsrSession, AudioSink, EventSource, SessionConfig, TranscriptEvent};

pub struct Provider {
    cfg: ProviderConfig,
}

impl Provider {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self { cfg }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The realtime endpoint multiplexes full-duplex "conversation" sessions and
/// ASR-only "transcription" sessions; we always want the latter.
fn transcription_url(base: &str) -> Result<String> {
    let (path, query) = base.split_once('?').unwrap_or((base, ""));
    if query.split('&').any(|kv| kv.starts_with("intent=")) {
        return Ok(base.to_string());
    }
    Ok(if query.is_empty() {
        format!("{path}?intent=transcription")
    } else {
        format!("{path}?intent=transcription&{query}")
    })
}

#[async_trait]
impl AsrProvider for Provider {
    async fn start_session(&self, cfg: &SessionConfig) -> Result<Box<dyn AsrSession>> {
        let url = transcription_url(&self.cfg.url)?;
        let mut request = url
            .as_str()
            .into_client_request()
            .with_context(|| format!("bad realtime url: {url}"))?;
        if let Some(key) = &cfg.api_key {
            let value = format!("Bearer {key}").parse().context("api key header")?;
            request.headers_mut().insert("Authorization", value);
        }
        let (ws, _resp) = connect_async(request)
            .await
            .with_context(|| format!("realtime connect failed: {url}"))?;
        let (ws_sink, ws_stream) = ws.split();

        let mut sink = Sink {
            ws: ws_sink,
            audio_seq: 0,
            elapsed_ms: 0.0,
            sample_rate: cfg.sample_rate,
        };
        sink.send_json(&session_update(cfg)).await?;
        Ok(Box::new(Session {
            sink,
            source: Source { ws: ws_stream },
        }))
    }
}

fn session_update(cfg: &SessionConfig) -> Value {
    let mut session = json!({
        "input_audio_format": "pcm16",
        "turn_detection": {"type": "server_vad"},
    });
    // "auto" means model-side detection; only pin a language when asked.
    if cfg.language != "auto" {
        session["language"] = json!(cfg.language);
    }
    json!({
        "type": "session.update",
        "event_id": next_event_id(),
        "session": session,
    })
}

fn next_event_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("evt-{:06}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

struct Session {
    sink: Sink,
    source: Source,
}

impl AsrSession for Session {
    fn split(self: Box<Self>) -> (Box<dyn AudioSink>, Box<dyn EventSource>) {
        (Box::new(self.sink), Box::new(self.source))
    }
}

struct Sink {
    ws: SplitSink<Ws, Message>,
    audio_seq: u64,
    elapsed_ms: f64,
    sample_rate: u32,
}

impl Sink {
    async fn send_json(&mut self, value: &Value) -> Result<()> {
        self.ws
            .send(Message::Text(value.to_string().into()))
            .await
            .context("realtime send")
    }
}

#[async_trait]
impl AudioSink for Sink {
    async fn send_audio(&mut self, pcm: &[i16]) -> Result<()> {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let event = json!({
            "type": "input_audio_buffer.append",
            "event_id": next_event_id(),
            "audio": B64.encode(bytes),
            "sglang": {"seq": self.audio_seq, "t_start_ms": self.elapsed_ms},
        });
        self.audio_seq += 1;
        self.elapsed_ms += pcm.len() as f64 / self.sample_rate as f64 * 1000.0;
        self.send_json(&event).await
    }

    async fn finalize(&mut self) -> Result<()> {
        // `commit` flushes the buffered audio; `transcription.done` ends the
        // session, after which the server emits `transcription.completed`.
        // (`sglang.input_audio.end` exists in the schema but the transcription
        // runtime rejects it, so don't send it.)
        self.send_json(&json!({
            "type": "input_audio_buffer.commit",
            "event_id": next_event_id(),
        }))
        .await?;
        self.send_json(&json!({
            "type": "transcription.done",
            "event_id": next_event_id(),
        }))
        .await
    }
}

struct Source {
    ws: SplitStream<Ws>,
}

#[async_trait]
impl EventSource for Source {
    async fn next_event(&mut self) -> Option<TranscriptEvent> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => return Some(TranscriptEvent::Failed(e.into())),
                    };
                    match value.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "transcription.segment" => {
                            let text = value
                                .get("text")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_final = value
                                .get("is_final")
                                .and_then(|f| f.as_bool())
                                .unwrap_or(false);
                            return Some(if is_final {
                                TranscriptEvent::Stable(text)
                            } else {
                                TranscriptEvent::Provisional(text)
                            });
                        }
                        "error" => {
                            return Some(TranscriptEvent::Failed(anyhow::anyhow!(
                                "server error event: {value}"
                            )));
                        }
                        // session.created/updated, speech_started/stopped,
                        // committed: log for the protocol notes, keep reading.
                        "transcription.completed" => return Some(TranscriptEvent::Completed),
                        other => {
                            tracing::debug!(%other, %value, "realtime event");
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => return None,
                Some(Ok(_)) => {} // ping/pong/binary: ignore
                Some(Err(e)) => {
                    return Some(TranscriptEvent::Failed(
                        anyhow::Error::new(e).context("realtime receive"),
                    ));
                }
            }
        }
    }
}
