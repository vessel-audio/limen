//! ASR provider abstraction: the transcription backend is pluggable and
//! sglang-omni is just one implementation.
//!
//! Shipped providers:
//! - [`openai_realtime`]: WebSocket OpenAI Realtime protocol, incremental results
//! - [`openai_batch`]: POST /v1/audio/transcriptions, whole-take fallback

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Instant;

use anyhow::Result;

use crate::config::Config;

pub mod openai_batch;
pub mod openai_realtime;

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
    /// Unstable incremental text; display-only, never injected.
    Provisional(String),
    /// Finalized text; handed to the injector.
    Stable(String),
    /// The session finished (after `finalize`): all stable text delivered.
    Completed,
    Failed(anyhow::Error),
}

pub trait AsrProvider: Send + Sync {
    fn start_session<'a>(
        &'a self,
        cfg: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn AsrSession>>>;
}

/// One live session. Splits into an audio sink and an event source so the
/// capture task and the injection task can run concurrently.
pub trait AsrSession: Send {
    fn split(self: Box<Self>) -> (Box<dyn AudioSink>, Box<dyn EventSource>);
}

pub trait AudioSink: Send {
    /// Push one chunk of 16kHz mono s16le audio.
    fn send_audio<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<()>>;
    /// End of audio: flush the stable tail; the event stream then winds down.
    fn finalize(&mut self) -> BoxFuture<'_, Result<()>>;
}

pub trait EventSource: Send {
    /// Next transcript event; `None` means the session is over.
    fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>>;
}

/// Build the configured provider.
pub fn from_config(cfg: &Config) -> Box<dyn AsrProvider> {
    use crate::config::ProviderKind;
    match cfg.provider.kind {
        ProviderKind::OpenaiRealtime => {
            Box::new(openai_realtime::Provider::new(cfg.provider.clone()))
        }
        ProviderKind::OpenaiBatch => Box::new(openai_batch::Provider::new(cfg.provider.clone())),
    }
}

/// `voice-input probe <wav>`: stream a file at real-time pace and print every
/// event with a timestamp. Exists to document provider wire behavior.
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
    let chunk_ms = cfg.audio.chunk_ms;
    let sender = tokio::spawn(async move {
        let chunk = (16_000 * chunk_ms / 1000) as usize;
        for frame in pcm.chunks(chunk) {
            sink.send_audio(frame).await?;
            tokio::time::sleep(std::time::Duration::from_millis(chunk_ms as u64)).await;
        }
        sink.finalize().await
    });

    let result = loop {
        match events.next_event().await {
            Some(ev) => {
                let at = started.elapsed();
                match &ev {
                    TranscriptEvent::Provisional(t) => println!("[{at:>7.1?}] provisional: {t}"),
                    TranscriptEvent::Stable(t) => println!("[{at:>7.1?}] STABLE: {t}"),
                    TranscriptEvent::Completed => println!("[{at:>7.1?}] completed"),
                    TranscriptEvent::Failed(e) => println!("[{at:>7.1?}] FAILED: {e:#}"),
                }
                match ev {
                    // A failure is terminal: stop feeding audio and report it
                    // instead of letting the sender sleep through the rest of
                    // the wav and exiting Ok.
                    TranscriptEvent::Failed(e) => {
                        sender.abort();
                        break Err(e.context("provider reported failure"));
                    }
                    TranscriptEvent::Completed => break Ok(()),
                    _ => {}
                }
            }
            // The provider ended the session on its own.
            None => break Ok(()),
        }
    };
    match sender.await {
        Ok(send_result) => send_result.and(result),
        Err(join_err) if join_err.is_cancelled() => result,
        Err(join_err) => Err(join_err.into()),
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
