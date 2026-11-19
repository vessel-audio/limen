//! Starting, stopping, restarting, and replacing the daemon, and the daemon
//! problems the window reports. Each test replays the events the bridge sends
//! for the situation, in the order its run loop sends them, and reads the
//! commands the window sends back. The Linux desktop suite keeps the
//! whole chain against a real daemon, and the start that fails on a broken
//! config, whose log opens in the desktop's handler: the test platform cannot
//! open files.

use gpui_kit::TestAppContext;
use limen_core::{Backend, SessionState, Shortcut};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent};
use crate::model::Msg;

/// A daemon too old to know the subscription, as it answers one.
const OUTDATED: &str = "invalid request: unknown variant `subscribe`";

/// The title bar's connection status, and the message of the overlay that
/// covers the page while there is no daemon to talk to.
fn connection(ui: &mut Ui) -> (String, Option<String>) {
    (
        ui.label("connection-status"),
        ui.try_find("connection-overlay")
            .and_then(|overlay| overlay.label().map(str::to_string)),
    )
}

/// A daemon of `version` says hello and sends its first snapshot.
fn greeted(ui: &mut Ui, version: &str) {
    ui.receive(BridgeEvent::Msg(Msg::Hello {
        protocol: limen_core::v1::VERSION,
        daemon_version: version.to_string(),
        daemon_pid: 1,
    }));
    ui.publish(|_| {});
}

/// Stopping asks first: cancelling keeps the daemon and confirming asks the
/// bridge to shut it down. Once the bridge reports it stopped, the overlay
/// offers Start, which asks the bridge to spawn one; the new daemon's hello
/// and snapshot lift the overlay.
#[gpui_kit::test]
fn stopping_asks_first_and_start_brings_it_back(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    ui.click("stop-daemon");
    let asked = (ui.label("shutdown-confirmation"), ui.sent());
    ui.click("cancel-shutdown");
    let cancelled = (
        ui.try_find("shutdown-confirmation").is_none(),
        connection(&mut ui),
        ui.sent(),
    );
    ui.click("stop-daemon");
    ui.click("confirm-shutdown");
    let confirmed = (ui.try_find("shutdown-confirmation").is_none(), ui.sent());
    ui.receive(BridgeEvent::Msg(Msg::DaemonStopped));
    let stopped = connection(&mut ui);
    ui.click("start-daemon");
    let started = ui.sent();
    ui.receive(BridgeEvent::Msg(Msg::DaemonReplaced));
    let starting = connection(&mut ui);
    greeted(&mut ui, env!("CARGO_PKG_VERSION"));
    let online = t!("connection.online").to_string();
    let offline = t!("connection.offline").to_string();
    assert_eq!(
        (
            asked,
            cancelled,
            confirmed,
            stopped,
            started,
            starting,
            connection(&mut ui)
        ),
        (
            (t!("connection.stop_confirm").into(), Vec::new()),
            (true, (online.clone(), None), Vec::new()),
            (true, vec![BridgeCommand::ShutdownDaemon]),
            (offline.clone(), Some(t!("connection.not_running").into())),
            vec![BridgeCommand::SpawnDaemon],
            (offline, Some(t!("connection.starting").into())),
            (online, None),
        )
    );
}

/// Restart asks the bridge to replace the daemon. The old one's connection
/// ends first, which the window reports as reconnecting. Once the bridge says
/// it replaced the daemon, the window forgets what it held of the old one: a
/// key typed into a form and a backend picked from the selector give way to
/// the new daemon's.
#[gpui_kit::test]
fn restarting_replaces_the_daemon(cx: &mut TestAppContext) {
    let mut daemon = Daemon::default();
    daemon.shortcut.backend = Backend::Portal;
    daemon.shortcut.session = SessionState::Bound;
    let mut ui = Ui::connected(cx, daemon);
    ui.click("shortcuts-page");
    ui.retype("toggle-input", "Super+Y");
    let typed = ui.find("toggle-input").value().map(str::to_string);
    ui.click("backend-none");
    let edited = (
        typed,
        ui.try_find("toggle-input").is_some(),
        ui.try_find("copy-toggle-command").is_some(),
    );
    ui.click("settings-page");
    ui.click("restart-daemon");
    let restarted = ui.sent();
    ui.receive(BridgeEvent::Msg(Msg::Disconnected {
        reason: "daemon closed the connection".to_string(),
    }));
    let reconnecting = ui.label("reconnecting-banner");
    ui.receive(BridgeEvent::Msg(Msg::DaemonReplaced));
    let starting = connection(&mut ui);
    greeted(&mut ui, env!("CARGO_PKG_VERSION"));
    ui.click("shortcuts-page");
    let forms = (
        ui.find("toggle-input").value().map(str::to_string),
        ui.try_find("copy-toggle-command").is_some(),
        connection(&mut ui),
    );
    assert_eq!(
        (edited, restarted, reconnecting, starting, forms),
        (
            (Some("Super+Y".to_string()), false, true),
            vec![BridgeCommand::RestartDaemon],
            format!(
                "{} 1… · daemon closed the connection",
                t!("connection.reconnecting")
            ),
            (
                t!("connection.offline").into(),
                Some(t!("connection.starting").into())
            ),
            (
                Some(Shortcut::DEFAULT_TOGGLE.to_string()),
                false,
                (t!("connection.online").into(), None)
            ),
        )
    );
}

