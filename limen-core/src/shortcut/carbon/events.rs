//! The event forwarder: global-hotkey reports presses and releases on a
//! process-global channel consumable from any thread; this thread turns them
//! into the daemon's activation mailbox entries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::registrar::Bindings;
use crate::shortcut::{Action, Activation, ActivationReceipt, HoldEdge};

/// How long one receive waits; the stop flag is checked between waits.
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(100);

/// Forwards Carbon hotkey events into the daemon's activation mailbox until
/// [`Self::stop`] asks it to leave.
pub(crate) struct EventForwarder {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EventForwarder {
    pub(crate) fn spawn(
        bindings: Bindings,
        stop: Arc<AtomicBool>,
        activations: mpsc::UnboundedSender<ActivationReceipt>,
    ) -> Self {
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("limen-hotkeys".into())
            .spawn(move || forward_loop(bindings, thread_stop, activations));
        Self {
            stop,
            thread: thread.ok(),
        }
    }

    /// Ask the thread to leave and join it.
    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread {
            let _ = thread.join();
        }
    }
}

fn forward_loop(
    bindings: Bindings,
    stop: Arc<AtomicBool>,
    activations: mpsc::UnboundedSender<ActivationReceipt>,
) {
    let receiver = GlobalHotKeyEvent::receiver();
    while !stop.load(Ordering::SeqCst) {
        let event = match receiver.recv_timeout(RECEIVE_TIMEOUT) {
            Ok(event) => event,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let action = bindings.lock().unwrap().get(&event.id).copied();
        tracing::debug!(id = event.id, state = ?event.state(), ?action, "hotkey event");
        let activation = match (action, event.state()) {
            (Some(Action::Toggle), HotKeyState::Pressed) => Some(Activation::Toggle),
            (Some(Action::Hold), HotKeyState::Pressed) => Some(Activation::Hold(HoldEdge::Press)),
            (Some(Action::Hold), HotKeyState::Released) => {
                Some(Activation::Hold(HoldEdge::Release))
            }
            // A toggle's release and unknown ids mean nothing to dictation.
            (Some(Action::Toggle), HotKeyState::Released) | (None, _) => None,
        };
        if let Some(activation) = activation {
            let _ = activations.send(ActivationReceipt {
                activation,
                received_at: Instant::now(),
            });
        }
    }
}
