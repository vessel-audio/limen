//! Tokio bridge for subscriptions, daemon lifecycle, and shortcut operations.

mod lifecycle;

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{Stream, TryStreamExt};
use limen_core::{
    Action as ShortcutAction, Backend, ConfigChange, DaemonNotRunning, Delivery, KeyChange,
    KeySource, Keys, Launch, ModelBackend, ModelRole, Paths, Provider, Push, Report, SetupError,
    Shortcut, clear, daemon_peer_pid, edit, list_models, load_provider, save_provider, set, setup,
    spawn_daemon, subscribe, terminate_daemon, v1,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use crate::model::{Msg, Toast};
use lifecycle::{
    StartError, StartResult, is_outdated, request_with_timeout, restart, wait_for_socket_removal,
    wait_for_termination,
};

/// Commands sent from the GPUI thread to the bridge.
#[derive(Debug, Clone)]
pub enum BridgeCommand {
    /// Stop the bridge even if GPUI tasks still retain command senders.
    Quit,
    Start,
    Stop,
    ClearHistory,
    SpawnDaemon,
    RestartDaemon,
    ShutdownDaemon,
    TerminateDaemon,
    Shortcut(ShortcutRequest),
    /// Read a provider's settings from the config file; `id` identifies the
    /// request in the reply so an older read cannot replace a newer one.
    LoadProvider {
        role: ModelRole,
        id: u64,
    },
    /// Save a provider and its key, and apply them in a running daemon.
    SaveProvider {
        id: u64,
        provider: Provider,
        key: KeyChange,
    },
    /// Ask the server a page's unsaved fields name which models it
    /// offers; `endpoint` names what the GUI asks about in its reply.
    ListModels {
        role: ModelRole,
        endpoint: String,
        provider: Provider,
        key: KeyChange,
    },
}

/// Shortcut writes executed through the same core operations as the CLI.
#[derive(Debug, Clone)]
pub enum ShortcutRequest {
    Setup {
        backend: Backend,
        keys: Keys,
    },
    Set {
        action: ShortcutAction,
        keys: Shortcut,
    },
    Clear {
        action: ShortcutAction,
    },
    Edit,
}

/// Events sent from the bridge to the GPUI thread.
#[derive(Debug)]
pub enum BridgeEvent {
    Msg(Msg),
    OfferTerminate,
    ShortcutReport(Result<Report, ShortcutFailure>),
    /// A provider's settings as read from the config file.
    ProviderLoaded {
        role: ModelRole,
        id: u64,
        result: Result<(Provider, KeySource), String>,
    },
    /// How saving a provider ended.
    ProviderSaved {
        role: ModelRole,
        id: u64,
        result: Result<ConfigChange, String>,
    },
    /// The models a server offers, or why it listed none.
    Models {
        role: ModelRole,
        endpoint: String,
        models: Result<Vec<String>, String>,
    },
    Toast(Toast),
}

/// A failed shortcut operation and the effects completed before the failure.
#[derive(Debug)]
pub struct ShortcutFailure {
    pub report: Report,
    pub message: String,
}

type ShortcutTasks = JoinSet<Result<Report, ShortcutFailure>>;

/// Start a current-thread Tokio runtime on a dedicated native thread.
/// What the bridge reaches: the user's files and daemon, and the backend
/// that lists the models a provider offers.
pub(crate) struct Env {
    pub(crate) paths: Paths,
    pub(crate) models: Arc<dyn ModelBackend>,
}

pub fn start(
    paths: Paths,
    models: Arc<dyn ModelBackend>,
) -> (
    mpsc::Sender<BridgeCommand>,
    mpsc::Receiver<BridgeEvent>,
    std::thread::JoinHandle<()>,
) {
    let (commands, command_rx) = mpsc::channel(32);
    let (events, event_rx) = mpsc::channel(64);
    let thread = std::thread::Builder::new()
        .name("limen-bridge".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("bridge runtime");
            runtime.block_on(run(Env { paths, models }, events, command_rx));
        })
        .expect("failed to spawn bridge thread");
    (commands, event_rx, thread)
}

#[derive(Debug, Clone, Copy)]
struct Backoff(Duration);

impl Default for Backoff {
    fn default() -> Self {
        Self(Duration::from_millis(500))
    }
}

impl Backoff {
    fn next(&mut self) -> Duration {
        let current = self.0;
        self.0 = (self.0 * 2).min(Duration::from_secs(5));
        current
    }
}

#[derive(Debug, Clone, Copy)]
enum StartMode {
    Spawn,
    Restart,
}

