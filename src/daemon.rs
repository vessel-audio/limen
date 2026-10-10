//! The resident daemon: one `select!` loop owns the ASR sink, the event
//! stream, and the state machine, so session state never crosses tasks.
//!
//! State machine: Idle --toggle--> Recording --toggle--> Finalizing
//! --(Completed / Failed / stream end)--> Idle.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio::{Capture, SAMPLE_RATE};
use crate::config::{Config, Paths};
use crate::inject::Injector;
use crate::notify::notify;
use crate::provider::{self, AudioSink, EventSource, SessionConfig, TranscriptEvent};
use crate::ipc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Recording,
    Finalizing,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Finalizing => "finalizing",
        }
    }
}

/// Everything belonging to one live dictation session.
struct Active {
    sink: Box<dyn AudioSink>,
    events: Box<dyn EventSource>,
    capture: tokio::task::JoinHandle<()>,
}

pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let mut injector = Injector::new();
    let mut state = State::Idle;
    let mut active: Option<Active> = None;
    // Fresh per session; the capture task fills it with 100ms PCM chunks.
    let mut pcm_rx: Option<mpsc::Receiver<Vec<i16>>> = None;

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    loop {
        tokio::select! {
            conn = listener.accept() => {
                let (stream, _) = conn?;
                let (req, _read, write) = ipc::recv(stream).await?;
                let reply = match req {
                    ipc::Request::Status => state.label().to_string(),
                    ipc::Request::Toggle => {
                        match toggle(state, &mut active, &mut pcm_rx, &cfg).await {
                            Ok(next) => {
                                state = next;
                                next.label().to_string()
                            }
                            Err(e) => {
                                // A failed start/stop must not wedge the machine.
                                teardown(&mut active, &mut pcm_rx).await;
                                state = State::Idle;
                                notify("voice-input error", &format!("{e:#}"));
                                format!("error: {e:#}")
                            }
                        }
                    }
                };
                ipc::reply(write, &reply).await?;
            }

            chunk = async { pcm_rx.as_mut().unwrap().recv().await },
                    if matches!(state, State::Recording) && pcm_rx.is_some() => {
                match chunk {
                    Some(pcm) => {
                        if let Some(a) = active.as_mut() {
                            if let Err(e) = a.sink.send_audio(&pcm).await {
                                tracing::warn!(error = %e, "send_audio failed");
                                teardown(&mut active, &mut pcm_rx).await;
                                state = State::Idle;
                                notify("voice-input error", &format!("send audio: {e:#}"));
                            }
                        }
                    }
                    // Capture ended on its own (device unplugged, pw-record died).
                    None => {
                        tracing::warn!("capture ended unexpectedly");
                        teardown(&mut active, &mut pcm_rx).await;
                        state = State::Idle;
                        notify("voice-input error", "microphone capture ended");
                    }
                }
            }

            ev = async { active.as_mut().unwrap().events.next_event().await },
                    if matches!(state, State::Recording | State::Finalizing) && active.is_some() => {
                match ev {
                    Some(TranscriptEvent::Stable(text)) => {
                        if let Err(e) = injector.type_segment(&text).await {
                            tracing::warn!(error = %e, "inject failed");
                            notify("voice-input error", &format!("type text: {e:#}"));
                        }
                    }
                    Some(TranscriptEvent::Provisional(text)) => {
                        tracing::debug!(text, "provisional");
                    }
                    Some(TranscriptEvent::Completed) => {
                        teardown(&mut active, &mut pcm_rx).await;
                        state = State::Idle;
                        notify("voice-input", "dictation finished");
                    }
                    Some(TranscriptEvent::Failed(e)) => {
                        let msg = format!("{e:#}");
                        teardown(&mut active, &mut pcm_rx).await;
                        state = State::Idle;
                        notify("voice-input error", &msg);
                    }
                    None => {
                        tracing::warn!("event stream ended without Completed");
                        teardown(&mut active, &mut pcm_rx).await;
                        state = State::Idle;
                    }
                }
            }
        }
    }
}

/// Handle one toggle: Idle -> start recording; Recording -> finalize;
/// Finalizing -> ignore (a second toggle must not kill the tail flush).
async fn toggle(
    state: State,
    active: &mut Option<Active>,
    pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>,
    cfg: &Config,
) -> Result<State> {
    match state {
        State::Idle => {
            let session_cfg = SessionConfig {
                model: cfg.provider.model.clone(),
                language: cfg.provider.language.clone(),
                sample_rate: SAMPLE_RATE,
                api_key: cfg.api_key(),
            };
            let session = provider::from_config(cfg).start_session(&session_cfg).await?;
            let (sink, events) = session.split();
            let (tx, rx) = mpsc::channel(64);
            let device = cfg.audio.device.clone();
            let chunk_ms = cfg.audio.chunk_ms;
            let capture = tokio::spawn(async move {
                if let Err(e) = capture_loop(device, chunk_ms, tx).await {
                    tracing::warn!(error = %e, "capture loop failed");
                }
            });
            *pcm_rx = Some(rx);
            *active = Some(Active { sink, events, capture });
            notify("voice-input", "listening…");
            Ok(State::Recording)
        }
        State::Recording => {
            if let Some(a) = active.as_mut() {
                a.capture.abort();
                a.sink.finalize().await?;
            }
            Ok(State::Finalizing)
        }
        State::Finalizing => Ok(State::Finalizing),
    }
}

/// Drop the session and the PCM channel; the capture task is killed via its
/// JoinHandle (pw-record dies with `kill_on_drop`).
async fn teardown(active: &mut Option<Active>, pcm_rx: &mut Option<mpsc::Receiver<Vec<i16>>>) {
    if let Some(a) = active.take() {
        a.capture.abort();
    }
    pcm_rx.take();
}

async fn capture_loop(
    device: Option<String>,
    chunk_ms: u32,
    tx: mpsc::Sender<Vec<i16>>,
) -> Result<()> {
    let mut capture = Capture::start(device.as_deref())?;
    let chunk_samples = (SAMPLE_RATE * chunk_ms / 1000) as usize;
    loop {
        match capture.read_chunk(chunk_samples).await? {
            Some(pcm) => {
                // Lagging consumer must not stall the microphone: drop frames.
                match tx.try_send(pcm) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!("pcm channel full, dropping a chunk");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                }
            }
            None => return Ok(()),
        }
    }
}
