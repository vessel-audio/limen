//! A scripted daemon in a child process. It speaks protocol v1 through the
//! core's own types, records every request, and follows the daemon's take
//! lifecycle closely enough for the GUI: starting records, stopping
//! transcribes for a while and then keeps the transcript in history, and a
//! start during transcription queues the next take behind it.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use limen_core::{
    Backend, Config, ConfigSummary, Control, DictationState, DictationStatus, HistoryEntry,
    HistoryOutcome, Paths, PerAction, Push, RequestEntry, RequestOutcome, SessionState, Shortcut,
    ShortcutStatus, StateSnapshot, WireSnapshot, v1,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};

/// The argument that turns the test binary into the fake daemon.
pub const SUBCOMMAND: &str = "fake-daemon";

/// The daemon's state when the GUI first connects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Reject subscriptions the way a daemon from before protocol v1 does.
    pub outdated: bool,
    /// The version the hello reports.
    pub version: String,
    pub history: Vec<HistoryEntry>,
    pub requests: Vec<RequestEntry>,
    pub shortcut: ShortcutStatus,
    /// How long a stopped take stays in transcription.
    pub transcribe_ms: u64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            outdated: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
            history: Vec::new(),
            requests: Vec::new(),
            shortcut: ShortcutStatus {
                backend: Backend::None,
                session: SessionState::Disabled,
                preferred: PerAction {
                    toggle: Shortcut::DEFAULT_TOGGLE,
                    hold: Shortcut::DEFAULT_HOLD,
                },
                actual: PerAction::default(),
                last_error: None,
            },
            transcribe_ms: 300,
        }
    }
}

/// The running fake daemon, seen from the test.
pub struct FakeDaemon {
    child: Child,
    log: PathBuf,
}

