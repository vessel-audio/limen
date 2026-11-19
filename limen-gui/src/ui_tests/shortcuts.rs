//! The shortcuts page: each backend's form, the keys applying one sends, the
//! key recorder, and the reports the bridge sends back.
//!
//! The page shows the form of the backend the daemon reports until the user
//! picks another, so tests choose a backend through the daemon and mean the
//! same on every platform; only the selector's own buttons differ, and those
//! are tested per platform. What the shortcut operations then do to the
//! config file and to GNOME's settings belongs to the bridge and the core.
//! The Linux desktop suite keeps one case that records a key for GNOME and
//! reads GNOME's real settings back.

use std::path::PathBuf;

use gpui_kit::TestAppContext;
use limen_core::{Action, Backend, Keys, Note, PerAction, Report, SessionState, Shortcut};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent, ShortcutRequest};

/// The elements the page may show, which together tell the forms apart.
const FORM_IDS: [&str; 5] = [
    "copy-toggle-command",
    "edit-portal-shortcuts",
    "record-toggle",
    "toggle-input",
    "hold-input",
];

const NONE_FORM: &[&str] = &["copy-toggle-command"];
const PORTAL_FORM: &[&str] = &["edit-portal-shortcuts", "toggle-input", "hold-input"];
const GNOME_FORM: &[&str] = &["record-toggle", "toggle-input"];
const CARBON_FORM: &[&str] = &["toggle-input", "hold-input"];

/// The selector's buttons on this platform, each with the form it opens.
#[cfg(target_os = "linux")]
const SELECTOR: [(&str, &[&str]); 3] = [
    ("backend-portal", PORTAL_FORM),
    ("backend-gnome", GNOME_FORM),
    ("backend-none", NONE_FORM),
];

/// macOS: Carbon registers the keys with the daemon directly, and the GNOME
/// keybinding backend needs a GNOME session.
#[cfg(target_os = "macos")]
const SELECTOR: [(&str, &[&str]); 2] =
    [("backend-carbon", CARBON_FORM), ("backend-none", NONE_FORM)];

/// A daemon whose shortcut backend is `backend`, in `session`.
fn daemon(backend: Backend, session: SessionState) -> Daemon {
    let mut daemon = Daemon::default();
    daemon.shortcut.backend = backend;
    daemon.shortcut.session = session;
    daemon
}

fn shortcut(text: &str) -> Shortcut {
    text.parse().expect("a shortcut")
}

/// The shortcuts page, with `daemon` connected.
fn open(cx: &mut TestAppContext, daemon: Daemon) -> Ui<'_> {
    let mut ui = Ui::connected(cx, daemon);
    ui.click("shortcuts-page");
    ui
}

/// Which of the form elements the page shows.
fn form(ui: &mut Ui) -> Vec<&'static str> {
    FORM_IDS
        .into_iter()
        .filter(|id| ui.try_find(id).is_some())
        .collect()
}

/// Have the daemon report this backend, as it does once the user has set
/// one up elsewhere.
fn report_backend(ui: &mut Ui, backend: Backend) {
    ui.daemon.shortcut.backend = backend;
    ui.publish(|_| {});
}

/// The request to set a backend up with both keys, as the page sends it.
fn setup(backend: Backend, toggle: Shortcut, hold: Shortcut) -> Vec<BridgeCommand> {
    vec![BridgeCommand::Shortcut(ShortcutRequest::Setup {
        backend,
        keys: Keys {
            toggle: Some(toggle),
            hold: Some(hold),
        },
    })]
}

/// Without a user's pick, the page shows the form of the backend the daemon
/// reports and follows it when it changes.
#[gpui_kit::test]
fn the_page_follows_the_daemons_backend(cx: &mut TestAppContext) {
    let mut ui = open(cx, daemon(Backend::None, SessionState::Disabled));
    let mut forms = vec![form(&mut ui)];
    for backend in [
        Backend::Portal,
        Backend::GnomeKeybinding,
        Backend::Carbon,
        Backend::None,
    ] {
        report_backend(&mut ui, backend);
        forms.push(form(&mut ui));
    }
    assert_eq!(
        forms,
        [NONE_FORM, PORTAL_FORM, GNOME_FORM, CARBON_FORM, NONE_FORM]
    );
}

