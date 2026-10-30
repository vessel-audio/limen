//! The shortcut actor against a fake portal on a private D-Bus daemon: the
//! races and lifecycle that unit tests of the state machine cannot show.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant, Variant, fdo, interface};

use super::protocol::{PORTAL, PORTAL_PATH, request_path};
use super::{Bus, ShortcutHandle};
use crate::config::ShortcutConfig;
use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus, Trigger};
use crate::shortcut::{Activation, ActivationReceipt, Backend, HoldEdge};

const WAIT: Duration = Duration::from_secs(10);

/// A D-Bus daemon of the test's own, without any activatable service: a
/// real portal must never start on it.
struct PrivateBus {
    daemon: Child,
    address: String,
    dir: PathBuf,
}

impl PrivateBus {
    async fn start(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("voice-input-portal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}/bus</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
                dir.display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address"])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("dbus-daemon is needed for the portal tests (nixpkgs dbus)");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .await
            .unwrap();
        Self {
            daemon,
            address: address.trim().to_string(),
            dir,
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How the fake portal answers.
#[derive(Debug, Clone)]
struct Script {
    register_error: Option<String>,
    /// The code BindShortcuts responds with; `None` keeps its dialog open.
    bind_response: Option<u32>,
    /// Hand out request handles that do not follow the portal's path convention.
    alternate_handles: bool,
    /// How long BindShortcuts waits before its method returns.
    bind_delay: Option<Duration>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            register_error: None,
            bind_response: Some(0),
            alternate_handles: false,
            bind_delay: None,
        }
    }
}

type Log = Arc<Mutex<Vec<String>>>;

struct Registry {
    log: Log,
    script: Arc<Mutex<Script>>,
}

#[interface(name = "org.freedesktop.host.portal.Registry")]
impl Registry {
    async fn register(
        &self,
        app_id: String,
        _options: HashMap<String, OwnedVariant>,
    ) -> fdo::Result<()> {
        self.log.lock().unwrap().push(format!("Register {app_id}"));
        let error = self.script.lock().unwrap().register_error.clone();
        match error {
            Some(error) => Err(fdo::Error::Failed(error)),
            None => Ok(()),
        }
    }
}

struct GlobalShortcuts {
    log: Log,
    script: Arc<Mutex<Script>>,
}

#[interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl GlobalShortcuts {
    async fn create_session(
        &self,
        options: HashMap<String, OwnedVariant>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        let sender = header.sender().unwrap().to_string();
        let token = |key: &str| String::try_from(options[key].clone()).unwrap();
        let request = self.request_path(&sender, &token("handle_token"));
        let session = format!(
            "{PORTAL_PATH}/session/{}/{}",
            sender.trim_start_matches(':').replace('.', "_"),
            token("session_handle_token")
        );
        self.serve(connection, &request, &session).await;
        self.log
            .lock()
            .unwrap()
            .push(format!("CreateSession {session}"));
        // Respond before the call returns: the client must have subscribed.
        respond(
            connection,
            &request,
            0,
            HashMap::from([("session_handle", Variant::from(session))]),
        )
        .await;
        Ok(OwnedObjectPath::try_from(request).unwrap())
    }

    async fn bind_shortcuts(
        &self,
        _session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedVariant>)>,
        _parent_window: String,
        options: HashMap<String, OwnedVariant>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        let sender = header.sender().unwrap().to_string();
        let token = String::try_from(options["handle_token"].clone()).unwrap();
        let request = self.request_path(&sender, &token);
        self.serve(connection, &request, "").await;
        let ids: Vec<String> = shortcuts.iter().map(|(id, _)| id.clone()).collect();
        self.log
            .lock()
            .unwrap()
            .push(format!("BindShortcuts {} {request}", ids.join(",")));
        let response = self.script.lock().unwrap().bind_response;
        if let Some(code) = response {
            let bound: Vec<(String, HashMap<String, Variant<'static>>)> =
                [("toggle", "z"), ("hold", "x")]
                    .map(|(id, key)| {
                        (
                            id.to_string(),
                            HashMap::from([(
                                "trigger_description".to_string(),
                                Variant::from(format!("Press <Super>{key}")),
                            )]),
                        )
                    })
                    .into();
            respond(
                connection,
                &request,
                code,
                HashMap::from([("shortcuts", Variant::from(bound))]),
            )
            .await;
        }
        let delay = self.script.lock().unwrap().bind_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(OwnedObjectPath::try_from(request).unwrap())
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcut_id: &str,
        timestamp: u64,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcut_id: &str,
        timestamp: u64,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn shortcuts_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: ObjectPath<'_>,
        shortcuts: Vec<(String, HashMap<String, Variant<'_>>)>,
    ) -> zbus::Result<()>;
}

impl GlobalShortcuts {
    fn request_path(&self, sender: &str, token: &str) -> String {
        match self.script.lock().unwrap().alternate_handles {
            true => format!("{PORTAL_PATH}/request/elsewhere/{token}"),
            false => request_path(sender, token),
        }
    }

    /// Serve the Request at `request` and, unless empty, the Session at
    /// `session`.
    async fn serve(&self, connection: &Connection, request: &str, session: &str) {
        let server = connection.object_server();
        server
            .at(
                request.to_string(),
                Closable {
                    log: Arc::clone(&self.log),
                    what: format!("Request.Close {request}"),
                },
            )
            .await
            .unwrap();
        if !session.is_empty() {
            server
                .at(
                    session.to_string(),
                    ClosableSession {
                        log: Arc::clone(&self.log),
                        what: format!("Session.Close {session}"),
                    },
                )
                .await
                .unwrap();
        }
    }
}

async fn respond(
    connection: &Connection,
    request: &str,
    code: u32,
    results: HashMap<&str, Variant<'_>>,
) {
    let emitter = SignalEmitter::new(connection, request.to_string()).unwrap();
    Closable::response(&emitter, code, results).await.unwrap();
}

/// A portal Request: logs `Close`.
struct Closable {
    log: Log,
    what: String,
}

#[interface(name = "org.freedesktop.portal.Request")]
impl Closable {
    async fn close(&self) {
        self.log.lock().unwrap().push(self.what.clone());
    }

    #[zbus(signal)]
    async fn response(
        emitter: &SignalEmitter<'_>,
        response: u32,
        results: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;
}

/// A portal Session: logs `Close`.
struct ClosableSession {
    log: Log,
    what: String,
}

#[interface(name = "org.freedesktop.portal.Session")]
impl ClosableSession {
    async fn close(&self) {
        self.log.lock().unwrap().push(self.what.clone());
    }

    #[zbus(signal)]
    async fn closed(
        emitter: &SignalEmitter<'_>,
        details: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;
}

/// The fake portal, owning the portal's well-known name on the private bus.
struct FakePortal {
    connection: Connection,
}

impl FakePortal {
    async fn start(bus: &PrivateBus, log: &Log, script: &Arc<Mutex<Script>>) -> Self {
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .serve_at(
                PORTAL_PATH,
                Registry {
                    log: Arc::clone(log),
                    script: Arc::clone(script),
                },
            )
            .serve_at(
                PORTAL_PATH,
                GlobalShortcuts {
                    log: Arc::clone(log),
                    script: Arc::clone(script),
                },
            )
            .name(PORTAL)
            .build()
            .await
            .unwrap();
        Self { connection }
    }

    fn emitter(&self) -> SignalEmitter<'_> {
        SignalEmitter::new(&self.connection, PORTAL_PATH).unwrap()
    }

    async fn activate(&self, session: &str, id: &str) {
        let session = ObjectPath::try_from(session).unwrap();
        GlobalShortcuts::activated(&self.emitter(), session, id, 0, HashMap::new())
            .await
            .unwrap();
    }

    async fn deactivate(&self, session: &str, id: &str) {
        let session = ObjectPath::try_from(session).unwrap();
        GlobalShortcuts::deactivated(&self.emitter(), session, id, 0, HashMap::new())
            .await
            .unwrap();
    }

    /// Close `session` from the portal's side.
    async fn close(&self, session: &str) {
        let emitter = SignalEmitter::new(&self.connection, session.to_string()).unwrap();
        ClosableSession::closed(&emitter, HashMap::new())
            .await
            .unwrap();
    }

    async fn change(&self, session: &str, description: &str) {
        let session = ObjectPath::try_from(session).unwrap();
        let shortcuts = vec![(
            "toggle".to_string(),
            HashMap::from([(
                "trigger_description".to_string(),
                Variant::from(description.to_string()),
            )]),
        )];
        GlobalShortcuts::shortcuts_changed(&self.emitter(), session, shortcuts)
            .await
            .unwrap();
    }
}

/// The actor under test, on the private bus.
struct Client {
    handle: ShortcutHandle,
    activations: mpsc::UnboundedReceiver<ActivationReceipt>,
}

impl Client {
    fn start(bus: &PrivateBus) -> Self {
        let (activations_tx, activations) = mpsc::unbounded_channel();
        let config = ShortcutConfig {
            backend: Backend::Portal,
            ..ShortcutConfig::default()
        };
        let handle = ShortcutHandle::spawn(
            config,
            Bus::Address(bus.address.clone()),
            activations_tx,
            |_, _| {},
        );
        Self {
            handle,
            activations,
        }
    }

    /// Wait until the session reaches `state`.
    async fn reaches(&self, state: SessionState) {
        let wait = async {
            while self.handle.status().session != state {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .unwrap_or_else(|_| panic!("never {state:?}: {:?}", self.handle.status()));
    }

    async fn next_activation(&mut self) -> Activation {
        tokio::time::timeout(WAIT, self.activations.recv())
            .await
            .expect("no activation")
            .unwrap()
            .activation
    }
}

/// The logged lines that start with `prefix`.
fn logged(log: &Log, prefix: &str) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with(prefix))
        .cloned()
        .collect()
}

/// Each logged call, without the object paths it names.
fn calls(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .map(|line| {
            line.split(' ')
                .filter(|word| !word.starts_with('/'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The path of the `index`th session created.
fn session(log: &Log, index: usize) -> String {
    logged(log, "CreateSession ")[index]
        .trim_start_matches("CreateSession ")
        .to_string()
}

/// Wait until `log` holds `count` lines starting with `prefix`.
async fn logs(log: &Log, prefix: &str, count: usize) {
    let wait = async {
        while logged(log, prefix).len() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(WAIT, wait)
        .await
        .unwrap_or_else(|_| panic!("{prefix} logged fewer than {count} times: {log:?}"));
}

fn expected_status(
    backend: Backend,
    session: SessionState,
    actual: PerAction<Option<Trigger>>,
) -> ShortcutStatus {
    ShortcutStatus {
        backend,
        session,
        preferred: PerAction {
            toggle: ShortcutConfig::default().toggle,
            hold: ShortcutConfig::default().hold,
        },
        actual,
        last_error: None,
    }
}

fn setup() -> (Log, Arc<Mutex<Script>>) {
    (Log::default(), Arc::new(Mutex::new(Script::default())))
}

#[tokio::test]
async fn a_response_sent_before_the_call_returns_still_binds() {
    let bus = PrivateBus::start("bind").await;
    let (log, script) = setup();
    let portal = FakePortal::start(&bus, &log, &script).await;
    let mut client = Client::start(&bus);
    client.reaches(SessionState::Bound).await;
    assert_eq!(
        client.handle.status(),
        expected_status(
            Backend::Portal,
            SessionState::Bound,
            PerAction {
                toggle: Some(Trigger::new("Press <Super>z".to_string())),
                hold: Some(Trigger::new("Press <Super>x".to_string())),
            }
        )
    );
    assert_eq!(
        calls(&log),
        [
            "Register io.github.Emin017.VoiceInput",
            "CreateSession",
            "BindShortcuts toggle,hold"
        ]
    );

    let session = session(&log, 0);
    portal.activate(&session, "toggle").await;
    portal.activate(&session, "hold").await;
    portal.deactivate(&session, "toggle").await;
    portal.deactivate(&session, "hold").await;
    assert_eq!(
        [
            client.next_activation().await,
            client.next_activation().await,
            client.next_activation().await
        ],
        [
            Activation::Toggle,
            Activation::Hold(HoldEdge::Press),
            Activation::Hold(HoldEdge::Release),
        ]
    );

    portal.change(&session, "Press <Super>F9").await;
    let wait = async {
        while client
            .handle
            .status()
            .actual
            .toggle
            .map(|trigger| trigger.description)
            != Some("Press <Super>F9".to_string())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(WAIT, wait).await.unwrap();
}

#[tokio::test]
async fn a_rebind_closes_the_old_session_and_ignores_its_signals() {
    let bus = PrivateBus::start("rebind").await;
    let (log, script) = setup();
    let portal = FakePortal::start(&bus, &log, &script).await;
    let mut client = Client::start(&bus);
    client.reaches(SessionState::Bound).await;
    let old = session(&log, 0);

    let config = ShortcutConfig {
        backend: Backend::Portal,
        ..ShortcutConfig::default()
    };
    tokio::time::timeout(WAIT, client.handle.rebind(config))
        .await
        .unwrap()
        .unwrap();
    logs(&log, "CreateSession ", 2).await;
    client.reaches(SessionState::Bound).await;
    let new = session(&log, 1);
    let lines = log.lock().unwrap().clone();
    let closed = lines
        .iter()
        .position(|line| *line == format!("Session.Close {old}"))
        .expect("the old session was not closed");
    let created = lines
        .iter()
        .position(|line| *line == format!("CreateSession {new}"))
        .unwrap();
    assert!(closed < created, "{lines:?}");

    portal.activate(&old, "toggle").await;
    portal.activate(&new, "hold").await;
    assert_eq!(
        client.next_activation().await,
        Activation::Hold(HoldEdge::Press)
    );
}

#[tokio::test]
async fn a_rebind_closes_the_request_whose_dialog_is_open() {
    let bus = PrivateBus::start("pending").await;
    let (log, script) = setup();
    script.lock().unwrap().bind_response = None;
    let _portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    logs(&log, "BindShortcuts ", 1).await;
    client.reaches(SessionState::Binding).await;
    let request = logged(&log, "BindShortcuts ")[0]
        .rsplit(' ')
        .next()
        .unwrap()
        .to_string();

    script.lock().unwrap().bind_response = Some(0);
    let config = ShortcutConfig {
        backend: Backend::Portal,
        ..ShortcutConfig::default()
    };
    tokio::time::timeout(WAIT, client.handle.rebind(config))
        .await
        .unwrap()
        .unwrap();
    client.reaches(SessionState::Bound).await;
    let old = session(&log, 0);
    assert_eq!(
        [
            logged(&log, "Request.Close "),
            logged(&log, "Session.Close ")
        ],
        [
            vec![format!("Request.Close {request}")],
            vec![format!("Session.Close {old}")]
        ]
    );
}

#[tokio::test]
async fn a_restarted_portal_gets_the_shortcuts_bound_again() {
    let bus = PrivateBus::start("restart").await;
    let (log, script) = setup();
    let portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    client.reaches(SessionState::Bound).await;

    drop(portal);
    client.reaches(SessionState::Failed).await;
    let _portal = FakePortal::start(&bus, &log, &script).await;
    logs(&log, "CreateSession ", 2).await;
    client.reaches(SessionState::Bound).await;
}

#[tokio::test]
async fn a_failed_registration_never_creates_a_session() {
    let bus = PrivateBus::start("register").await;
    let (log, script) = setup();
    script.lock().unwrap().register_error = Some("no desktop entry".to_string());
    let _portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    client.reaches(SessionState::Failed).await;
    let error = client.handle.status().last_error.unwrap();
    assert!(error.contains("no desktop entry"), "{error}");
    assert_eq!(logged(&log, "CreateSession"), Vec::<String>::new());
}

#[tokio::test]
async fn a_cancelled_dialog_is_not_asked_again() {
    let bus = PrivateBus::start("cancel").await;
    let (log, script) = setup();
    script.lock().unwrap().bind_response = Some(1);
    let _portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    client.reaches(SessionState::Cancelled).await;
    tokio::time::sleep(super::machine::FIRST_RETRY * 2).await;
    assert_eq!(
        (
            client.handle.status().session,
            logged(&log, "BindShortcuts ").len()
        ),
        (SessionState::Cancelled, 1)
    );
}

#[tokio::test]
async fn a_session_the_portal_closes_is_set_up_again() {
    let bus = PrivateBus::start("closed").await;
    let (log, script) = setup();
    let portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    client.reaches(SessionState::Bound).await;

    portal.close(&session(&log, 0)).await;
    client.reaches(SessionState::Failed).await;
    logs(&log, "CreateSession ", 2).await;
    client.reaches(SessionState::Bound).await;
}

#[tokio::test]
async fn a_portal_that_exits_with_its_dialog_open_is_asked_again() {
    let bus = PrivateBus::start("exit-pending").await;
    let (log, script) = setup();
    script.lock().unwrap().bind_response = None;
    let portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    logs(&log, "BindShortcuts ", 1).await;

    script.lock().unwrap().bind_response = Some(0);
    drop(portal);
    let _portal = FakePortal::start(&bus, &log, &script).await;
    logs(&log, "BindShortcuts ", 2).await;
    client.reaches(SessionState::Bound).await;
}

#[tokio::test]
async fn a_bus_that_never_answers_does_not_hold_up_a_rebind() {
    let dir = std::env::temp_dir().join(format!("voice-input-portal-mute-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("bus");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    // Accept connections and keep them open without a word.
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let (activations_tx, _activations) = mpsc::unbounded_channel();
    let portal = ShortcutConfig {
        backend: Backend::Portal,
        ..ShortcutConfig::default()
    };
    let handle = ShortcutHandle::spawn(
        portal,
        Bus::Address(format!("unix:path={}", socket.display())),
        activations_tx,
        |_, _| {},
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    let none = ShortcutConfig::default();
    tokio::time::timeout(Duration::from_secs(1), handle.rebind(none))
        .await
        .expect("the rebind waited for the bus")
        .unwrap();
    assert_eq!(
        handle.status(),
        expected_status(Backend::None, SessionState::Disabled, PerAction::default())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn responses_on_unexpected_request_paths_are_heard() {
    let bus = PrivateBus::start("alternate").await;
    let (log, script) = setup();
    script.lock().unwrap().alternate_handles = true;
    let _portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    // The portal responds on its own path, before the method returns.
    client.reaches(SessionState::Bound).await;
}

#[tokio::test]
async fn a_rebind_during_a_slow_call_closes_the_request_and_the_session() {
    let bus = PrivateBus::start("slow-call").await;
    let (log, script) = setup();
    script.lock().unwrap().bind_delay = Some(Duration::from_secs(3));
    let _portal = FakePortal::start(&bus, &log, &script).await;
    let client = Client::start(&bus);
    logs(&log, "BindShortcuts ", 1).await;
    let request = logged(&log, "BindShortcuts ")[0]
        .rsplit(' ')
        .next()
        .unwrap()
        .to_string();
    let session = session(&log, 0);

    script.lock().unwrap().bind_delay = None;
    let config = ShortcutConfig {
        backend: Backend::Portal,
        ..ShortcutConfig::default()
    };
    tokio::time::timeout(WAIT, client.handle.rebind(config))
        .await
        .unwrap()
        .unwrap();
    client.reaches(SessionState::Bound).await;
    assert_eq!(
        [
            logged(&log, "Request.Close "),
            logged(&log, "Session.Close ")
        ],
        [
            vec![format!("Request.Close {request}")],
            vec![format!("Session.Close {session}")]
        ]
    );
}