impl FakeDaemon {
    pub fn spawn(
        env: &[(&'static str, OsString)],
        socket: &Path,
        artifacts: &Path,
        scenario: &Scenario,
    ) -> Result<Self> {
        let log = artifacts.join("requests.jsonl");
        let child = Command::new(std::env::current_exe()?)
            .arg(SUBCOMMAND)
            .arg(serde_json::to_string(scenario)?)
            .arg(&log)
            .env_clear()
            .envs(env.iter().cloned())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(artifacts.join("fake-daemon.log"))?)
            .spawn()
            .context("cannot start the fake daemon")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            match Instant::now() > deadline {
                true => bail!("the fake daemon did not bind {}", socket.display()),
                false => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        Ok(Self { child, log })
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Result<Vec<v1::Request>> {
        match std::fs::read_to_string(&self.log) {
            Ok(text) => text.lines().map(parse).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// How the daemon exited, once it has.
    pub fn exit_status(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse(line: &str) -> Result<v1::Request> {
    let mut value = serde_json::from_str::<serde_json::Value>(line)?;
    value
        .as_object_mut()
        .context("request is not an object")?
        .remove("v");
    Ok(serde_json::from_value(value)?)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The child process entry point: `fake-daemon <scenario json> <log path>`.
pub fn serve(args: &[String]) -> ExitCode {
    let result = tokio::runtime::Runtime::new()
        .context("cannot start a runtime")
        .and_then(|runtime| {
            let [scenario, log] = args else {
                bail!("usage: {SUBCOMMAND} <scenario json> <log path>");
            };
            runtime.block_on(run(serde_json::from_str(scenario)?, PathBuf::from(log)))
        });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fake daemon: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Daemon state shared by every connection.
struct Daemon {
    scenario: Scenario,
    take: u64,
    dictation: DictationStatus,
    queued: bool,
    next_history_id: u64,
    log: std::fs::File,
    snapshots: watch::Sender<Option<(StateSnapshot, ShortcutStatus)>>,
    /// The `provider.url` of the config file as of the last reload.
    provider_url: String,
}

impl Daemon {
    /// Begin a new toggle take.
    fn record(&mut self) {
        self.take += 1;
        self.dictation = DictationStatus {
            state: DictationState::Recording {
                control: Control::Toggle,
            },
            since_ms: Some(now_ms()),
        };
    }

    fn publish(&self) {
        let state = StateSnapshot {
            take: self.take,
            dictation: self.dictation,
            queued: self.queued,
            history: self.scenario.history.clone(),
            last_error: None,
            requests: self.scenario.requests.clone(),
            config: ConfigSummary {
                input_method: "fcitx5".to_string(),
                provider_model: "e2e-model".to_string(),
                provider_url: self.provider_url.clone(),
                polish_enabled: false,
            },
        };
        self.snapshots
            .send_replace(Some((state, self.scenario.shortcut.clone())));
    }
}

async fn run(scenario: Scenario, log: PathBuf) -> Result<()> {
    let paths = Paths::discover()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let next_history_id = scenario.history.iter().map(|entry| entry.id + 1).max();
    let take = scenario.history.iter().map(|entry| entry.take).max();
    let (snapshots, _) = watch::channel(None);
    let daemon = Arc::new(Mutex::new(Daemon {
        scenario,
        take: take.unwrap_or_default(),
        dictation: DictationStatus {
            state: DictationState::Idle,
            since_ms: None,
        },
        queued: false,
        next_history_id: next_history_id.unwrap_or(1),
        log: std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?,
        snapshots,
        provider_url: "http://127.0.0.1:9/v1".to_string(),
    }));
    daemon.lock().await.publish();
    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = Arc::clone(&daemon);
        let paths = paths.clone();
        tokio::spawn(async move {
            if let Err(error) = connection(stream, daemon, &paths).await {
                eprintln!("fake daemon connection: {error:#}");
            }
        });
    }
}

async fn connection(stream: UnixStream, daemon: Arc<Mutex<Daemon>>, paths: &Paths) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).await?;
    let line = line.trim();
    // Peer-credential probes connect and leave without a request.
    if line.is_empty() {
        return Ok(());
    }
    {
        let mut daemon = daemon.lock().await;
        writeln!(daemon.log, "{line}")?;
        daemon.log.flush()?;
    }
    let request = parse(line)?;
    let reply = match request {
        v1::Request::Subscribe => return subscribe(writer, daemon).await,
        v1::Request::Shutdown => {
            send(&mut writer, &v1::Reply::Ok).await?;
            std::fs::remove_file(&paths.socket)?;
            std::process::exit(0);
        }
        v1::Request::Start => {
            let mut daemon = daemon.lock().await;
            match daemon.dictation.state {
                DictationState::Transcribing | DictationState::Committing => {
                    daemon.queued = true;
                }
                DictationState::Idle
                | DictationState::Starting { .. }
                | DictationState::Recording { .. } => daemon.record(),
            }
            daemon.publish();
            v1::Reply::Accepted {
                take: daemon.take + u64::from(daemon.queued),
            }
        }
        v1::Request::Stop { delivery } => stop(&daemon, delivery).await,
        v1::Request::ClearHistory => {
            let mut daemon = daemon.lock().await;
            daemon
                .scenario
                .history
                .retain(|entry| matches!(entry.outcome, HistoryOutcome::Pending));
            daemon.publish();
            v1::Reply::Ok
        }
        v1::Request::Hold { .. } | v1::Request::ShortcutRebind => v1::Reply::Ok,
        v1::Request::ShortcutStatus => {
            v1::Reply::ShortcutStatus(daemon.lock().await.scenario.shortcut.clone())
        }
        v1::Request::ProviderReload => match Config::load(&paths.config_file) {
            Ok(config) => {
                let mut daemon = daemon.lock().await;
                daemon.provider_url = config.provider.url;
                daemon.publish();
                v1::Reply::Ok
            }
            Err(error) => v1::Reply::Error {
                message: format!("{error:#}"),
            },
        },
    };
    send(&mut writer, &reply).await
}

/// Move a recording take into transcription, then keep its transcript.
async fn stop(daemon: &Arc<Mutex<Daemon>>, delivery: v1::Delivery) -> v1::Reply {
    let mut state = daemon.lock().await;
    if !matches!(state.dictation.state, DictationState::Recording { .. }) {
        return v1::Reply::Error {
            message: "not recording".to_string(),
        };
    }
    let take = state.take;
    let started_ms = now_ms();
    state.dictation = DictationStatus {
        state: DictationState::Transcribing,
        since_ms: Some(started_ms),
    };
    state.publish();
    let transcribing = Duration::from_millis(state.scenario.transcribe_ms);
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        tokio::time::sleep(transcribing).await;
        let mut daemon = daemon.lock().await;
        let text = format!("e2e transcript {take}");
        let id = daemon.next_history_id;
        daemon.next_history_id += 1;
        daemon.scenario.history.push(HistoryEntry {
            id,
            take,
            at_ms: now_ms(),
            original: text.clone(),
            text: Some(text),
            outcome: match delivery {
                v1::Delivery::Keep => HistoryOutcome::Kept,
                v1::Delivery::Input => HistoryOutcome::Committed,
            },
            truncated: false,
        });
        daemon.scenario.requests.push(RequestEntry {
            take,
            started_ms,
            duration_ms: now_ms().saturating_sub(started_ms),
            outcome: RequestOutcome::Succeeded,
        });
        daemon.dictation = DictationStatus {
            state: DictationState::Idle,
            since_ms: None,
        };
        if daemon.queued {
            daemon.queued = false;
            daemon.record();
        }
        daemon.publish();
    });
    v1::Reply::Accepted { take }
}

/// Send the hello, then every snapshot change and, while recording, levels.
async fn subscribe(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    daemon: Arc<Mutex<Daemon>>,
) -> Result<()> {
    let (outdated, version, mut snapshots) = {
        let daemon = daemon.lock().await;
        (
            daemon.scenario.outdated,
            daemon.scenario.version.clone(),
            daemon.snapshots.subscribe(),
        )
    };
    if outdated {
        return send(
            &mut writer,
            &Push::Error {
                message: "invalid request: unknown variant `subscribe`".to_string(),
            },
        )
        .await;
    }
    send(
        &mut writer,
        &Push::Hello {
            protocol: v1::VERSION,
            daemon_version: version,
        },
    )
    .await?;
    let mut levels = tokio::time::interval(Duration::from_millis(100));
    let mut seq = 0;
    snapshots.mark_changed();
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                changed?;
                let current = snapshots.borrow_and_update().clone();
                if let Some((state, shortcut)) = current {
                    seq += 1;
                    send(&mut writer, &Push::Snapshot(Box::new(WireSnapshot { seq, state, shortcut }))).await?;
                }
            }
            _ = levels.tick() => {
                let recording = snapshots.borrow().as_ref().is_some_and(|(state, _)| {
                    matches!(state.dictation.state, DictationState::Recording { .. })
                });
                if recording {
                    send(&mut writer, &Push::Level { rms: 0.2, dbfs: -14.0 }).await?;
                }
            }
        }
    }
}

async fn send(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &impl Serialize,
) -> Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    Ok(())
}
