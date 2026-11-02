//! ASR session abstraction for buffered, whole-take transcription.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use crate::backend::{BoxFuture, ModelBackend, Transcriber};
use crate::config::Config;
use crate::connection::{self, AsrRoute};

mod buffered;
mod multipart;
mod setup;

pub use setup::{Endpoint, server_url, server_url_with_route, split_url};

/// Static parameters of one ASR session; the model and key are bound when
/// the provider is built.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub language: String,
    pub sample_rate: u32,
}

/// Transcription events coming back from a provider.
#[derive(Debug)]
pub enum TranscriptEvent {
    /// Final text of the take; handed to the input method.
    Stable(String),
    /// The finalized session delivered its final text.
    Completed,
    Failed(anyhow::Error),
}

/// Factory for independent ASR sessions.
pub trait AsrProvider: Send + Sync {
    fn start_session<'a>(
        &'a self,
        cfg: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn AsrSession>>>;
}

/// One live session. Splits into an audio sink and an event source so the
/// capture task and the text input task can run concurrently.
pub trait AsrSession: Send {
    fn split(self: Box<Self>) -> (Box<dyn AudioSink>, Box<dyn EventSource>);
}

/// Buffered audio input for one ASR session.
pub trait AudioSink: Send {
    /// Push one chunk of 16kHz mono s16le audio.
    fn send_audio<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<()>>;
    /// Finish the audio and start transcription; results arrive on EventSource.
    fn finalize(&mut self) -> BoxFuture<'_, Result<()>>;
}

/// Final transcription results and terminal status for one session.
pub trait EventSource: Send {
    /// Next transcript event; `None` means the session is over.
    ///
    /// Must be cancel-safe: the session actor polls it inside `select!`, so
    /// dropping the future before it completes must not lose an event.
    fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>>;
}

/// Build the buffered transcription provider for the configured service:
/// through `backend`, unless it is Limen's own upload.
pub fn from_config(cfg: &Config, backend: &dyn ModelBackend) -> Result<Box<dyn AsrProvider>> {
    let model = cfg.provider.model.clone();
    let transcriber: Arc<dyn Transcriber> = match connection::asr(cfg)? {
        AsrRoute::Upload(url) => Arc::new(multipart::Server::new(url, model, cfg.api_key())),
        AsrRoute::Api(connection) => Arc::from(backend.transcriber(connection, model)?),
    };
    Ok(Box::new(buffered::Provider::new(transcriber)))
}

/// `limen probe <wav>`: transcribe a file and print the final event flow.
pub async fn probe(cfg: &Config, backend: &dyn ModelBackend, wav: &Path) -> Result<()> {
    let pcm = load_wav_16k_mono(wav)?;
    let session_cfg = SessionConfig {
        language: cfg.provider.language.clone(),
        sample_rate: 16_000,
    };
    let session = from_config(cfg, backend)?
        .start_session(&session_cfg)
        .await?;
    let (mut sink, mut events) = session.split();

    let started = Instant::now();
    let chunk = (16_000 * cfg.audio.chunk_ms / 1000) as usize;
    for frame in pcm.chunks(chunk) {
        sink.send_audio(frame).await?;
    }
    sink.finalize().await?;

    loop {
        let event = events.next_event().await;
        let at = started.elapsed();
        match event {
            Some(TranscriptEvent::Stable(text)) => println!("[{at:>7.1?}] final: {text}"),
            Some(TranscriptEvent::Completed) => {
                println!("[{at:>7.1?}] completed");
                return Ok(());
            }
            Some(TranscriptEvent::Failed(error)) => {
                println!("[{at:>7.1?}] FAILED: {error:#}");
                return Err(error.context("provider reported failure"));
            }
            None => anyhow::bail!("provider ended without Completed"),
        }
    }
}

/// Read a wav as 16kHz mono s16le. Resampling belongs to the daemon's audio
/// layer; the probe deliberately only accepts already-correct input.
fn load_wav_16k_mono(path: &Path) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    anyhow::ensure!(spec.bits_per_sample == 16, "only 16-bit wav, got {spec:?}");
    anyhow::ensure!(
        spec.sample_rate == 16_000 && spec.channels == 1,
        "probe needs 16kHz mono wav, got {}Hz {}ch",
        spec.sample_rate,
        spec.channels
    );
    let samples = reader.samples::<i16>().collect::<Result<_, _>>()?;
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{SessionConfig, from_config};
    use crate::backend::testing::{Built, Recording, Refusing};
    use crate::backend::{Api, Connection};
    use crate::config::Config;

    fn config(url: &str) -> Config {
        toml::from_str(&format!("[provider]\nurl = \"{url}\"\nmodel = \"asr\"\n")).unwrap()
    }

    #[test]
    fn only_whisper_cpp_skips_the_backend() {
        let backend = Recording::default();

        for url in [
            "http://127.0.0.1:8000/v1/audio/transcriptions",
            "http://127.0.0.1:8080/inference",
            "https://asr.example/v1/audio/transcriptions?api-version=1",
        ] {
            from_config(&config(url), &backend).unwrap();
        }

        assert_eq!(
            backend.built.lock().unwrap().clone(),
            [Built::Transcriber(
                Connection {
                    api: Api::Compatible,
                    base_url: Some("http://127.0.0.1:8000/v1".to_string()),
                    api_key: None,
                },
                "asr".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn a_whisper_cpp_take_is_uploaded_and_reported_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            // The upload ends with the closing multipart boundary.
            while !request.ends_with(b"--\r\n") {
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0, "the upload ended early");
                request.extend_from_slice(&chunk[..count]);
            }
            let body = r#"{"text":"完整文本"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let config = config(&format!("http://{address}/inference"));
        let session = from_config(&config, &Refusing)
            .unwrap()
            .start_session(&SessionConfig {
                language: "auto".to_string(),
                sample_rate: 16_000,
            })
            .await
            .unwrap();
        let (mut sink, mut events) = session.split();
        sink.send_audio(&[1, 2, 3]).await.unwrap();
        sink.finalize().await.unwrap();

        let mut reported = Vec::new();
        while let Some(event) = tokio::time::timeout(Duration::from_secs(2), events.next_event())
            .await
            .unwrap()
        {
            reported.push(format!("{event:?}"));
        }

        assert!(server.await.unwrap().starts_with("POST /inference "));
        assert_eq!(reported, [r#"Stable("完整文本")"#, "Completed"]);
    }
}
