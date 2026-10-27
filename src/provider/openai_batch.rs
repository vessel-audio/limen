//! Buffered provider: accumulate one take, POST it as a wav to `<url>` (an
//! OpenAI-compatible `/v1/audio/transcriptions`), and emit one final result.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::ProviderConfig;
use crate::provider::{
    AsrProvider, AsrSession, AudioSink, BoxFuture, EventSource, SessionConfig, TranscriptEvent,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Provider {
    cfg: ProviderConfig,
    client: reqwest::Client,
    request_timeout: Duration,
}

impl Provider {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
            request_timeout: REQUEST_TIMEOUT,
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
                self.client.clone(),
                self.cfg.url.clone(),
                self.request_timeout,
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
    finish: Option<oneshot::Sender<Take>>,
}

struct Take {
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
            let take = Take {
                cfg: self.cfg.clone(),
                pcm: std::mem::take(&mut self.pcm),
            };
            finish
                .send(take)
                .map_err(|_| anyhow!("transcription worker stopped"))
        })
    }
}

struct Source {
    rx: mpsc::Receiver<TranscriptEvent>,
    worker: Option<JoinHandle<()>>,
}

impl EventSource for Source {
    fn next_event(&mut self) -> BoxFuture<'_, Option<TranscriptEvent>> {
        Box::pin(async move {
            let event = self.rx.recv().await;
            if matches!(
                event,
                None | Some(TranscriptEvent::Completed | TranscriptEvent::Failed(_))
            ) && let Some(worker) = self.worker.take()
            {
                worker.abort();
                let _ = worker.await;
            }
            event
        })
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
    finish: oneshot::Receiver<Take>,
    http: reqwest::Client,
    url: String,
    request_timeout: Duration,
    events: mpsc::Sender<TranscriptEvent>,
) {
    let Ok(take) = finish.await else {
        return;
    };
    let outcome = match transcribe(take, http, url, request_timeout).await {
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
    take: Take,
    http: reqwest::Client,
    url: String,
    request_timeout: Duration,
) -> Result<String> {
    let wav = encode_wav(&take.pcm, take.cfg.sample_rate)?;
    let mut form = reqwest::multipart::Form::new()
        .text("model", take.cfg.model)
        .part(
            "file",
            reqwest::multipart::Part::bytes(wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")?,
        );
    if take.cfg.language != "auto" {
        form = form.text("language", take.cfg.language);
    }
    let mut req = http.post(url).multipart(form).timeout(request_timeout);
    if let Some(key) = take.cfg.api_key {
        req = req.bearer_auth(key);
    }
    let response = req.send().await.context("transcription request")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("HTTP {status}: {body}");
    }
    response
        .json::<TranscriptionResponse>()
        .await
        .context("parse response")
        .map(|body| body.text)
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::{Provider, Source};
    use crate::config::{ProviderConfig, ProviderKind};
    use crate::provider::{AsrProvider, AudioSink, EventSource, SessionConfig, TranscriptEvent};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    struct MockServer {
        url: String,
        request: oneshot::Receiver<Vec<u8>>,
        release: oneshot::Sender<()>,
        task: JoinHandle<()>,
    }

    async fn mock_server(status: &'static str, body: String) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    return;
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
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        request.len() >= header_end + 4 + content_length
                    });
                if complete {
                    break;
                }
            }
            let _ = request_tx.send(request);
            if release_rx.await.is_err() {
                return;
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        MockServer {
            url: format!("http://{address}/v1/audio/transcriptions"),
            request,
            release,
            task,
        }
    }

    fn provider(url: String) -> Provider {
        Provider::new(ProviderConfig {
            kind: ProviderKind::OpenaiBatch,
            url,
            model: "test-model".to_string(),
            language: "auto".to_string(),
            api_key_env: None,
        })
    }

    async fn start_session(provider: &Provider) -> (Box<dyn AudioSink>, Box<dyn EventSource>) {
        let config = SessionConfig {
            model: "test-model".to_string(),
            language: "auto".to_string(),
            sample_rate: 16_000,
            api_key: None,
        };
        provider.start_session(&config).await.unwrap().split()
    }

    #[tokio::test]
    async fn finalize_returns_before_http_and_emits_one_complete_transcript() {
        let server = mock_server("200 OK", json!({"text": "完整文本"}).to_string()).await;
        let provider = provider(server.url);
        let (mut sink, mut events) = start_session(&provider).await;
        sink.send_audio(&[1, 2]).await.unwrap();
        sink.send_audio(&[3, 4]).await.unwrap();

        let mut request = server.request;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut request)
                .await
                .is_err(),
            "recording sent an HTTP request before finalize"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), events.next_event())
                .await
                .is_err(),
            "recording produced a transcript before finalize"
        );
        tokio::time::timeout(Duration::from_millis(100), sink.finalize())
            .await
            .expect("finalize waited for the HTTP response")
            .unwrap();
        let request = tokio::time::timeout(TEST_TIMEOUT, request)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), events.next_event())
                .await
                .is_err(),
            "transcript arrived before the server responded"
        );

        server.release.send(()).unwrap();
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
        server.task.await.unwrap();

        let wav_start = request
            .windows(4)
            .position(|part| part == b"RIFF")
            .expect("multipart body has no WAV");
        let mut wav = hound::WavReader::new(Cursor::new(&request[wav_start..])).unwrap();
        let samples = wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(samples, [1, 2, 3, 4]);
        assert!(sink.finalize().await.is_err());
        assert!(sink.send_audio(&[5]).await.is_err());
    }

    #[tokio::test]
    async fn http_and_json_errors_emit_failure_without_a_transcript() {
        for (status, body) in [
            ("500 Internal Server Error", "not-json"),
            ("200 OK", "not-json"),
        ] {
            let server = mock_server(status, body.to_string()).await;
            let provider = provider(server.url);
            let (mut sink, mut events) = start_session(&provider).await;
            sink.send_audio(&[1]).await.unwrap();
            sink.finalize().await.unwrap();
            server.release.send(()).unwrap();

            assert!(matches!(
                tokio::time::timeout(TEST_TIMEOUT, events.next_event())
                    .await
                    .unwrap(),
                Some(TranscriptEvent::Failed(_))
            ));
            assert!(events.next_event().await.is_none());
            server.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn request_timeout_emits_failure_without_a_transcript() {
        let server = mock_server("200 OK", json!({"text": "too late"}).to_string()).await;
        let mut provider = provider(server.url);
        provider.request_timeout = Duration::from_millis(50);
        let (mut sink, mut events) = start_session(&provider).await;
        sink.send_audio(&[1]).await.unwrap();
        sink.finalize().await.unwrap();
        server.request.await.unwrap();

        assert!(matches!(
            tokio::time::timeout(TEST_TIMEOUT, events.next_event())
                .await
                .unwrap(),
            Some(TranscriptEvent::Failed(_))
        ));
        drop(server.release);
        server.task.await.unwrap();
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
}