/// Each of the selector's buttons shows its backend's form, and the pick
/// holds when the daemon's next snapshot still reports another backend.
#[gpui_kit::test]
fn the_backend_selector_swaps_the_form(cx: &mut TestAppContext) {
    let mut ui = open(cx, daemon(Backend::None, SessionState::Disabled));
    let shown = SELECTOR.map(|(button, _)| {
        ui.click(button);
        let picked = form(&mut ui);
        ui.publish(|_| {});
        (button, picked, form(&mut ui))
    });
    assert_eq!(
        shown,
        SELECTOR.map(|(button, expected)| (button, expected.to_vec(), expected.to_vec()))
    );
}

/// Without a backend, the page offers the command to bind by hand, which
/// copies without asking the daemon anything.
#[gpui_kit::test]
fn copying_the_toggle_command(cx: &mut TestAppContext) {
    let mut ui = open(cx, Daemon::default());
    ui.click("copy-toggle-command");
    assert_eq!(
        (ui.clipboard(), ui.sent()),
        (Some("limen toggle".to_string()), Vec::new())
    );
}

/// Applying the portal backend sends both typed keys, and the report the
/// bridge sends back names the desktop entry it installed.
#[gpui_kit::test]
fn applying_the_portal_backend_sends_the_typed_keys(cx: &mut TestAppContext) {
    let mut ui = open(cx, daemon(Backend::Portal, SessionState::Disabled));
    ui.retype("toggle-input", "Super+Y");
    ui.retype("hold-input", "Super+U");
    ui.click("apply-shortcut-backend");
    let sent = ui.sent();
    let entry = PathBuf::from("/data/applications/io.github.Emin017.Limen.desktop");
    let mut report = Report::default();
    report.notes.push(Note::Installed(entry.clone()));
    ui.receive(BridgeEvent::ShortcutReport(Ok(report)));
    assert_eq!(
        (sent, ui.label("shortcut-report")),
        (
            setup(Backend::Portal, shortcut("Super+Y"), shortcut("Super+U")),
            format!("{} {}", t!("note.installed"), entry.display())
        )
    );
}

/// Each backend applies the keys it binds, the daemon's preferred ones while
/// the fields are untouched: both for the portal and Carbon, the toggle alone
/// for GNOME, and none when the user binds the command by hand.
#[gpui_kit::test]
fn applying_a_backend_sends_the_keys_it_binds(cx: &mut TestAppContext) {
    let mut ui = open(cx, Daemon::default());
    let PerAction { toggle, hold } = ui.daemon.shortcut.preferred;
    let sent = [
        Backend::Portal,
        Backend::GnomeKeybinding,
        Backend::Carbon,
        Backend::None,
    ]
    .map(|backend| {
        report_backend(&mut ui, backend);
        ui.click("apply-shortcut-backend");
        ui.sent()
    });
    let keys = |backend, keys| {
        vec![BridgeCommand::Shortcut(ShortcutRequest::Setup {
            backend,
            keys,
        })]
    };
    assert_eq!(
        sent,
        [
            setup(Backend::Portal, toggle, hold),
            keys(
                Backend::GnomeKeybinding,
                Keys {
                    toggle: Some(toggle),
                    hold: None
                }
            ),
            setup(Backend::Carbon, toggle, hold),
            keys(
                Backend::None,
                Keys {
                    toggle: None,
                    hold: None
                }
            ),
        ]
    );
}

/// A shortcut that cannot be parsed is reported instead of applied.
#[gpui_kit::test]
fn an_unparsable_portal_shortcut_is_reported(cx: &mut TestAppContext) {
    let mut ui = open(cx, daemon(Backend::Portal, SessionState::Disabled));
    ui.retype("toggle-input", "Super+");
    ui.click("apply-shortcut-backend");
    let error = "Super+"
        .parse::<Shortcut>()
        .expect_err("no key")
        .to_string();
    assert_eq!(
        (ui.sent(), ui.label("shortcut-report")),
        (Vec::new(), error)
    );
}

/// Once the portal is set up, editing its shortcuts asks the bridge to open
/// this app's page in GNOME Settings.
#[gpui_kit::test]
fn editing_portal_shortcuts_asks_for_gnome_settings(cx: &mut TestAppContext) {
    let mut ui = open(cx, daemon(Backend::Portal, SessionState::Bound));
    ui.click("edit-portal-shortcuts");
    assert_eq!(
        ui.sent(),
        vec![BridgeCommand::Shortcut(ShortcutRequest::Edit)]
    );
}