struct SpawnGate {
    task: Option<JoinHandle<StartResult>>,
}

impl SpawnGate {
    fn start(&mut self, paths: &Paths, mode: StartMode) -> bool {
        if self.task.is_some() {
            return false;
        }
        let paths = paths.clone();
        self.task = Some(tokio::spawn(async move {
            match mode {
                StartMode::Spawn => spawn_daemon(&paths)
                    .await
                    .map_err(|error| StartError::Failed(format!("{error:#}"))),
                StartMode::Restart => restart(&paths).await,
            }
        }));
        true
    }

    async fn finish(&mut self) -> Option<StartResult> {
        match &mut self.task {
            Some(task) => Some(
                task.await
                    .unwrap_or_else(|error| Err(StartError::Failed(error.to_string()))),
            ),
            None => std::future::pending().await,
        }
    }
}

#[tracing::instrument(skip_all)]
async fn run(
    env: Env,
    events: mpsc::Sender<BridgeEvent>,
    mut commands: mpsc::Receiver<BridgeCommand>,
) {
    let mut backoff = Backoff::default();
    let mut spawn = SpawnGate { task: None };
    let mut shortcuts = ShortcutTasks::new();
    let mut stream: Option<Pin<Box<dyn Stream<Item = Result<Push>>>>> = None;
    let mut next_connect = tokio::time::Instant::now();
    let mut stopped = false;
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return; };
                match command {
                    BridgeCommand::Quit => return,
                    command => handle_command(
                        &env,
                        &events,
                        command,
                        &mut stream,
                        &mut spawn,
                        &mut stopped,
                        &mut shortcuts,
                    ).await,
                }
            }
            result = shortcuts.join_next(), if !shortcuts.is_empty() => {
                match result {
                    Some(Ok(result)) => {
                        let _ = events.send(BridgeEvent::ShortcutReport(result)).await;
                    }
                    Some(Err(error)) => {
                        let _ = events.send(BridgeEvent::Msg(Msg::CommandError {
                            message: format!("shortcut operation failed: {error}"),
                        })).await;
                    }
                    None => {}
                }
            }
            outcome = spawn.finish() => {
                let Some(outcome) = outcome else { continue; };
                spawn.task = None;
                match outcome {
                    Ok(()) => {
                        stopped = false;
                        next_connect = tokio::time::Instant::now();
                        let _ = events.send(BridgeEvent::Msg(Msg::DaemonReplaced)).await;
                    }
                    Err(StartError::Outdated(message)) => {
                        stopped = true;
                        let _ = events.send(BridgeEvent::Msg(Msg::DaemonError { message })).await;
                        let _ = events.send(BridgeEvent::OfferTerminate).await;
                    }
                    Err(StartError::Failed(log_tail)) => {
                        stopped = true;
                        let _ = events.send(BridgeEvent::Msg(Msg::SpawnFailed { log_tail })).await;
                    }
                }
            }
            _ = tokio::time::sleep_until(next_connect), if stream.is_none() && spawn.task.is_none() && !stopped => {
                match subscribe(&env.paths).await {
                    Ok(mut candidate) => match tokio::time::timeout(Duration::from_secs(3), candidate.try_next()).await.unwrap_or_else(|_| Err(anyhow::anyhow!("subscription hello timed out"))) {
                        Ok(Some(Push::Hello { protocol, .. })) if protocol != 1 => {
                            stopped = true;
                            let _ = events.send(BridgeEvent::Msg(Msg::DaemonError {
                                message: format!("unsupported protocol {protocol}; supported: 1"),
                            })).await;
                            let _ = events.send(BridgeEvent::OfferTerminate).await;
                        }
                        Ok(Some(Push::Hello { protocol, daemon_version })) => {
                            match daemon_peer_pid(&env.paths).await {
                                Ok(daemon_pid) => {
                                    let _ = events.send(BridgeEvent::Msg(Msg::Hello { protocol, daemon_version, daemon_pid })).await;
                                    backoff = Backoff::default();
                                    stream = Some(Box::pin(candidate));
                                }
                                Err(error) => {
                                    let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: format!("{error:#}") })).await;
                                    next_connect = tokio::time::Instant::now() + backoff.next();
                                }
                            }
                        }
                        Ok(Some(Push::Error { message })) => {
                            if is_outdated(&message) {
                                stopped = true;
                                let _ = events.send(BridgeEvent::Msg(Msg::DaemonError { message: message.clone() })).await;
                                let _ = events.send(BridgeEvent::OfferTerminate).await;
                            } else {
                                let _ = events.send(BridgeEvent::Msg(Msg::CommandError { message })).await;
                            }
                            next_connect = tokio::time::Instant::now() + backoff.next();
                        }
                        Ok(Some(_)) | Ok(None) | Err(_) => {
                            let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: "subscription ended".to_string() })).await;
                            next_connect = tokio::time::Instant::now() + backoff.next();
                        }
                    },
                    Err(error) => {
                        if error.downcast_ref::<DaemonNotRunning>().is_some() && spawn.task.is_none() {
                            let _ = events.send(BridgeEvent::Msg(Msg::NotRunning)).await;
                            spawn.start(&env.paths, StartMode::Spawn);
                        } else {
                            let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: format!("{error:#}") })).await;
                            next_connect = tokio::time::Instant::now() + backoff.next();
                        }
                    }
                }
            }
            push = next_push(&mut stream), if stream.is_some() => {
                match push {
                    Some(Ok(Push::Snapshot(snapshot))) => {
                        let _ = events.send(BridgeEvent::Msg(Msg::Snapshot(snapshot))).await;
                    }
                    Some(Ok(Push::Level { rms, dbfs })) => {
                        let _ = events.send(BridgeEvent::Msg(Msg::Level(limen_core::Level { rms, dbfs }))).await;
                    }
                    Some(Ok(Push::Hello { .. })) => {}
                    Some(Ok(Push::Error { message })) => {
                        if is_outdated(&message) {
                            stopped = true;
                            let _ = events.send(BridgeEvent::Msg(Msg::DaemonError { message })).await;
                            let _ = events.send(BridgeEvent::OfferTerminate).await;
                        } else {
                            let _ = events.send(BridgeEvent::Msg(Msg::CommandError { message: message.clone() })).await;
                            let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: message })).await;
                            next_connect = tokio::time::Instant::now() + backoff.next();
                        }
                        stream = None;
                    }
                    Some(Err(error)) => {
                        let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: format!("{error:#}") })).await;
                        stream = None;
                        next_connect = tokio::time::Instant::now() + backoff.next();
                    }
                    None => {
                        let _ = events.send(BridgeEvent::Msg(Msg::Disconnected { reason: "daemon closed the connection".to_string() })).await;
                        stream = None;
                        next_connect = tokio::time::Instant::now() + backoff.next();
                    }
                }
            }
        }
    }
}

