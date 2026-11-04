//! Daemon startup and the common shutdown/cleanup path.

use std::sync::Arc;

use anyhow::Result;
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
use crate::shortcut::portal::{Bus, ShortcutHandle};
use crate::usage::Meter;

pub(crate) struct ActorExit {
    pub(super) result: Result<()>,
    pub(super) session: SessionHandle,
    pub(super) shortcuts: ShortcutHandle,
}

/// Run the resident daemon with the discovered paths and loaded
/// configuration; `backend` builds every model client it uses.
pub async fn run(paths: Paths, cfg: Config, backend: Arc<dyn ModelBackend>) -> Result<()> {
    let mut socket = ipc::bind(&paths)?;
    let (cmd_tx, commands) = mpsc::channel::<ipc::Command>(8);
    let (polish, polish_rx) = watch::channel(cfg.polish.clone());
    let meter = Meter::open(&paths);
    let (input_tx, input_reports, input_worker) =
        input::spawn(&cfg, polish_rx, backend.clone(), meter.clone())?;

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (activation_tx, activations) = mpsc::unbounded_channel();
    let shortcuts = ShortcutHandle::spawn(cfg.shortcut, Bus::Session, activation_tx, notify);
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