/// A failed portal or Carbon session offers a retry that sets the backend up
/// again with the keys the daemon prefers; once the daemon is binding again
/// the offer goes.
#[gpui_kit::test]
fn retrying_a_failed_session(cx: &mut TestAppContext) {
    let (toggle, hold) = (shortcut("Super+Y"), shortcut("Super+U"));
    let mut ui = open(cx, daemon(Backend::None, SessionState::Disabled));
    ui.daemon.shortcut.preferred = PerAction { toggle, hold };
    let retried = [
        (Backend::Portal, "retry-portal"),
        (Backend::Carbon, "retry-carbon"),
    ]
    .map(|(backend, retry)| {
        ui.daemon.shortcut.session = SessionState::Failed;
        report_backend(&mut ui, backend);
        ui.click(retry);
        let sent = ui.sent();
        ui.daemon.shortcut.session = SessionState::Binding;
        ui.publish(|_| {});
        (sent, ui.try_find(retry).is_some())
    });
    assert_eq!(
        retried,
        [
            (setup(Backend::Portal, toggle, hold), false),
            (setup(Backend::Carbon, toggle, hold), false),
        ]
    );
}

/// The GNOME form, with the recorder idle.
fn gnome(cx: &mut TestAppContext) -> Ui<'_> {
    open(cx, daemon(Backend::GnomeKeybinding, SessionState::Disabled))
}

/// Recording takes the next chord as the GNOME toggle and sends it; Escape
/// cancels a recording and Backspace clears the binding.
#[gpui_kit::test]
fn recording_a_gnome_shortcut_with_the_keyboard(cx: &mut TestAppContext) {
    let mut ui = gnome(cx);
    let idle = ui.label("recorder-state");
    ui.click("record-toggle");
    let recording = ui.label("recorder-state");
    ui.press("ctrl-alt-v");
    let recorded = (ui.label("recorder-state"), ui.sent());
    ui.click("record-toggle");
    ui.press("escape");
    let cancelled = (ui.label("recorder-state"), ui.sent());
    ui.click("record-toggle");
    ui.press("backspace");
    let cleared = (ui.label("recorder-state"), ui.sent());
    assert_eq!(
        (idle, recording, recorded, cancelled, cleared),
        (
            t!("shortcuts.not_set").into(),
            t!("shortcuts.recording").into(),
            (
                "Ctrl+Alt+V".to_string(),
                vec![BridgeCommand::Shortcut(ShortcutRequest::Set {
                    action: Action::Toggle,
                    keys: shortcut("Ctrl+Alt+V"),
                })]
            ),
            ("Ctrl+Alt+V".to_string(), Vec::new()),
            (
                t!("shortcuts.not_set").into(),
                vec![BridgeCommand::Shortcut(ShortcutRequest::Clear {
                    action: Action::Toggle,
                })]
            ),
        )
    );
}

/// A shifted symbol cannot be recorded: the recorder says so and sends
/// nothing.
#[gpui_kit::test]
fn shifted_symbols_need_manual_entry(cx: &mut TestAppContext) {
    let mut ui = gnome(cx);
    ui.click("record-toggle");
    ui.press("shift-!");
    assert_eq!(
        (
            ui.label("recorder-error"),
            ui.label("recorder-state"),
            ui.sent()
        ),
        (
            t!("hint.shifted_symbol").into(),
            t!("shortcuts.invalid").into(),
            Vec::new()
        )
    );
}

/// A typed shortcut is sent by its button once it parses, Clear sends the
/// removal of the binding, and a typed shortcut that does not parse is
/// reported instead.
#[gpui_kit::test]
fn setting_and_clearing_a_gnome_shortcut_by_hand(cx: &mut TestAppContext) {
    let mut ui = gnome(cx);
    ui.retype("toggle-input", "Shift+A");
    ui.click("set-toggle-manual");
    let rejected = (ui.label("recorder-error"), ui.sent());
    ui.retype("toggle-input", "Super+J");
    ui.click("set-toggle-manual");
    let set = (ui.label("recorder-state"), ui.sent());
    ui.click("clear-toggle");
    let cleared = ui.sent();
    let error = "Shift+A"
        .parse::<Shortcut>()
        .and_then(Shortcut::validate)
        .expect_err("Shift alone cannot be global")
        .to_string();
    assert_eq!(
        (rejected, set, cleared),
        (
            (error, Vec::new()),
            (
                "Super+J".to_string(),
                vec![BridgeCommand::Shortcut(ShortcutRequest::Set {
                    action: Action::Toggle,
                    keys: shortcut("Super+J"),
                })]
            ),
            vec![BridgeCommand::Shortcut(ShortcutRequest::Clear {
                action: Action::Toggle,
            })]
        )
    );
}
