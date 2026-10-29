//! Daemon startup and the common shutdown/cleanup path.

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use super::Projection;
use super::input;
use super::{DictationActor, Mailboxes};
use crate::config::{Config, Paths};
use crate::dictation::Event;
use crate::ipc;
use crate::notify::notify;
use crate::session::SessionHandle;
use crate::shortcut::portal::{Bus, ShortcutHandle};

pub(crate) struct ActorExit {
    pub(super) result: Result<()>,
    pub(super) session: SessionHandle,
    pub(super) shortcuts: ShortcutHandle,
}

/// Run the resident daemon with the discovered paths and loaded configuration.
pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let (cmd_tx, commands) = mpsc::channel::<ipc::Command>(8);
    let (input_tx, input_reports, input_worker) = input::spawn(&cfg)?;

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (activation_tx, activations) = mpsc::unbounded_channel();
    let shortcuts = ShortcutHandle::spawn(cfg.shortcut, Bus::Session, activation_tx, notify);
    let (session_report_tx, session_reports) = mpsc::unbounded_channel::<Event>();
    let (projection, _snapshots) = Projection::new(&cfg);
    let (session, levels) = SessionHandle::spawn(cfg, session_report_tx);
    let actor = DictationActor::new(
        session,
        input_tx,
        notify,
        paths.config_file.clone(),
        shortcuts,
        projection,
    );

    // Do not accept until every actor and snapshot exists.
    let accept = ipc::spawn_accept_loop(listener, cmd_tx.clone());
    notify("voice-input", "daemon started");
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
    match std::fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to remove socket {}", paths.socket.display()));
        }
    }
    drop(levels);
    exit.result
}
