//! One generation of the portal session: a new session bus connection that
//! registers the app, creates a session, and binds the shortcuts, each call
//! made when the shortcut actor asks for it, and that reports results and
//! signals to the actor, tagged with its generation.
//!
//! A generation never ends on its own: after a failure it waits for `Stop`,
//! so the actor always hears the failure before the end. On `Stop` it closes
//! its request in flight and its session, then drops its connection, which
//! makes the portal release the shortcuts.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::message::Type;
use zbus::{Connection, MatchRule, Message, MessageStream, OwnedObjectPath, Proxy, Variant};

use super::protocol::{
    self, GLOBAL_SHORTCUTS, Outcome, OwnerChange, PORTAL, PORTAL_PATH, REGISTRY, REQUEST, SESSION,
    Shortcuts, Vardict,
};
use crate::shortcut::Shortcut;
use crate::shortcut::desktop_entry::APP_ID;
use crate::shortcut::status::{PerAction, Trigger};

/// How long a call may take. The confirmation dialog that a bind can raise
/// is not a call: its response is waited for as long as the dialog is open.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long closing a generation may take before its connection is dropped
/// anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

const LOST: &str = "lost the session bus connection";

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
    async fn connect(&self) -> zbus::Result<Connection> {
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
    let mut link = match Link::open(&bus).await {
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

/// `error` and its causes, each once: zbus repeats a D-Bus error as its cause.
fn describe(error: &anyhow::Error) -> String {
    let mut causes: Vec<String> = error.chain().map(ToString::to_string).collect();
    causes.dedup();
    causes.join(": ")
}

/// The next message of `stream`; never, without one.
async fn next(stream: Option<&mut MessageStream>) -> Option<zbus::Result<Message>> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// A request in flight, and the subscription to its response.
struct Pending {
    path: OwnedObjectPath,
    kind: Request,
    responses: MessageStream,
}

#[derive(Debug, Clone)]
enum Request {
    /// `session`: the path the session will have.
    CreateSession {
        session: String,
    },
    BindShortcuts,
}

impl Request {
    fn method(&self) -> &'static str {
        match self {
            Self::CreateSession { .. } => "CreateSession",
            Self::BindShortcuts => "BindShortcuts",
        }
    }
}

/// The connection of one generation and what it subscribed to.
struct Link {
    connection: Connection,
    unique_name: String,
    /// The portal's owner when last seen: the session lives in it.
    owner: Option<String>,
    owners: Option<MessageStream>,
    signals: Option<MessageStream>,
    session: Option<OwnedObjectPath>,
    /// `Closed` of the session, once there is one.
    closed: Option<MessageStream>,
    pending: Option<Pending>,
    tokens: u32,
}

impl Link {
    /// Connect, watch the portal's owner before learning who it is, so that
    /// no change in between goes unnoticed, and subscribe to the shortcut
    /// signals before any session exists.
    async fn open(bus: &Bus) -> Result<Self> {
        let connection = bus
            .connect()
            .await
            .context("cannot connect to the session bus")?;
        let unique_name = connection
            .unique_name()
            .context("the session bus gave no unique name")?
            .to_string();
        let owners = subscribe(
            &connection,
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")
                .interface("org.freedesktop.DBus")
                .member("NameOwnerChanged")
                .arg(0, PORTAL),
        )
        .await?;
        let owner = zbus::fdo::DBusProxy::new(&connection)
            .await?
            .get_name_owner(PORTAL.try_into()?)
            .await
            .ok()
            .map(|owner| owner.to_string());
        let signals = subscribe(
            &connection,
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(PORTAL)
                .interface(GLOBAL_SHORTCUTS)
                .path(PORTAL_PATH),
        )
        .await?;
        Ok(Self {
            connection,
            unique_name,
            owner,
            owners: Some(owners),
            signals: Some(signals),
            session: None,
            closed: None,
            pending: None,
            tokens: 0,
        })
    }

    /// Make `call`; what it reports right away, if anything.
    async fn call(&mut self, call: Call) -> Result<Option<Report>> {
        match call {
            Call::Register => {
                tokio::time::timeout(
                    CALL_TIMEOUT,
                    self.proxy(PORTAL_PATH, REGISTRY)
                        .await?
                        .call::<_, _, ()>("Register", &(APP_ID, Vardict::new())),
                )
                .await
                .context("the portal did not answer Register")?
                .context("the portal did not register the app")?;
                Ok(Some(Report::Registered))
            }
            Call::CreateSession => {
                let session_token = self.token();
                let session = protocol::session_path(&self.unique_name, &session_token);
                // Like a response, a session closed right away must not be missed.
                self.closed = Some(self.closed_signal(&session).await?);
                self.request(
                    Request::CreateSession { session },
                    |proxy, token| async move {
                        let options = HashMap::from([
                            ("handle_token", Variant::from(token)),
                            ("session_handle_token", Variant::from(session_token)),
                        ]);
                        proxy.call("CreateSession", &(options,)).await
                    },
                )
                .await?;
                Ok(None)
            }
            Call::Bind(preferred) => {
                let session = self.session.clone().context("no session to bind to")?;
                self.request(Request::BindShortcuts, |proxy, token| async move {
                    let options = HashMap::from([("handle_token", Variant::from(token))]);
                    let shortcuts = protocol::bind_argument(preferred);
                    proxy
                        .call("BindShortcuts", &(session, shortcuts, "", options))
                        .await
                })
                .await?;
                Ok(None)
            }
        }
    }

    /// Subscribe to the response of the request that `call` creates with a
    /// new token, then make the call: a portal may respond before the call
    /// returns.
    async fn request<F>(
        &mut self,
        kind: Request,
        call: impl FnOnce(Proxy<'static>, String) -> F,
    ) -> Result<()>
    where
        F: Future<Output = zbus::Result<OwnedObjectPath>>,
    {
        let method = kind.method();
        let token = self.token();
        let path = protocol::request_path(&self.unique_name, &token);
        let responses = self.responses(&path).await?;
        let proxy = self.proxy(PORTAL_PATH, GLOBAL_SHORTCUTS).await?;
        let handle = tokio::time::timeout(CALL_TIMEOUT, call(proxy, token))
            .await
            .with_context(|| format!("the portal did not answer {method}"))?
            .with_context(|| format!("{method} failed"))?;
        // A portal older than the request path convention responds elsewhere.
        let responses = match handle.as_str() == path {
            true => responses,
            false => self.responses(handle.as_str()).await?,
        };
        self.pending = Some(Pending {
            path: handle,
            kind,
            responses,
        });
        Ok(())
    }

    async fn closed_signal(&self, path: &str) -> Result<MessageStream> {
        subscribe(
            &self.connection,
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(PORTAL)
                .interface(SESSION)
                .member("Closed")
                .path(path),
        )
        .await
    }

    async fn responses(&self, path: &str) -> Result<MessageStream> {
        subscribe(
            &self.connection,
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(PORTAL)
                .interface(REQUEST)
                .member("Response")
                .path(path),
        )
        .await
    }

    /// What the response to the pending request says.
    async fn response(&mut self, message: Option<zbus::Result<Message>>) -> Report {
        let Some(pending) = self.pending.take() else {
            return Report::Failed("a response without a request".to_string());
        };
        let body = match message {
            Some(Ok(message)) => message.body().deserialize::<(u32, Vardict)>(),
            Some(Err(_)) | None => return Report::Failed(LOST.to_string()),
        };
        let Ok((code, results)) = body else {
            return Report::Failed(format!(
                "the portal sent a malformed {} response",
                pending.kind.method()
            ));
        };
        match pending.kind {
            Request::CreateSession { session: expected } => {
                match protocol::created_session(code, &results) {
                    Outcome::Done(session) => {
                        // A portal older than the path convention put it elsewhere.
                        if session.as_str() != expected {
                            match self.closed_signal(session.as_str()).await {
                                Ok(closed) => self.closed = Some(closed),
                                Err(error) => return Report::Failed(describe(&error)),
                            }
                        }
                        self.session = Some(session);
                        Report::SessionCreated
                    }
                    Outcome::Cancelled => Report::Cancelled,
                    Outcome::Failed(error) => Report::Failed(error),
                }
            }
            Request::BindShortcuts => match protocol::bound(code, &results) {
                Outcome::Done(triggers) => Report::Bound(triggers),
                Outcome::Cancelled => Report::Cancelled,
                Outcome::Failed(error) => Report::Failed(error),
            },
        }
    }

    /// What a shortcut signal says, if it is about our session.
    fn signal(&self, message: &Message) -> Option<Report> {
        let ours = |session: &OwnedObjectPath| self.session.as_ref() == Some(session);
        let body = message.body();
        match message.header().member()?.as_str() {
            "Activated" => body
                .deserialize::<(OwnedObjectPath, String, u64, Vardict)>()
                .ok()
                .filter(|(session, ..)| ours(session))
                .map(|(_, id, ..)| Report::Activated(id)),
            "Deactivated" => body
                .deserialize::<(OwnedObjectPath, String, u64, Vardict)>()
                .ok()
                .filter(|(session, ..)| ours(session))
                .map(|(_, id, ..)| Report::Deactivated(id)),
            "ShortcutsChanged" => body
                .deserialize::<(OwnedObjectPath, Shortcuts)>()
                .ok()
                .filter(|(session, _)| ours(session))
                .map(|(_, shortcuts)| Report::Changed(protocol::triggers(&shortcuts))),
            _ => None,
        }
    }

    fn owner_changed(&mut self, message: &Message) -> Option<Report> {
        let (_, old, new) = message
            .body()
            .deserialize::<(String, String, String)>()
            .ok()?;
        match protocol::owner_change(self.owner.as_deref(), &old, &new) {
            OwnerChange::Restarted => Some(Report::PortalRestarted),
            OwnerChange::Appeared(owner) => {
                self.owner = Some(owner);
                None
            }
            OwnerChange::Unrelated => None,
        }
    }

    /// Close the request in flight, then the session; dropping the
    /// connection afterwards releases whatever is left.
    async fn close(self) {
        let close = async {
            if let Some(pending) = &self.pending {
                let _ = self.close_object(&pending.path, REQUEST).await;
            }
            if let Some(session) = &self.session {
                let _ = self.close_object(session, SESSION).await;
            }
        };
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, close).await;
    }

    async fn close_object(&self, path: &OwnedObjectPath, interface: &str) -> Result<()> {
        self.proxy(path.as_str(), interface)
            .await?
            .call::<_, _, ()>("Close", &())
            .await?;
        Ok(())
    }

    async fn proxy(&self, path: &str, interface: &str) -> Result<Proxy<'static>> {
        Ok(Proxy::new(
            &self.connection,
            PORTAL,
            path.to_string(),
            interface.to_string(),
        )
        .await?)
    }

    fn token(&mut self) -> String {
        self.tokens += 1;
        format!("vi_{}", self.tokens)
    }
}

async fn subscribe(
    connection: &Connection,
    rule: zbus::match_rule::Builder<'_>,
) -> Result<MessageStream> {
    Ok(MessageStream::for_match_rule(rule.build()?, connection, None).await?)
}
