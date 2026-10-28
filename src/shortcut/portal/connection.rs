//! One generation of the portal session: a new session bus connection that
//! registers the app, creates a session, and binds the shortcuts, each call
//! made when the shortcut actor asks for it, and that reports results and
//! signals to the actor, tagged with its generation.
//!
//! A generation never ends on its own: after a failure it waits for `Stop`,
//! so the actor always hears the failure before the end. On `Stop` it closes
//! its request in flight and its session, then drops its connection, which
//! makes the portal release the shortcuts.

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::{Connection, Message, MessageStream};

use super::link::{LOST, Link, describe};
use crate::shortcut::Shortcut;
use crate::shortcut::status::{PerAction, Trigger};

/// Which session a report belongs to: every new setup counts one up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Generation(pub(super) u64);

/// Which bus the portal is on.
#[derive(Debug, Clone)]
pub(crate) enum Bus {
    Session,
    /// A private bus with a fake portal.
    #[cfg(test)]
    Address(String),
}

impl Bus {
    /// A new connection: the portal registers an app once per connection.
    pub(super) async fn connect(&self) -> zbus::Result<Connection> {
        match self {
            Self::Session => Connection::session().await,
            #[cfg(test)]
            Self::Address(address) => {
                zbus::connection::Builder::address(address.as_str())
                    .build()
                    .await
            }
        }
    }
}

/// A call the actor asks a generation to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Call {
    Register,
    CreateSession,
    Bind(PerAction<Shortcut>),
}

/// What the actor tells a generation.
#[derive(Debug)]
pub(super) enum Command {
    Call(Call),
    Stop,
}

/// What a generation reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Report {
    /// Connected and subscribed: ready for calls.
    Connected,
    Registered,
    SessionCreated,
    Bound(PerAction<Option<Trigger>>),
    Cancelled,
    Failed(String),
    Activated(String),
    Deactivated(String),
    Changed(PerAction<Option<Trigger>>),
    /// The portal that holds the session went away.
    PortalRestarted,
}

/// Run one generation until the actor stops it.
pub(super) async fn run(
    generation: Generation,
    bus: Bus,
    mut commands: mpsc::UnboundedReceiver<Command>,
    reports: mpsc::UnboundedSender<(Generation, Report)>,
) {
    let report = |report| {
        let _ = reports.send((generation, report));
    };
    // A rebind must not wait for a bus that is slow to answer.
    let opened = tokio::select! {
        opened = Link::open(&bus) => opened,
        command = commands.recv() => match command {
            Some(Command::Stop) | None => return,
            // The actor makes its first call once connected.
            Some(Command::Call(call)) => Err(anyhow::anyhow!("{call:?} before connecting")),
        },
    };
    let mut link = match opened {
        Ok(link) => {
            report(Report::Connected);
            link
        }
        Err(error) => {
            report(Report::Failed(describe(&error)));
            while let Some(Command::Call(_)) = commands.recv().await {}
            return;
        }
    };
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Call(call)) => {
                    // A stop does not wait for a slow call: a rebind waits for the stop.
                    let outcome = tokio::select! {
                        outcome = link.call(call) => outcome,
                        command = commands.recv() => match command {
                            Some(Command::Stop) | None => return link.close().await,
                            // The actor makes one call at a time.
                            Some(Command::Call(call)) => Err(anyhow::anyhow!("{call:?} during a call")),
                        },
                    };
                    match outcome {
                        Ok(Some(done)) => report(done),
                        Ok(None) => {}
                        Err(error) => report(Report::Failed(describe(&error))),
                    }
                }
                Some(Command::Stop) | None => return link.close().await,
            },
            message = next(link.pending.as_mut().map(|pending| &mut pending.responses)) => {
                report(link.response(message).await);
            }
            message = next(link.signals.as_mut()) => match message {
                Some(Ok(message)) => {
                    if let Some(signal) = link.signal(&message) {
                        report(signal);
                    }
                }
                Some(Err(_)) | None => {
                    link.signals = None;
                    report(Report::Failed(LOST.to_string()));
                }
            },
            message = next(link.owners.as_mut()) => match message {
                Some(Ok(message)) => {
                    if let Some(change) = link.owner_changed(&message) {
                        report(change);
                    }
                }
                Some(Err(_)) | None => {
                    link.owners = None;
                    report(Report::Failed(LOST.to_string()));
                }
            },
            _ = next(link.closed.as_mut()) => {
                link.closed = None;
                link.session = None;
                report(Report::Failed("the portal closed the session".to_string()));
            }
        }
    }
}

/// The next message of `stream`; never, without one.
async fn next(stream: Option<&mut MessageStream>) -> Option<zbus::Result<Message>> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}
