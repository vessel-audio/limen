//! Buffered provider: accumulate one take, hand it as a WAV to a
//! [`Transcriber`] once recording ends, and emit one final result.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::attempts::Attempts;
use crate::backend::{BoxFuture, Take, Transcriber};
use crate::provider::{
    AsrProvider, AsrSession, AudioSink, EventSource, SessionConfig, TranscriptEvent,
};

/// How long one transcription try may take when the provider sets none.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Provider {
    transcriber: Arc<dyn Transcriber>,
    attempts: Attempts,
}

impl Provider {
    /// Buffered takes sent to `transcriber`, each tried as `attempts` says.
    pub fn new(transcriber: Arc<dyn Transcriber>, attempts: Attempts) -> Self {
        Self {
            transcriber,
            attempts,
        }
    }
}

impl AsrProvider for Provider {
    fn start_session<'a>(
        &'a self,
        cfg: &'a SessionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn AsrSession>>> {
        Box::pin(async move {
            let (events, rx) = mpsc::channel(8);
            let (finish, finish_rx) = oneshot::channel();
            let worker = tokio::spawn(transcription_worker(
                finish_rx,
                self.transcriber.clone(),
                self.attempts,
                events,
            ));
            let session: Box<dyn AsrSession> = Box::new(Session {
                sink: Sink {
                    cfg: cfg.clone(),
                    pcm: Vec::new(),
                    finish: Some(finish),
                },
                source: Source {
                    rx,
                    worker: Some(worker),
                },
            });
            Ok(session)
        })
    }
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
    cfg: SessionConfig,
    pcm: Vec<i16>,
    finish: Option<oneshot::Sender<Recording>>,
}

/// What the sink hands the worker when recording ends.
struct Recording {
    cfg: SessionConfig,
    pcm: Vec<i16>,
}

impl AudioSink for Sink {
    fn send_audio<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.finish.is_none() {
                bail!("cannot send audio after finalize");
            }
            self.pcm.extend_from_slice(pcm);
            Ok(())
        })
    }

    fn finalize(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let finish = self.finish.take().context("session already finalized")?;
            let recording = Recording {
                cfg: self.cfg.clone(),
                pcm: std::mem::take(&mut self.pcm),
            };
            finish
                .send(recording)
                .map_err(|_| anyhow!("transcription worker stopped"))
        })
    }
}

struct Source {
    rx: mpsc::Receiver<TranscriptEvent>,
    /// Finishes right after sending its terminal event; aborted on drop.
    worker: Option<JoinHandle<()>>,
}

