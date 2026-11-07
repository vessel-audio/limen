//! Daemon startup and the common shutdown/cleanup path.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};

use super::Projection;
use super::input;
use super::{DictationActor, Mailboxes};
use crate::backend::ModelBackend;
use crate::config::{Config, Paths};
use crate::dictation::Event;
use crate::ipc;
use crate::notify::notify;
use crate::session::SessionHandle;
use crate::shortcut::ShortcutHandle;
use crate::usage::Meter;

pub(crate) struct ActorExit {
    pub(super) result: Result<()>,
    pub(super) session: SessionHandle,
    pub(super) shortcuts: ShortcutHandle,
}

/// Run the resident daemon with the discovered paths and loaded
/// configuration; `backend` builds every model client it uses.
///
/// Sync because macOS needs the calling (main) thread: Carbon hotkey events
/// only dispatch while the main thread pumps its event loop, so there the
/// tokio runtime moves to a worker thread and the main thread runs the pump.
/// On Linux this is a plain runtime wrapper.
pub fn run(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    run_platform(paths, cfg, backend)
}

/// The tokio runtime goes on a worker thread; the main thread pumps the
/// Carbon event loop, without which no hotkey event is ever dispatched.
#[cfg(target_os = "macos")]
fn run_platform(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    let pump = crate::shortcut::carbon::install_event_pump();
    let worker_done = Arc::new(AtomicBool::new(false));
    let thread_done = worker_done.clone();
    let worker = std::thread::Builder::new()
        .name("limen-runtime".into())
        .spawn(move || {
            let result = run_runtime(paths, cfg, backend);
            thread_done.store(true, Ordering::SeqCst);
            result
        })
        .context("failed to spawn the daemon runtime thread")?;
    pump.run(worker_done);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("the daemon runtime thread panicked"))?
}

#[cfg(not(target_os = "macos"))]
fn run_platform(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    run_runtime(paths, cfg, backend)
}

/// Build the tokio runtime and block on the daemon.
fn run_runtime(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build the daemon runtime")?
        .block_on(run_async(paths, cfg, backend))
}

async fn run_async(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    let mut socket = ipc::bind(&paths)?;
    let (cmd_tx, commands) = mpsc::channel::<ipc::Command>(8);
    let (polish, polish_rx) = watch::channel(cfg.polish.clone());
    let meter = Meter::open(&paths);
    let (input_tx, input_reports, input_worker) =
        input::spawn(&cfg, polish_rx, backend.clone(), meter.clone())?;

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (activation_tx, activations) = mpsc::unbounded_channel();
    let shortcuts = crate::shortcut::spawn_shortcuts(cfg.shortcut, activation_tx, notify);
    let (session_report_tx, session_reports) = mpsc::unbounded_channel::<Event>();
    let (projection, snapshots) = Projection::new(&cfg);
    let (session, levels) = SessionHandle::spawn(cfg, backend, meter, session_report_tx);
    let shortcut_watch = shortcuts.subscribe();
    let actor = DictationActor::new(
        session,
        input_tx,
        polish,
        notify,
        paths.config_file.clone(),
        shortcuts,
        projection,
    );

    // Do not accept until every actor and snapshot exists.
    let watchers = ipc::Watchers::new(snapshots, shortcut_watch, levels);
    let accept = ipc::spawn_accept_loop(
        socket.listener.take().expect("bound socket listener"),
        cmd_tx.clone(),
        watchers,
    );
    notify("Limen", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    let exit = actor
        .run(Mailboxes {
            commands,
            input_reports,
            session_reports,
            activations,
        })
        .await;
    let accept = crate::task::join_task("accept", accept, crate::task::StopMode::Abort).await;
    // The actor dropped the input queue on exit; the worker drains or times out.
    crate::task::join_task("input", input_worker, crate::task::StopMode::Graceful).await;
    exit.session.stop().await;
    exit.shortcuts.stop().await;
    drop(cmd_tx);
    let accept = match accept {
        crate::task::JoinOutcome::Joined => Ok(()),
        crate::task::JoinOutcome::Abandoned => Err(anyhow::anyhow!(
            "accept loop did not stop; socket left in place"
        )),
    };
    accept?;
    socket.identity.remove_if_owned(&paths)?;
    exit.result
}