/// A daemon that cannot start leaves the window waiting while it spawns, then
/// shows the tail of its log; starting again asks the bridge to spawn once
/// more, and a second failure shows the same.
#[gpui_kit::test]
fn a_failed_start_shows_the_log_and_can_be_retried(cx: &mut TestAppContext) {
    let log_tail = "invalid config: expected a table".to_string();
    let mut ui = Ui::open(cx);
    ui.receive(BridgeEvent::Msg(Msg::NotRunning));
    let spawning = connection(&mut ui);
    ui.receive(BridgeEvent::Msg(Msg::SpawnFailed {
        log_tail: log_tail.clone(),
    }));
    let failed = |ui: &mut Ui| {
        (
            connection(ui),
            ui.find("connection-overlay")
                .description()
                .map(str::to_string),
            ui.try_find("open-startup-log").is_some(),
        )
    };
    let failure = failed(&mut ui);
    ui.click("start-daemon");
    let retried = ui.sent();
    ui.receive(BridgeEvent::Msg(Msg::SpawnFailed {
        log_tail: log_tail.clone(),
    }));
    assert_eq!(
        (spawning, failure, retried, failed(&mut ui)),
        (
            (
                t!("connection.offline").into(),
                Some(t!("connection.starting").into())
            ),
            (
                (
                    t!("connection.offline").into(),
                    Some(t!("connection.failed_start").into())
                ),
                Some(log_tail.clone()),
                true
            ),
            vec![BridgeCommand::SpawnDaemon],
            (
                (
                    t!("connection.offline").into(),
                    Some(t!("connection.failed_start").into())
                ),
                Some(log_tail),
                true
            ),
        )
    );
}

/// An outdated daemon is named in a banner, which offers to terminate it
/// once the bridge says it may; terminating asks for confirmation, cancelling
/// keeps the daemon, and confirming asks the bridge to terminate it.
#[gpui_kit::test]
fn an_outdated_daemon_can_be_terminated(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    ui.receive(BridgeEvent::Msg(Msg::DaemonError {
        message: OUTDATED.to_string(),
    }));
    let named = (
        ui.label("outdated-banner"),
        ui.try_find("terminate-daemon").is_some(),
    );
    ui.receive(BridgeEvent::OfferTerminate);
    let offered = ui.try_find("terminate-daemon").is_some();
    ui.click("terminate-daemon");
    let asked = (ui.label("shutdown-confirmation"), ui.sent());
    ui.click("cancel-shutdown");
    let cancelled = (
        ui.try_find("shutdown-confirmation").is_none(),
        ui.try_find("outdated-banner").is_some(),
        ui.sent(),
    );
    ui.click("terminate-daemon");
    ui.click("confirm-shutdown");
    let confirmed = (ui.try_find("shutdown-confirmation").is_none(), ui.sent());
    ui.receive(BridgeEvent::Msg(Msg::DaemonStopped));
    assert_eq!(
        (
            named,
            offered,
            asked,
            cancelled,
            confirmed,
            connection(&mut ui)
        ),
        (
            (format!("{}: {OUTDATED}", t!("connection.outdated")), false),
            true,
            (t!("connection.terminate_confirm").into(), Vec::new()),
            (true, true, Vec::new()),
            (true, vec![BridgeCommand::TerminateDaemon]),
            (
                t!("connection.offline").into(),
                Some(t!("connection.not_running").into())
            ),
        )
    );
}

/// A daemon of another version is named in a banner, and the window stays
/// connected to it.
#[gpui_kit::test]
fn a_mismatched_daemon_version_is_named(cx: &mut TestAppContext) {
    let mut ui = Ui::open(cx);
    greeted(&mut ui, "0.0.1");
    assert_eq!(
        (ui.label("version-banner"), connection(&mut ui)),
        (
            format!(
                "{} 0.0.1; {} {}",
                t!("connection.daemon_version"),
                t!("connection.gui_expects"),
                env!("CARGO_PKG_VERSION")
            ),
            (t!("connection.online").into(), None)
        )
    );
}
