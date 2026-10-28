//! The session bus connection of one generation: what it subscribed to,
//! the calls it makes, and what the portal's responses and signals mean for
//! its session.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::message::Type;
use zbus::{Connection, MatchRule, Message, MessageStream, OwnedObjectPath, Proxy, Variant};

use super::connection::{Bus, Call, Report};
use super::protocol::{
    self, GLOBAL_SHORTCUTS, Outcome, OwnerChange, PORTAL, PORTAL_PATH, REGISTRY, REQUEST, SESSION,
    Shortcuts, Vardict,
};
use crate::shortcut::desktop_entry::APP_ID;

/// How long a call may take. The confirmation dialog that a bind can raise
/// is not a call: its response is waited for as long as the dialog is open.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long closing a generation may take before its connection is dropped
/// anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) const LOST: &str = "lost the session bus connection";

/// `error` and its causes, each once: zbus repeats a D-Bus error as its cause.
pub(super) fn describe(error: &anyhow::Error) -> String {
    let mut causes: Vec<String> = error.chain().map(ToString::to_string).collect();
    causes.dedup();
    causes.join(": ")
}

/// A request in flight, and the subscription to its response.
pub(super) struct Pending {
    path: OwnedObjectPath,
    kind: Request,
    pub(super) responses: MessageStream,
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
pub(super) struct Link {
    connection: Connection,
    unique_name: String,
    /// The portal's owner when last seen: the session lives in it.
    owner: Option<String>,
    pub(super) owners: Option<MessageStream>,
    pub(super) signals: Option<MessageStream>,
    pub(super) session: Option<OwnedObjectPath>,
    /// `Closed` of the session, once there is one.
    pub(super) closed: Option<MessageStream>,
    pub(super) pending: Option<Pending>,
    tokens: u32,
}

impl Link {
    /// Connect, watch the portal's owner before learning who it is, so that
    /// no change in between goes unnoticed, and subscribe to the shortcut
    /// signals before any session exists.
    pub(super) async fn open(bus: &Bus) -> Result<Self> {
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
    pub(super) async fn call(&mut self, call: Call) -> Result<Option<Report>> {
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
    pub(super) async fn response(&mut self, message: Option<zbus::Result<Message>>) -> Report {
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
    pub(super) fn signal(&self, message: &Message) -> Option<Report> {
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

    pub(super) fn owner_changed(&mut self, message: &Message) -> Option<Report> {
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
    pub(super) async fn close(self) {
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

    /// A proxy that never reads properties: Register must be the first call
    /// the portal sees from this connection.
    async fn proxy(&self, path: &str, interface: &str) -> Result<Proxy<'static>> {
        Ok(zbus::proxy::Builder::new(&self.connection)
            .destination(PORTAL)
            .path(path.to_string())
            .interface(interface.to_string())
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
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
