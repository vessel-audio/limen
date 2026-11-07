//! The Carbon backend: the shortcut actor registers the configured hotkeys
//! with the macOS Carbon hotkey manager and turns them into dictation input.
//!
//! Actor model, mirroring the portal backend but simpler — there is no
//! connection to lose, so a rebind is one registration round instead of a
//! session lifecycle:
//! - Mailbox: rebind requests sent through a [`ShortcutHandle`].
//! - Reports: activations go to the dictation loop's mailbox from the
//!   forwarder thread; the status is published on a watch channel.
//!
//! Registration conflicts need the user to pick another key, so there is no
//! retry loop: every rebind is already a retry.

mod convert;
mod events;
mod registrar;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

pub(crate) use registrar::install_event_pump;

use crate::config::ShortcutConfig;
use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus};
use crate::shortcut::{Action, ActivationReceipt, Backend};

/// A request to apply a `[shortcut]` section; `done` is answered once its
/// state is published.
struct Rebind {
    config: ShortcutConfig,
    done: oneshot::Sender<()>,
}

/// Address of the shortcut actor. Dropping it stops the actor.
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
        activations: mpsc::UnboundedSender<ActivationReceipt>,
        notify: fn(&str, &str),
    ) -> Self {
        let (mailbox, rebinds) = mpsc::unbounded_channel();
        let (status_tx, status) = watch::channel(initial_status(config));
        let actor = ShortcutActor {
            rebinds,
            status: status_tx,
            activations,
            notify,
            forwarder: None,
        };
        let handle = Self {
            mailbox,
            status,
            task: None,
        };
        // The daemon's own config is the first rebind; nobody waits for it.
        drop(handle.rebind(config));
        Self {
            task: Some(tokio::spawn(actor.run())),
            ..handle
        }
    }

    /// Apply `config`. The receiver resolves once the actor has published
    /// the state that follows from it, so a status read afterwards is never
    /// the one before the rebind.
    pub(crate) fn rebind(&self, config: ShortcutConfig) -> oneshot::Receiver<()> {
        let (done, rebound) = oneshot::channel();
        if let Err(error) = self.mailbox.send(Rebind { config, done }) {
            tracing::warn!(config = ?error.0.config, "shortcut actor is gone");
        }
        rebound
    }

    /// Close the actor mailbox and join the actor.
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
        let (_, status) = watch::channel(initial_status(ShortcutConfig::default()));
        Self {
            mailbox,
            status,
            task: None,
        }
    }
}

/// The actor's private state, owned by its task for its whole life.
struct ShortcutActor {
    rebinds: mpsc::UnboundedReceiver<Rebind>,
    status: watch::Sender<ShortcutStatus>,
    activations: mpsc::UnboundedSender<ActivationReceipt>,
    notify: fn(&str, &str),
    /// Present while the Carbon backend owns the hotkeys.
    forwarder: Option<events::EventForwarder>,
}

impl ShortcutActor {
    async fn run(mut self) {
        while let Some(Rebind { config, done }) = self.rebinds.recv().await {
            self.rebind(config).await;
            let _ = done.send(());
        }
        // Every handle is gone: the daemon is shutting down. Registration
        // ends with the process; just stop forwarding.
        self.stop_forwarder().await;
    }

    async fn rebind(&mut self, config: ShortcutConfig) {
        match config.backend {
            Backend::Carbon => self.bind(config).await,
            _ => {
                self.stop_forwarder().await;
                self.publish(status(&config, SessionState::Disabled, None));
            }
        }
    }

    async fn bind(&mut self, config: ShortcutConfig) {
        let keys = [(Action::Toggle, config.toggle), (Action::Hold, config.hold)];
        let hotkeys = match keys
            .into_iter()
            .map(|(action, shortcut)| convert::hotkey(shortcut).map(|hotkey| (action, hotkey)))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(hotkeys) => hotkeys,
            Err(error) => {
                self.publish(status(
                    &config,
                    SessionState::Failed,
                    Some(error.to_string()),
                ));
                return;
            }
        };
        let registrar = match registrar::registrar() {
            Ok(registrar) => registrar,
            Err(error) => {
                self.publish(status(&config, SessionState::Failed, Some(error)));
                return;
            }
        };
        match registrar.register(hotkeys).await {
            Ok(()) => {
                self.start_forwarder();
                let mut bound = status(&config, SessionState::Bound, None);
                bound.actual = PerAction {
                    toggle: Some(convert::describe(config.toggle)),
                    hold: Some(convert::describe(config.hold)),
                };
                self.publish(bound);
            }
            Err(error) => {
                self.publish(status(&config, SessionState::Failed, Some(error.clone())));
                (self.notify)("Limen", &format!("global shortcuts: {error}"));
            }
        }
    }

    fn start_forwarder(&mut self) {
        if self.forwarder.is_some() {
            return;
        }
        match registrar::bindings() {
            Some(bindings) => {
                let stop = Arc::new(AtomicBool::new(false));
                self.forwarder = Some(events::EventForwarder::spawn(
                    bindings,
                    stop,
                    self.activations.clone(),
                ));
            }
            None => tracing::warn!("hotkey event pump is not installed; no activations"),
        }
    }

    async fn stop_forwarder(&mut self) {
        if let Ok(registrar) = registrar::registrar() {
            registrar.unregister_all().await;
        }
        if let Some(forwarder) = self.forwarder.take() {
            forwarder.stop();
        }
    }

    fn publish(&self, status: ShortcutStatus) {
        self.status.send_replace(status);
    }
}

/// The status for `config` in `session`, before anything is bound.
fn status(
    config: &ShortcutConfig,
    session: SessionState,
    last_error: Option<String>,
) -> ShortcutStatus {
    ShortcutStatus {
        backend: config.backend,
        session,
        preferred: PerAction {
            toggle: config.toggle,
            hold: config.hold,
        },
        actual: PerAction::default(),
        last_error,
    }
}

/// What clients see while the actor's first rebind is still queued.
fn initial_status(config: ShortcutConfig) -> ShortcutStatus {
    let session = match config.backend {
        Backend::Carbon => SessionState::Binding,
        _ => SessionState::Disabled,
    };
    status(&config, session, None)
}

#[cfg(test)]
mod tests {
    use super::{SessionState, initial_status, status};
    use crate::config::ShortcutConfig;
    use crate::shortcut::Backend;

    #[test]
    fn carbon_config_starts_binding() {
        let config = ShortcutConfig {
            backend: Backend::Carbon,
            ..ShortcutConfig::default()
        };
        assert_eq!(initial_status(config).session, SessionState::Binding);
    }

    #[test]
    fn other_backends_start_disabled() {
        let config = ShortcutConfig::default();
        assert_eq!(
            initial_status(config),
            status(&config, SessionState::Disabled, None)
        );
    }
}
