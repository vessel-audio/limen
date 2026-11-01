//! ASR session abstraction for buffered, whole-take transcription.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Instant;

use anyhow::Result;

use crate::config::Config;

pub mod openai_batch;
mod setup;

pub use setup::{Endpoint, set_server, split_url};

/// Object-safe, cancellable future used by the provider trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Static parameters of one ASR session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub model: String,
    pub language: String,
    pub sample_rate: u32,
    pub api_key: Option<String>,
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

/// Build the buffered transcription provider.
pub fn from_config(cfg: &Config) -> Box<dyn AsrProvider> {
    Box::new(openai_batch::Provider::new(cfg.provider.clone()))
}

/// `limen probe <wav>`: transcribe a file and print the final event flow.
pub async fn probe(cfg: &Config, wav: &Path) -> Result<()> {
    let pcm = load_wav_16k_mono(wav)?;
    let session_cfg = SessionConfig {
        model: cfg.provider.model.clone(),
        language: cfg.provider.language.clone(),
        sample_rate: 16_000,
        api_key: cfg.api_key(),
    };
    let session = from_config(cfg).start_session(&session_cfg).await?;
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
