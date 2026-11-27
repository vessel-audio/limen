//! In-process UI tests: the control panel in a headless GPUI window on
//! GPUI's test platform, the same on macOS and Linux and with no display,
//! permissions, or daemon. Tests find elements by the IDs `a11y` gives them,
//! drive them with synthetic pointer and key events, and read back what the
//! frame exposes: labels, selection, focus, and bounds.
//!
//! The bridge is the seam. What the view asks of the daemon and the config
//! file arrives on its command channel, where a test reads it; the bridge's
//! replies, and the daemon's snapshots from a [`Daemon`] the test edits, go
//! back through the same handler the app uses. The bridge's own tests cover
//! what it does with the commands, and the Linux desktop suite keeps a few
//! cases that run the whole chain against a daemon.
//!
//! The window runs with reduced motion, as Lightweight mode does, so every
//! spring and transition lands on its end state in the frame that starts
//! it: GPUI's springs read the wall clock, not the test clock, so animating
//! them would make results depend on how fast frames render. The test clock
//! still drives timers, so a test settles the window by advancing it rather
//! than waiting. Text goes through GPUI's no-op text system: bounds that
//! depend on glyphs are not meaningful, and neither are tooltips, which the
//! observer does not see. The Linux desktop suite covers rendering,
//! tooltips, animation, and the operating system's side.
//!
//! Labels are compared with the translations themselves rather than with
//! English text. Translations are process-wide, so every window holds them
//! shared, and a test that switches the language holds them alone.

mod daemon;
mod dictation;
mod history;
mod lifecycle;
mod model_discovery;
mod model_forms;
mod model_support;
mod models;
mod navigation;
mod network;
mod routing;
mod settings;
mod shortcuts;
mod usage;

use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use gpui_kit::test::{ElementSnapshot, TestWindowExt};
use gpui_kit::{AnyWindowHandle, AppContext, Entity, Pixels, Point, ScrollDelta, TestAppContext};
use limen_core::{Paths, StateSnapshot};
use tokio::sync::mpsc;

use crate::app::AppView;
use crate::bridge::{BridgeCommand, BridgeEvent};
use daemon::Daemon;

/// How far the test clock runs after an interaction, past any timer it
/// starts, such as a tooltip's delay.
const SETTLE: Duration = Duration::from_secs(2);

/// Held by every test that reads translations, and alone by one that
/// switches the language.
static TRANSLATIONS: RwLock<()> = RwLock::new(());

