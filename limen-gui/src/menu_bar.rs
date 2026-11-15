//! The macOS app's resident half: a menu-bar status item that follows the
//! dictation state and drives the window and the bridge, a window whose
//! close hides the app instead of ending it, and the hotkey event pump of
//! the daemon this process hosts.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;
use limen_core::{DictationState, EventPump};
use tokio::sync::mpsc;

use crate::app::AppView;
use crate::bridge::BridgeCommand;
use crate::model::{Connection, DictationButton};
use crate::status_item::{Indicator, MenuEvent, StatusItem, unhide_app};

/// How often the hotkey registrations the daemon asked for are performed.
const PUMP_INTERVAL: Duration = Duration::from_millis(25);

/// The hosted daemon's hotkey event pump, until the app quits. GPUI's
/// application loop dispatches the hotkey events; the pump only performs
/// registrations, which must happen on the main thread.
#[derive(Clone)]
pub(crate) struct Pump(Rc<RefCell<Option<EventPump>>>);

impl Pump {
    pub(crate) fn new(pump: EventPump) -> Self {
        Self(Rc::new(RefCell::new(Some(pump))))
    }

    /// Perform registrations from the main thread until [`Self::close`].
    pub(crate) fn start(&self, cx: &App) {
        let pump = self.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(PUMP_INTERVAL).await;
                match pump.0.borrow_mut().as_mut() {
                    Some(pump) => pump.drain_commands(),
                    None => return,
                }
            }
        })
        .detach();
    }

    /// Unregister every hotkey and drop the pump, so a daemon still asking
    /// for registrations hears the event loop is gone instead of waiting on
    /// a main thread that is about to block on its shutdown.
    pub(crate) fn close(&self) {
        drop(self.0.borrow_mut().take());
    }
}

/// Put Limen in the menu bar. Without a status item the window keeps its
/// default close, since nothing could bring a hidden app back.
pub(crate) fn attach(
    window: AnyWindowHandle,
    view: Entity<AppView>,
    commands: mpsc::Sender<BridgeCommand>,
    cx: &mut App,
) {
    let (item, mut picks) = match StatusItem::install() {
        Ok(installed) => installed,
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "menu-bar status item unavailable");
            return;
        }
    };
    let mut shown = Indicator::Idle;
    let observed = view.clone();
    cx.observe(&observed, move |view, cx| {
        let model = &view.read(cx).model;
        let next = match (&model.connection, &model.snapshot) {
            (Connection::Connected, Some(snapshot)) => match snapshot.state.dictation.state {
                DictationState::Idle => Indicator::Idle,
                DictationState::Starting { .. } | DictationState::Recording { .. } => {
                    Indicator::Recording
                }
                DictationState::Transcribing | DictationState::Committing => {
                    Indicator::Transcribing
                }
            },
            (Connection::Connected, None)
            | (
                Connection::NotRunning
                | Connection::Connecting
                | Connection::Spawning
                | Connection::Reconnecting { .. }
                | Connection::SpawnFailed { .. }
                | Connection::ProtocolMismatch { .. },
                _,
            ) => Indicator::Offline,
        };
        match next == shown {
            true => {}
            false => {
                item.show(next);
                shown = next;
            }
        }
    })
    .detach();
    let _ = window.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, |_, cx| {
            cx.set_activation_policy(ActivationPolicy::Accessory);
            cx.hide();
            false
        });
    });
    cx.spawn(async move |cx| {
        while let Some(pick) = picks.recv().await {
            cx.update(|cx| match pick {
                MenuEvent::ToggleDictation => {
                    let command = match view.read(cx).model.button() {
                        DictationButton::Start | DictationButton::RecordAnother => {
                            Some(BridgeCommand::Start)
                        }
                        DictationButton::Stop => Some(BridgeCommand::Stop),
                        DictationButton::Queued | DictationButton::Disabled(_) => None,
                    };
                    if let Some(command) = command {
                        let _ = commands.try_send(command);
                    }
                }
                MenuEvent::OpenWindow => show(window, cx),
                MenuEvent::Quit => cx.quit(),
            });
        }
    })
    .detach();
}

/// Bring the hidden or backgrounded window to the front, with a Dock icon.
pub(crate) fn show(window: AnyWindowHandle, cx: &mut App) {
    cx.set_activation_policy(ActivationPolicy::Regular);
    unhide_app();
    cx.activate(true);
    let _ = window.update(cx, |_, window, _| window.activate_window());
}