impl EventSource for Source {
    fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>> {
        // Nothing may be awaited after `recv` returns: a cancelled call would
        // drop the event it already took off the channel.
        Box::pin(self.rx.recv())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

async fn transcription_worker(
    finish: oneshot::Receiver<Recording>,
    transcriber: Arc<dyn Transcriber>,
    attempts: Attempts,
    events: mpsc::Sender<TranscriptEvent>,
) {
    let Ok(recording) = finish.await else {
        return;
    };
    let outcome = match transcribe(recording, &*transcriber, attempts).await {
        Ok(text) => TranscriptEvent::Stable(text),
        Err(error) => TranscriptEvent::Failed(error),
    };
    let is_ok = !matches!(outcome, TranscriptEvent::Failed(_));
    // Best-effort delivery; a dead receiver means the caller went away.
    let _ = events.send(outcome).await;
    if is_ok {
        // The trait promises Completed once the final text is delivered.
        let _ = events.send(TranscriptEvent::Completed).await;
    }
}

async fn transcribe(
    recording: Recording,
    transcriber: &dyn Transcriber,
    attempts: Attempts,
) -> Result<String> {
    let take = Take {
        wav: encode_wav(&recording.pcm, recording.cfg.sample_rate)?,
        language: match recording.cfg.language.as_str() {
            "auto" => None,
            language => Some(language.to_string()),
        },
    };
    attempts
        .run("transcription", || transcriber.transcribe(take.clone()))
        .await
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};
    use std::task::Poll;
    use std::time::Duration;

    use anyhow::{Result, bail};
    use tokio::sync::{Notify, oneshot};

    use super::{Provider, Source};
    use crate::attempts::Attempts;
    use crate::backend::{BoxFuture, Take, Transcriber};
    use crate::provider::{AsrProvider, AudioSink, EventSource, SessionConfig, TranscriptEvent};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    /// One try with the default timeout.
    const ONCE: Attempts = Attempts {
        timeout: super::REQUEST_TIMEOUT,
        retries: 0,
        budget: super::REQUEST_TIMEOUT,
    };

    /// Records each take and answers it once `release` is notified.
    struct Scripted {
        answer: fn() -> Result<String>,
        takes: Mutex<Vec<Take>>,
        asked: Notify,
        release: Notify,
    }

    impl Scripted {
        fn new(answer: fn() -> Result<String>) -> Arc<Self> {
            Arc::new(Self {
                answer,
                takes: Mutex::new(Vec::new()),
                asked: Notify::new(),
                release: Notify::new(),
            })
        }
    }

    impl Transcriber for Scripted {
        fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<String>> {
            Box::pin(async move {
                self.takes.lock().unwrap().push(take);
                self.asked.notify_one();
                self.release.notified().await;
                (self.answer)()
            })
        }
    }

    async fn start_session(
        provider: &Provider,
        language: &str,
    ) -> (Box<dyn AudioSink>, Box<dyn EventSource>) {
        let config = SessionConfig {
            language: language.to_string(),
            sample_rate: 16_000,
        };
        provider.start_session(&config).await.unwrap().split()
    }

    #[tokio::test]
    async fn finalize_returns_before_transcribing_and_emits_one_complete_transcript() {
        let transcriber = Scripted::new(|| Ok("完整文本".to_string()));
        let provider = Provider::new(transcriber.clone(), ONCE);
        let (mut sink, mut events) = start_session(&provider, "zh").await;
        sink.send_audio(&[1, 2]).await.unwrap();
        sink.send_audio(&[3, 4]).await.unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(30), transcriber.asked.notified())
                .await
                .is_err(),
            "recording asked for a transcript before finalize"
        );
        tokio::time::timeout(Duration::from_millis(100), sink.finalize())
            .await
            .expect("finalize waited for the transcript")
            .unwrap();
        tokio::time::timeout(TEST_TIMEOUT, transcriber.asked.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), events.next_event())
                .await
                .is_err(),
            "transcript arrived before the transcriber answered"
        );

        transcriber.release.notify_one();
        match tokio::time::timeout(TEST_TIMEOUT, events.next_event())
            .await
            .unwrap()
        {
            Some(TranscriptEvent::Stable(text)) => assert_eq!(text, "完整文本"),
            other => panic!("expected one final transcript, got {other:?}"),
        }
        assert!(matches!(
            tokio::time::timeout(TEST_TIMEOUT, events.next_event())
                .await
                .unwrap(),
            Some(TranscriptEvent::Completed)
        ));

        let take = transcriber.takes.lock().unwrap().remove(0);
        let mut wav = hound::WavReader::new(Cursor::new(&take.wav)).unwrap();
        let samples = wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            (samples, take.language),
            (vec![1, 2, 3, 4], Some("zh".to_string()))
        );
        assert!(sink.finalize().await.is_err());
        assert!(sink.send_audio(&[5]).await.is_err());
    }

    #[tokio::test]
    async fn automatic_language_leaves_detection_to_the_model() {
        let transcriber = Scripted::new(|| Ok(String::new()));
        let provider = Provider::new(transcriber.clone(), ONCE);
        let (mut sink, _events) = start_session(&provider, "auto").await;
        sink.finalize().await.unwrap();
        tokio::time::timeout(TEST_TIMEOUT, transcriber.asked.notified())
            .await
            .unwrap();

        assert_eq!(transcriber.takes.lock().unwrap()[0].language, None);
    }

    #[tokio::test]
    async fn a_failed_transcription_emits_failure_without_a_transcript() {
        let transcriber = Scripted::new(|| bail!("HTTP 500"));
        let provider = Provider::new(transcriber.clone(), ONCE);
        let (mut sink, mut events) = start_session(&provider, "auto").await;
        sink.send_audio(&[1]).await.unwrap();
        sink.finalize().await.unwrap();
        transcriber.release.notify_one();

        assert!(matches!(
            tokio::time::timeout(TEST_TIMEOUT, events.next_event())
                .await
                .unwrap(),
            Some(TranscriptEvent::Failed(_))
        ));
        assert!(events.next_event().await.is_none());
    }

    #[tokio::test]
    async fn request_timeout_emits_failure_without_a_transcript() {
        let transcriber = Scripted::new(|| Ok("too late".to_string()));
        let mut provider = Provider::new(transcriber.clone(), ONCE);
        provider.attempts.timeout = Duration::from_millis(50);
        let (mut sink, mut events) = start_session(&provider, "auto").await;
        sink.send_audio(&[1]).await.unwrap();
        sink.finalize().await.unwrap();

        assert!(matches!(
            tokio::time::timeout(TEST_TIMEOUT, events.next_event())
                .await
                .unwrap(),
            Some(TranscriptEvent::Failed(_))
        ));
        assert!(events.next_event().await.is_none());
    }

    #[tokio::test]
    async fn dropping_the_event_source_cancels_the_worker() {
        struct CancelNotification(Option<oneshot::Sender<()>>);
        impl Drop for CancelNotification {
            fn drop(&mut self) {
                if let Some(notification) = self.0.take() {
                    let _ = notification.send(());
                }
            }
        }

        let (_events, rx) = tokio::sync::mpsc::channel(1);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let notification = CancelNotification(Some(cancel_tx));
        let worker = tokio::spawn(async move {
            let _notification = notification;
            std::future::pending::<()>().await;
        });
        let source = Source {
            rx,
            worker: Some(worker),
        };
        drop(source);
        tokio::time::timeout(TEST_TIMEOUT, cancel_rx)
            .await
            .expect("worker was not cancelled")
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_next_event_does_not_lose_the_terminal_event() {
        let (events, rx) = tokio::sync::mpsc::channel(1);
        let worker = tokio::spawn(std::future::pending::<()>());
        let mut source = Source {
            rx,
            worker: Some(worker),
        };
        events.send(TranscriptEvent::Completed).await.unwrap();
        drop(events);

        // The session actor polls `next_event` inside `select!`, which drops
        // the future whenever a command wins the race.
        let mut next = source.next_event();
        let first = std::future::poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx))).await;
        drop(next);
        let delivered = match first {
            Poll::Ready(event) => event,
            Poll::Pending => tokio::time::timeout(TEST_TIMEOUT, source.next_event())
                .await
                .unwrap(),
        };

        match delivered {
            Some(TranscriptEvent::Completed) => {}
            other => panic!("terminal event lost after cancellation, got {other:?}"),
        }
    }
}
