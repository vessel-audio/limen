//! In-process UI tests: the control panel in a headless GPUI window on
//! GPUI's test platform, the same on macOS and Linux and with no display,
//! permissions, or daemon. Tests find elements by the IDs `a11y` gives them,
//! drive them with synthetic pointer and key events, and read back what the
//! frame exposes: labels, selection, focus, and bounds. The bridge is
//! replaced by its command channel, so the view runs without a daemon.
//!
//! The window runs with reduced motion, as Lightweight mode does, so every
//! spring and transition lands on its end state in the frame that starts
//! it: GPUI's springs read the wall clock, not the test clock, so animating
//! them would make results depend on how fast frames render. The test clock
//! still drives timers, so a test settles the window by advancing it rather
//! than waiting. Animation is left to the Linux desktop suite. Text goes through GPUI's
//! no-op text system: bounds that depend on glyphs are not meaningful, and
//! neither are tooltips, which the observer does not see. The Linux desktop
//! suite covers rendering, tooltips, and the operating system's side.
//!
//! Labels are compared with other labels rather than with English text,
//! since translations are process-wide and another test may switch them.

mod navigation;

use std::time::Duration;

use gpui_kit::test::{ElementSnapshot, TestWindowExt};
use gpui_kit::{AnyWindowHandle, AppContext, Pixels, Point, TestAppContext};
use limen_core::Paths;
use tokio::sync::mpsc;

use crate::app::AppView;
use crate::bridge::BridgeCommand;

/// How far the test clock runs after an interaction, past any timer it
/// starts, such as a tooltip's delay.
const SETTLE: Duration = Duration::from_secs(2);

/// The control panel's window, driven from a test.
pub(super) struct Ui<'a> {
    cx: &'a mut TestAppContext,
    window: AnyWindowHandle,
    /// Kept open so the view's commands are accepted rather than refused.
    _commands: mpsc::Receiver<BridgeCommand>,
}

impl<'a> Ui<'a> {
    /// Open the control panel as the app does, before any daemon answers.
    pub(super) fn open(cx: &'a mut TestAppContext) -> Self {
        cx.update(|cx| {
            crate::init(cx);
            cx.set_reduce_motion(true);
        });
        let (commands, received) = mpsc::channel(32);
        // Nothing reads or writes these before a test asks it to.
        let root = std::env::temp_dir().join(format!("limen-ui-tests-{}", std::process::id()));
        let paths = Paths {
            config_file: root.join("config.toml"),
            socket: root.join("limen.sock"),
            log_file: root.join("daemon.log"),
            cache_dir: root.join("cache"),
        };
        let (window, _) = cx
            .update(|cx| {
                gpui_kit::open_window(crate::window_options(), cx, |window, cx| {
                    cx.new(|cx| AppView::new(paths, commands, window, cx))
                })
            })
            .expect("open the control panel");
        let mut ui = Self {
            cx,
            window,
            _commands: received,
        };
        ui.settle();
        ui
    }

    /// The element with this ID in a freshly drawn frame; panics, listing
    /// the IDs there are, when it is missing.
    pub(super) fn find(&mut self, id: &'static str) -> ElementSnapshot {
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                window.find(id)
            })
            .expect("the control panel closed")
    }

    /// The element with this ID, or `None` when the frame has none.
    pub(super) fn try_find(&mut self, id: &'static str) -> Option<ElementSnapshot> {
        self.cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                window.try_find(id)
            })
            .expect("the control panel closed")
    }

    /// Click the element's centre, then let the window settle.
    pub(super) fn click(&mut self, id: &'static str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.click(id, cx))
            .expect("the control panel closed");
        self.settle();
    }

    /// Drag the pointer between two window positions, then settle.
    pub(super) fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>) {
        self.cx
            .update_window(self.window, |_, window, cx| window.drag(from, to, cx))
            .expect("the control panel closed");
        self.settle();
    }

    /// Press a key, such as "tab" or "cmd-a", on the focused element, then
    /// settle.
    pub(super) fn press(&mut self, key: &str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.press(key, cx))
            .expect("the control panel closed");
        self.settle();
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
