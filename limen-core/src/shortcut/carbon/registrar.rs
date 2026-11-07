//! The main-thread Carbon event pump: global-hotkey's manager must be
//! created, used, and dropped on the main thread, and hotkey events only
//! dispatch while the main event loop runs. The daemon's main thread owns
//! this pump; the tokio runtime runs on a worker thread and talks to it
//! through a command queue.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use global_hotkey::GlobalHotKeyManager;
use tokio::sync::oneshot;

use crate::shortcut::Action;

/// How long one pump of the Carbon event loop may take; register commands
/// are drained between pumps, so this bounds registration latency.
const PUMP_SECONDS: f64 = 0.025;

static REGISTRAR: OnceLock<Result<Registrar, String>> = OnceLock::new();
static BINDINGS: OnceLock<Bindings> = OnceLock::new();

/// A request the main thread's manager must perform.
enum Command {
    Register(
        Vec<(Action, global_hotkey::hotkey::HotKey)>,
        oneshot::Sender<Result<(), String>>,
    ),
    UnregisterAll(oneshot::Sender<()>),
}

/// Client half of the main-thread registrar, shared with the actor.
pub(crate) struct Registrar {
    commands: mpsc::Sender<Command>,
}

impl Registrar {
    pub(crate) async fn register(
        &self,
        hotkeys: Vec<(Action, global_hotkey::hotkey::HotKey)>,
    ) -> Result<(), String> {
        let (done, answer) = oneshot::channel();
        self.commands
            .send(Command::Register(hotkeys, done))
            .map_err(|_| "the hotkey event loop is gone".to_string())?;
        answer
            .await
            .map_err(|_| "the hotkey event loop is gone".to_string())?
    }

    pub(crate) async fn unregister_all(&self) {
        let (done, answer) = oneshot::channel();
        if self.commands.send(Command::UnregisterAll(done)).is_ok() {
            let _ = answer.await;
        }
    }
}

/// The registrar installed by [`install_event_pump`], or why it failed.
pub(crate) fn registrar() -> Result<&'static Registrar, String> {
    match REGISTRAR.get() {
        Some(Ok(registrar)) => Ok(registrar),
        Some(Err(error)) => Err(error.clone()),
        None => Err("the hotkey event pump is not installed".to_string()),
    }
}

/// Hotkey id → action table, written on registration and read by the
/// forwarder thread to name incoming events.
pub(crate) type Bindings = Arc<Mutex<HashMap<u32, Action>>>;

/// The main-thread half: owns the manager and pumps the Carbon event loop.
/// Must be created on the main thread before the tokio runtime starts.
pub(crate) struct EventPump {
    /// `None` when the manager could not be created; the pump still drains
    /// the command queue so registrants see their registrar error instead of
    /// a deadlock.
    manager: Option<GlobalHotKeyManager>,
    /// What the manager currently holds; unregister_all needs the list.
    registered: Vec<global_hotkey::hotkey::HotKey>,
    commands: mpsc::Receiver<Command>,
    bindings: Bindings,
}

/// Create the manager on this (the main) thread and publish the registrar.
pub(crate) fn install_event_pump() -> EventPump {
    let (commands, receiver) = mpsc::channel();
    let bindings = Arc::new(Mutex::new(HashMap::new()));
    let _ = BINDINGS.set(bindings.clone());
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => {
            let _ = REGISTRAR.set(Ok(Registrar { commands }));
            Some(manager)
        }
        Err(error) => {
            let _ = REGISTRAR.set(Err(format!("failed to create the hotkey manager: {error}")));
            None
        }
    };
    EventPump {
        manager,
        registered: Vec::new(),
        commands: receiver,
        bindings,
    }
}

/// The binding table installed with the event pump, for the forwarder.
pub(crate) fn bindings() -> Option<Bindings> {
    BINDINGS.get().cloned()
}

impl EventPump {
    /// Pump the Carbon event loop until `worker_done` is set (the daemon's
    /// runtime thread has exited). Blocks the main thread.
    pub(crate) fn run(mut self, worker_done: Arc<AtomicBool>) {
        while !worker_done.load(Ordering::SeqCst) {
            self.drain_commands();
            // RunCurrentEventLoop dispatches queued Carbon hotkey events and
            // returns after the slice, so commands are drained at least every
            // PUMP_SECONDS seconds.
            // SAFETY: no pointers or borrowed memory cross the FFI boundary;
            // the call only runs the current thread's event loop.
            unsafe { RunCurrentEventLoop(PUMP_SECONDS) };
        }
        self.drain_commands();
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Register(hotkeys, done) => {
                    let _ = done.send(self.register(hotkeys));
                }
                Command::UnregisterAll(done) => {
                    self.unregister();
                    let _ = done.send(());
                }
            }
        }
    }

    /// Drop every registration, best effort.
    fn unregister(&mut self) {
        if let Some(manager) = &self.manager {
            let _ = manager.unregister_all(&self.registered);
        }
        self.registered.clear();
        self.bindings.lock().unwrap().clear();
    }

    /// Replace every registration with `hotkeys`, updating the binding table
    /// only once the desktop accepted them.
    fn register(
        &mut self,
        hotkeys: Vec<(Action, global_hotkey::hotkey::HotKey)>,
    ) -> Result<(), String> {
        let manager = self
            .manager
            .as_mut()
            .ok_or_else(|| "the hotkey manager is not available".to_string())?;
        manager
            .unregister_all(&self.registered)
            .map_err(|e| e.to_string())?;
        self.registered.clear();
        let list: Vec<global_hotkey::hotkey::HotKey> =
            hotkeys.iter().map(|(_, hotkey)| *hotkey).collect();
        manager.register_all(&list).map_err(|e| e.to_string())?;
        self.registered = list;
        let mut bindings = self.bindings.lock().unwrap();
        bindings.clear();
        bindings.extend(
            hotkeys
                .iter()
                .map(|(action, hotkey)| (hotkey.id(), *action)),
        );
        Ok(())
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// Receive and dispatch pending Carbon events (including hotkey
    /// activations) for the current thread's event queue, returning after at
    /// most `duration` seconds.
    fn RunCurrentEventLoop(duration: f64);
}
