//! The portal backend: the shortcut actor binds the global shortcuts through
//! the GlobalShortcuts portal and turns them into dictation input.
//!
//! Actor model:
//! - State: the [`Machine`] that decides everything, and the task of the
//!   generation that holds the portal connection, if one runs. A portal
//!   session lives as long as its D-Bus connection, so the daemon holds it.
//! - Mailbox: rebind requests sent through a [`ShortcutHandle`], the
//!   reports of the generation, the end of its task, and the retry timer,
//!   handled one at a time.
//! - Reports: shortcut activations go to the dictation loop's mailbox; the
//!   status is published on a watch channel for IPC to read.
//!
//! A rebind closes the old session, waits for its task to end, and only then
//! starts the new generation, so no two generations hold a connection at
//! once and no stale signal reaches dictation.

mod connection;
mod link;
mod machine;
mod protocol;
#[cfg(test)]
mod tests;

use std::pin::Pin;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

pub(crate) use connection::Bus;
use connection::{Command, Generation, Report};
use machine::{Input, Machine, Output};

use super::ActivationReceipt;
use super::status::ShortcutStatus;
use crate::config::ShortcutConfig;

/// A request to apply a `[shortcut]` section; `done` is answered once its
/// state is published.
struct Rebind {
    config: ShortcutConfig,
    done: oneshot::Sender<()>,
}

/// Address of the shortcut actor. Dropping it stops the actor, which closes
/// the portal session.
pub(crate) struct ShortcutHandle {
    mailbox: mpsc::UnboundedSender<Rebind>,
    status: watch::Receiver<ShortcutStatus>,
    task: Option<JoinHandle<()>>,
}

impl ShortcutHandle {
    /// Spawn the actor for `config`. Activations go to `activations`;
    /// `notify` shows a desktop notification as `(summary, body)`.
    pub(crate) fn spawn(
        config: ShortcutConfig,
        bus: Bus,
        activations: mpsc::UnboundedSender<ActivationReceipt>,
        notify: fn(&str, &str),
    ) -> Self {
        let (mailbox, rebinds) = mpsc::unbounded_channel();
        let (reports_tx, reports) = mpsc::unbounded_channel();
        let machine = Machine::default();
        let (status_tx, status) = watch::channel(machine::initial_status(config));
        let actor = ShortcutActor {
            machine,
            bus,
            rebinds,
            reports_tx,
            reports,
            running: None,
            retry: None,
            status: status_tx,
            activations,
            notify,
            rebound: Vec::new(),
        };
        let handle = Self {
            mailbox,
            status,
            task: None,
        };
        // The daemon's own config is the first rebind; nobody waits for it.
        drop(handle.rebind(config));
        let task = tokio::spawn(actor.run());
        Self {
            task: Some(task),
            ..handle
        }
    }

    /// Apply `config`. The receiver resolves once the actor has published
    /// the state that follows from it, such as `binding`, so a status read
    /// afterwards is never the one before the rebind.
    pub(crate) fn rebind(&self, config: ShortcutConfig) -> oneshot::Receiver<()> {
        let (done, rebound) = oneshot::channel();
        if let Err(error) = self.mailbox.send(Rebind { config, done }) {
            tracing::warn!(config = ?error.0.config, "shortcut actor is gone");
        }
        rebound
    }

    /// Close the actor mailbox and join the portal session owner.
    pub(crate) async fn stop(self) {
        let Self { mailbox, task, .. } = self;
        drop(mailbox);
        if let Some(task) = task {
            crate::task::join_task("shortcut", task, crate::task::StopMode::Graceful).await;
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<ShortcutStatus> {
        self.status.clone()
    }

    pub(crate) fn status(&self) -> ShortcutStatus {
        self.status.borrow().clone()
    }
}

#[cfg(test)]
impl ShortcutHandle {
    /// A handle without an actor: rebinds go nowhere, the status stays
    /// disabled.
    pub(crate) fn detached() -> Self {
        let (mailbox, _) = mpsc::unbounded_channel();
        let (_, status) = watch::channel(machine::initial_status(ShortcutConfig::default()));
        Self {
            mailbox,
            status,
            task: None,
        }
    }
}

/// The generation that holds the portal connection.
struct Running {
    generation: Generation,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

/// The actor's private state, owned by its task for its whole life.
struct ShortcutActor {
    machine: Machine,
    bus: Bus,
    rebinds: mpsc::UnboundedReceiver<Rebind>,
    reports_tx: mpsc::UnboundedSender<(Generation, Report)>,
    reports: mpsc::UnboundedReceiver<(Generation, Report)>,
    running: Option<Running>,
    retry: Option<Pin<Box<tokio::time::Sleep>>>,
    status: watch::Sender<ShortcutStatus>,
    activations: mpsc::UnboundedSender<ActivationReceipt>,
    notify: fn(&str, &str),
    /// Rebinds whose state is not published yet.
    rebound: Vec<oneshot::Sender<()>>,
}

impl ShortcutActor {
    async fn run(mut self) {
        loop {
            let input = tokio::select! {
                rebind = self.rebinds.recv() => match rebind {
                    Some(Rebind { config, done }) => {
                        self.rebound.push(done);
                        Input::Rebind(config)
                    }
                    // Every handle is gone: the daemon is shutting down.
                    None => return self.stop().await,
                },
                Some((generation, report)) = self.reports.recv() => Input::Report(generation, report),
                generation = async {
                    match self.running.as_mut() {
                        Some(running) => {
                            let _ = (&mut running.task).await;
                            running.generation
                        }
                        None => std::future::pending().await,
                    }
                } => {
                    self.running = None;
                    Input::Ended(generation)
                }
                _ = async { self.retry.as_mut().unwrap().await }, if self.retry.is_some() => {
                    self.retry = None;
                    Input::RetryDue
                }
            };
            for output in self.machine.handle(input) {
                self.perform(output);
            }
        }
    }

    fn perform(&mut self, output: Output) {
        match output {
            Output::Connect(generation) => {
                let (commands, receiver) = mpsc::unbounded_channel();
                let task = tokio::spawn(connection::run(
                    generation,
                    self.bus.clone(),
                    receiver,
                    self.reports_tx.clone(),
                ));
                self.running = Some(Running {
                    generation,
                    commands,
                    task,
                });
            }
            Output::Call(generation, call) => self.tell(generation, Command::Call(call)),
            Output::Stop(generation) => self.tell(generation, Command::Stop),
            Output::Publish(status) => {
                self.status.send_replace(status);
            }
            Output::Rebound => {
                for done in self.rebound.drain(..) {
                    let _ = done.send(());
                }
            }
            Output::Activate(activation) => {
                tracing::debug!(?activation, "global shortcut");
                let _ = self.activations.send(ActivationReceipt {
                    activation,
                    received_at: tokio::time::Instant::now(),
                });
            }
            Output::Notify(error) => {
                (self.notify)("Limen error", &format!("global shortcuts: {error}"));
            }
            Output::RetryAfter(wait) => {
                self.retry = Some(Box::pin(tokio::time::sleep(wait)));
            }
        }
    }

    fn tell(&self, generation: Generation, command: Command) {
        match &self.running {
            Some(running) if running.generation == generation => {
                let _ = running.commands.send(command);
            }
            Some(_) | None => tracing::debug!(?generation, ?command, "generation is gone"),
        }
    }

    /// Close the session before the actor ends.
    async fn stop(self) {
        if let Some(running) = self.running {
            let _ = running.commands.send(Command::Stop);
            let _ = running.task.await;
        }
    }
}