/// A test's hold on the translations.
enum Translations {
    Read(#[allow(dead_code)] RwLockReadGuard<'static, ()>),
    /// Restores English when the test ends, however it ends.
    Switch(#[allow(dead_code)] RwLockWriteGuard<'static, ()>),
}

impl Translations {
    fn read() -> Self {
        Self::Read(
            TRANSLATIONS
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn switch() -> Self {
        Self::Switch(
            TRANSLATIONS
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

impl Drop for Translations {
    fn drop(&mut self) {
        match self {
            Self::Read(_) => {}
            Self::Switch(_) => crate::locale::Locale::En.initialize(),
        }
    }
}

/// The control panel's window, driven from a test.
pub(super) struct Ui<'a> {
    cx: &'a mut TestAppContext,
    window: AnyWindowHandle,
    view: Entity<AppView>,
    /// What the view asked of the bridge and no test has read yet.
    commands: mpsc::Receiver<BridgeCommand>,
    /// The daemon whose snapshots [`Self::publish`] sends.
    pub(super) daemon: Daemon,
    _translations: Translations,
}

impl<'a> Ui<'a> {
    /// Open the control panel as the app does, before any daemon answers.
    pub(super) fn open(cx: &'a mut TestAppContext) -> Self {
        Self::launch(cx, Translations::read())
    }

    /// Open the control panel once `daemon` has said hello and sent its
    /// first snapshot, as the app is once it reaches a daemon.
    pub(super) fn connected(cx: &'a mut TestAppContext, daemon: Daemon) -> Self {
        Self::launch(cx, Translations::read()).connect(daemon)
    }

    /// Open the control panel connected to `daemon`, for a test that
    /// switches the interface language; English returns when it ends.
    pub(super) fn switching_language(cx: &'a mut TestAppContext, daemon: Daemon) -> Self {
        Self::launch(cx, Translations::switch()).connect(daemon)
    }

    fn launch(cx: &'a mut TestAppContext, translations: Translations) -> Self {
        cx.update(|cx| {
            crate::init(cx);
            cx.set_reduce_motion(true);
        });
        let (sender, commands) = mpsc::channel(256);
        // Nothing reads or writes these: the bridge that would is not running.
        let root = std::env::temp_dir().join(format!("limen-ui-tests-{}", std::process::id()));
        let paths = Paths {
            config_file: root.join("config.toml"),
            socket: root.join("limen.sock"),
            log_file: root.join("daemon.log"),
            cache_dir: root.join("cache"),
        };
        let (window, view) = cx
            .update(|cx| {
                gpui_kit::open_window(crate::window_options(), cx, |window, cx| {
                    cx.new(|cx| AppView::new(paths, sender, window, cx))
                })
            })
            .expect("open the control panel");
        let mut ui = Self {
            cx,
            window,
            view,
            commands,
            daemon: Daemon::default(),
            _translations: translations,
        };
        ui.settle();
        ui
    }

    fn connect(mut self, daemon: Daemon) -> Self {
        self.daemon = daemon;
        self.receive(Daemon::hello());
        let snapshot = self.daemon.snapshot();
        self.receive(snapshot);
        self
    }

    /// Hand the view an event as the bridge does, then settle.
    pub(super) fn receive(&mut self, event: BridgeEvent) {
        self.cx
            .update(|cx| self.view.update(cx, |view, cx| view.receive(event, cx)));
        self.settle();
    }

    /// Change the daemon's state and publish it as its next snapshot.
    pub(super) fn publish(&mut self, change: impl FnOnce(&mut StateSnapshot)) {
        change(&mut self.daemon.state);
        let snapshot = self.daemon.snapshot();
        self.receive(snapshot);
    }

    /// The commands the view sent since this was last asked, oldest first.
    pub(super) fn sent(&mut self) -> Vec<BridgeCommand> {
        std::iter::from_fn(|| self.commands.try_recv().ok()).collect()
    }

    /// Read the view's state, such as a form's fields.
    pub(super) fn read<T>(&mut self, read: impl FnOnce(&AppView, &gpui_kit::App) -> T) -> T {
        self.cx.read(|cx| read(self.view.read(cx), cx))
    }

    /// The text on the clipboard, if any.
    pub(super) fn clipboard(&mut self) -> Option<String> {
        self.cx.read_from_clipboard().and_then(|item| item.text())
    }

    /// The element with this ID in a freshly drawn frame; panics, listing
    /// the IDs there are, when it is missing.
    pub(super) fn find(&mut self, id: &str) -> ElementSnapshot {
        let id = gpui_kit::SharedString::from(id.to_string());
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                window.find(id)
            })
            .expect("the control panel closed")
    }

    /// The element with this ID, or `None` when the frame has none.
    pub(super) fn try_find(&mut self, id: &str) -> Option<ElementSnapshot> {
        let id = gpui_kit::SharedString::from(id.to_string());
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id)
            })
            .expect("the control panel closed")
    }

    /// The label of the element with this ID, which must have one.
    pub(super) fn label(&mut self, id: &str) -> String {
        self.find(id)
            .label()
            .unwrap_or_else(|| panic!("#{id} has no label"))
            .to_string()
    }

    /// Run `act` on the window, then let it settle.
    fn act(&mut self, act: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App)) {
        self.cx
            .update_window(self.window, |_, window, cx| act(window, cx))
            .expect("the control panel closed");
        self.settle();
    }

    /// Click the element's centre, then let the window settle.
    pub(super) fn click(&mut self, id: &str) {
        let id = gpui_kit::SharedString::from(id.to_string());
        self.act(|window, cx| window.click(id, cx));
    }

    /// Turn the wheel over the element, then settle.
    pub(super) fn scroll(&mut self, id: &str, delta: ScrollDelta) {
        let id = gpui_kit::SharedString::from(id.to_string());
        self.act(|window, cx| window.scroll(id, delta, cx));
    }

    /// Drag the pointer between two window positions, then settle.
    pub(super) fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>) {
        self.act(|window, cx| window.drag(from, to, cx));
    }

    /// Press a key, such as "tab" or "cmd-a", on the focused element, then
    /// settle.
    pub(super) fn press(&mut self, key: &str) {
        self.act(|window, cx| window.press(key, cx));
    }

    /// Type text into the focused element, each key in an update of its
    /// own, then settle. Unlike gpui-kit's `input`, which draws a frame
    /// inside the key's update, a key's events reach their subscribers
    /// before anything draws, as in the app: a page that resets an
    /// untouched field while drawing would otherwise undo the key that
    /// touched it.
    pub(super) fn input(&mut self, text: &str) {
        for character in text.chars() {
            self.cx
                .update_window(self.window, |_, window, cx| {
                    let text = character.to_string();
                    let mut key = gpui_kit::Keystroke::parse_with_os(&text, cx.platform_info().os)
                        .expect("a character is a valid keystroke");
                    key.key_char = Some(text);
                    window.dispatch_keystroke(key, cx);
                })
                .expect("the control panel closed");
        }
        self.settle();
    }

    /// Replace a text field's contents by typing, as a user does.
    pub(super) fn retype(&mut self, id: &str, text: &str) {
        self.click(id);
        self.press(SELECT_ALL);
        self.input(text);
    }

    /// Run due timers and tasks, then draw the frame they leave behind.
    fn settle(&mut self) {
        self.cx.executor().advance_clock(SETTLE);
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("the control panel closed");
    }
}

/// The platform's chord for selecting a text field's contents.
#[cfg(target_os = "macos")]
const SELECT_ALL: &str = "cmd-a";
#[cfg(not(target_os = "macos"))]
const SELECT_ALL: &str = "ctrl-a";