async fn next_push(
    stream: &mut Option<Pin<Box<dyn Stream<Item = Result<Push>>>>>,
) -> Option<Result<Push>> {
    match stream {
        Some(stream) => match stream.try_next().await {
            Ok(push) => push.map(Ok),
            Err(error) => Some(Err(error)),
        },
        None => std::future::pending().await,
    }
}

async fn handle_command(
    env: &Env,
    events: &mpsc::Sender<BridgeEvent>,
    command: BridgeCommand,
    stream: &mut Option<Pin<Box<dyn Stream<Item = Result<Push>>>>>,
    spawn: &mut SpawnGate,
    stopped: &mut bool,
    shortcuts: &mut ShortcutTasks,
) {
    let paths = &env.paths;
    match command {
        BridgeCommand::Start => match request_with_timeout(paths, &v1::Request::Start).await {
            Ok(v1::Reply::Accepted { .. }) => {}
            Ok(v1::Reply::Error { message }) => {
                let _ = events
                    .send(BridgeEvent::Msg(Msg::CommandError { message }))
                    .await;
            }
            Ok(_) => {
                let _ = events
                    .send(BridgeEvent::Msg(Msg::CommandError {
                        message: "unexpected start reply".to_string(),
                    }))
                    .await;
            }
            Err(error) => {
                let _ = events
                    .send(BridgeEvent::Msg(Msg::CommandError {
                        message: format!("{error:#}"),
                    }))
                    .await;
            }
        },
        BridgeCommand::Stop => {
            match request_with_timeout(
                paths,
                &v1::Request::Stop {
                    delivery: Delivery::Keep,
                },
            )
            .await
            {
                Ok(v1::Reply::Accepted { take }) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::StopAccepted { take }))
                        .await;
                }
                Ok(v1::Reply::Error { message }) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError { message }))
                        .await;
                }
                Ok(_) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: "unexpected stop reply".to_string(),
                        }))
                        .await;
                }
                Err(error) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: format!("{error:#}"),
                        }))
                        .await;
                }
            }
        }
        BridgeCommand::ClearHistory => {
            match request_with_timeout(paths, &v1::Request::ClearHistory).await {
                Ok(v1::Reply::Ok) => {}
                Ok(v1::Reply::Error { message }) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError { message }))
                        .await;
                }
                Ok(_) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: "unexpected clear_history reply".to_string(),
                        }))
                        .await;
                }
                Err(error) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: format!("{error:#}"),
                        }))
                        .await;
                }
            }
        }
        BridgeCommand::SpawnDaemon => {
            *stopped = false;
            spawn.start(paths, StartMode::Spawn);
        }
        BridgeCommand::RestartDaemon => {
            *stopped = false;
            spawn.start(paths, StartMode::Restart);
        }
        BridgeCommand::ShutdownDaemon => {
            match request_with_timeout(paths, &v1::Request::Shutdown).await {
                Ok(v1::Reply::Ok) => match wait_for_socket_removal(paths).await {
                    Ok(()) => {
                        *stopped = true;
                        *stream = None;
                        let _ = events.send(BridgeEvent::Msg(Msg::DaemonStopped)).await;
                    }
                    Err(StartError::Outdated(message) | StartError::Failed(message)) => {
                        let _ = events
                            .send(BridgeEvent::Msg(Msg::CommandError { message }))
                            .await;
                    }
                },
                Ok(v1::Reply::Error { message }) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::DaemonError { message }))
                        .await;
                    let _ = events.send(BridgeEvent::OfferTerminate).await;
                }
                Ok(_) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: "unexpected shutdown reply".to_string(),
                        }))
                        .await;
                }
                Err(error) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError {
                            message: format!("{error:#}"),
                        }))
                        .await;
                }
            }
        }
        BridgeCommand::TerminateDaemon => match terminate_daemon(paths).await {
            Ok(termination) => match wait_for_termination(paths, termination).await {
                Ok(()) => {
                    *stopped = true;
                    *stream = None;
                    let _ = events.send(BridgeEvent::Msg(Msg::DaemonStopped)).await;
                }
                Err(StartError::Outdated(message) | StartError::Failed(message)) => {
                    let _ = events
                        .send(BridgeEvent::Msg(Msg::CommandError { message }))
                        .await;
                }
            },
            Err(error) => {
                let _ = events
                    .send(BridgeEvent::Msg(Msg::CommandError {
                        message: format!("{error:#}"),
                    }))
                    .await;
            }
        },
        BridgeCommand::Shortcut(request) => {
            let paths = paths.clone();
            shortcuts.spawn(async move { run_shortcut(&paths, request).await });
        }
        BridgeCommand::LoadProvider { role, id } => {
            let (paths, events) = (paths.clone(), events.clone());
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || load_provider(&paths, role))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = events
                    .send(BridgeEvent::ProviderLoaded { role, id, result })
                    .await;
            });
        }
        BridgeCommand::SaveProvider { id, provider, key } => {
            // Off the loop, so snapshots and levels keep flowing meanwhile.
            let (paths, events) = (paths.clone(), events.clone());
            let role = match provider {
                Provider::Asr { .. } => ModelRole::Asr,
                Provider::Polish { .. } => ModelRole::Polish,
            };
            tokio::spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    save_provider(&paths, provider, key),
                )
                .await
                .map_err(|_| "daemon request timed out".to_string())
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = events
                    .send(BridgeEvent::ProviderSaved { role, id, result })
                    .await;
            });
        }
        BridgeCommand::ListModels {
            role,
            endpoint,
            provider,
            key,
        } => {
            let (paths, events) = (paths.clone(), events.clone());
            let backend = env.models.clone();
            tokio::spawn(async move {
                let models = list_models(&paths, &provider, &key, &*backend)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = events
                    .send(BridgeEvent::Models {
                        role,
                        endpoint,
                        models,
                    })
                    .await;
            });
        }
        BridgeCommand::Quit => unreachable!("quit is handled by the bridge loop"),
    }
}

async fn run_shortcut(paths: &Paths, request: ShortcutRequest) -> Result<Report, ShortcutFailure> {
    let result = match request {
        ShortcutRequest::Setup { backend, keys } => setup(paths, backend, keys, Launch::Gui).await,
        ShortcutRequest::Set { action, keys } => set(paths, action, keys).await,
        ShortcutRequest::Clear { action } => clear(paths, action).await,
        ShortcutRequest::Edit => edit(paths).await,
    };
    result.map_err(|error| {
        let message = match &error {
            SetupError::Usage { message, .. } => message.clone(),
            SetupError::Runtime { error, .. } => format!("{error:#}"),
        };
        ShortcutFailure {
            report: error.report().clone(),
            message,
        }
    })
}

#[cfg(test)]
mod tests;
