//! Buffered fallback provider: accumulate one take, POST it as a wav to
//! `<url>` (an OpenAI-compatible `/v1/audio/transcriptions`), emit a single
//! Stable event with the returned text.

use std::io::Cursor;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::config::ProviderConfig;
use crate::provider::{
    AsrProvider, AsrSession, AudioSink, EventSource, SessionConfig, TranscriptEvent,
};

pub struct Provider {
    cfg: ProviderConfig,
    client: reqwest::Client,
}

impl Provider {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl AsrProvider for Provider {
    async fn start_session(&self, cfg: &SessionConfig) -> Result<Box<dyn AsrSession>> {
        let (tx, rx) = mpsc::channel(8);
        Ok(Box::new(Session {
            sink_cfg: cfg.clone(),
            http: self.client.clone(),
            url: self.cfg.url.clone(),
            pcm: Vec::new(),
            events: tx,
            rx: Some(rx),
        }))
    }
}

struct Session {
    sink_cfg: SessionConfig,
    http: reqwest::Client,
    url: String,
    pcm: Vec<i16>,
    events: mpsc::Sender<TranscriptEvent>,
    rx: Option<mpsc::Receiver<TranscriptEvent>>,
}

impl AsrSession for Session {
    fn split(self: Box<Self>) -> (Box<dyn AudioSink>, Box<dyn EventSource>) {
        let rx = self.rx.expect("session split twice");
        (
            Box::new(Sink {
                pcm: self.pcm,
                http: self.http,
                url: self.url,
                cfg: self.sink_cfg,
                events: self.events,
            }),
            Box::new(Source { rx }),
        )
    }
}

struct Sink {
    pcm: Vec<i16>,
    http: reqwest::Client,
    url: String,
    cfg: SessionConfig,
    events: mpsc::Sender<TranscriptEvent>,
}

#[async_trait]
impl AudioSink for Sink {
    async fn send_audio(&mut self, pcm: &[i16]) -> Result<()> {
        self.pcm.extend_from_slice(pcm);
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        let wav = encode_wav(&self.pcm, self.cfg.sample_rate)?;
        let mut form = reqwest::multipart::Form::new()
            .text("model", self.cfg.model.clone())
            .part(
                "file",
                reqwest::multipart::Part::bytes(wav)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            );
        if self.cfg.language != "auto" {
            form = form.text("language", self.cfg.language.clone());
        }
        let mut req = self.http.post(&self.url).multipart(form);
        if let Some(key) = &self.cfg.api_key {
            req = req.bearer_auth(key);
        }
        let outcome = match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp
                .json::<TranscriptionResponse>()
                .await
            {
                Ok(body) => TranscriptEvent::Stable(body.text),
                Err(e) => TranscriptEvent::Failed(anyhow::Error::new(e).context("parse response")),
            },
            Ok(resp) => TranscriptEvent::Failed(anyhow::anyhow!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )),
            Err(e) => {
                TranscriptEvent::Failed(anyhow::Error::new(e).context("transcription request"))
            }
        };
        // Best-effort delivery; a dead receiver means the caller went away.
        let is_ok = !matches!(outcome, TranscriptEvent::Failed(_));
        let _ = self.events.send(outcome).await;
        if is_ok {
            // The trait promises Completed once the stable tail is delivered.
            let _ = self.events.send(TranscriptEvent::Completed).await;
        }
        Ok(())
    }
}

struct Source {
    rx: mpsc::Receiver<TranscriptEvent>,
}

#[async_trait]
impl EventSource for Source {
    async fn next_event(&mut self) -> Option<TranscriptEvent> {
        self.rx.recv().await
    }
}

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Wrap raw s16le samples in a minimal WAV container.
fn encode_wav(pcm: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).context("wav writer")?;
        for &s in pcm {
            writer.write_sample(s)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}
