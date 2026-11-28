//! A loaded plugin's editor, state, and health.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use vst_audio::{Command, HostEvent, Instance, RequestId, Status};

use super::host::Vst3Error;

/// How long saving a plugin's state may take.
const STATE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a plugin's editor may take to open.
const EDITOR_TIMEOUT: Duration = Duration::from_secs(10);

/// What a worker answered a request with.
enum Answer {
    Done,
    State(Vec<u8>),
}

/// Something that happened to a loaded plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEvent {
    /// The user closed the plugin's editor window.
    EditorClosed,
    /// The plugin's worker failed; the processor reports it too.
    Failed(String),
    /// The plugin now has another latency than the one it loaded with, so a
    /// graph built around the old one is out of step.
    LatencyChanged,
}

/// A loaded plugin's controls. It owns the plugin's worker, which stops when
/// this is dropped.
pub struct Vst3Control {
    instance: Instance,
    pending: VecDeque<HostEvent>,
}

impl Vst3Control {
    pub(super) fn new(instance: Instance) -> Self {
        Self {
            instance,
            pending: VecDeque::new(),
        }
    }

    /// Show the plugin's own editor in a window of its own. Waits for the
    /// worker to say it opened, so that a machine with no display, or a plugin
    /// with no editor, is an error here rather than later.
    pub fn open_editor(&mut self) -> Result<(), Vst3Error> {
        let request = self.instance.controller().request(Command::OpenEditor)?;
        self.wait_for(request, EDITOR_TIMEOUT).map(|_| ())
    }

    /// Whether the plugin's worker is up. A worker that crashed or was killed
    /// is not, and its processor does nothing more.
    pub fn is_running(&self) -> bool {
        self.instance.controller().status() == Status::Running
    }

    /// The operating system's id of the plugin's worker process.
    pub fn worker_pid(&self) -> u32 {
        self.instance.controller().worker_pid()
    }

    /// Replace the plugin's state with `bytes`, as [`Self::save_state`] gave.
    pub fn restore_state(&mut self, bytes: Vec<u8>) -> Result<(), Vst3Error> {
        let request = self
            .instance
            .controller()
            .request(Command::RestoreState(bytes))?;
        self.wait_for(request, STATE_TIMEOUT).map(|_| ())
    }

    pub fn close_editor(&self) -> Result<(), Vst3Error> {
        self.instance.controller().request(Command::CloseEditor)?;
        Ok(())
    }

    /// The plugin's state, to pass to [`super::Vst3Host::load`] later.
    pub fn save_state(&mut self) -> Result<Vec<u8>, Vst3Error> {
        let request = self.instance.controller().request(Command::SaveState)?;
        match self.wait_for(request, STATE_TIMEOUT)? {
            Answer::State(bytes) => Ok(bytes),
            Answer::Done => Err(Vst3Error::Plugin(
                "the plugin answered without its state".to_string(),
            )),
        }
    }

    /// Wait for the worker's answer to `request`, keeping whatever else it
    /// says meanwhile for [`Self::events`].
    fn wait_for(&mut self, request: RequestId, timeout: Duration) -> Result<Answer, Vst3Error> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.instance.try_next_event() {
                Some(HostEvent::State {
                    request: Some(id),
                    bytes,
                }) if id == request => return Ok(Answer::State(bytes)),
                Some(HostEvent::Completed { request: id }) if id == request => {
                    return Ok(Answer::Done);
                }
                Some(HostEvent::RequestFailed {
                    request: id,
                    reason,
                }) if id == request => {
                    return Err(Vst3Error::Plugin(reason));
                }
                Some(other) => self.pending.push_back(other),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        Err(Vst3Error::Timeout(timeout))
    }

    /// What happened since the last call.
    pub fn events(&mut self) -> Vec<PluginEvent> {
        while let Some(event) = self.instance.try_next_event() {
            self.pending.push_back(event);
        }
        self.pending.drain(..).filter_map(Self::translate).collect()
    }

    fn translate(event: HostEvent) -> Option<PluginEvent> {
        match event {
            HostEvent::EditorClosed => Some(PluginEvent::EditorClosed),
            HostEvent::Failed { reason } => Some(PluginEvent::Failed(reason)),
            HostEvent::LatencyChanged(_) => Some(PluginEvent::LatencyChanged),
            // A refused request, such as an editor that cannot open, is told
            // to whoever made it; the worker itself is fine.
            HostEvent::RequestFailed { .. }
            | HostEvent::Completed { .. }
            | HostEvent::State { .. }
            | HostEvent::Parameters { .. }
            | HostEvent::EditorStatus { .. }
            | HostEvent::EditorImage { .. }
            | HostEvent::EditorResized { .. }
            | HostEvent::ParameterEdited { .. }
            | HostEvent::ParametersChanged
            | HostEvent::RecoveryRequired
            | HostEvent::Recovered { .. }
            | HostEvent::Stopped
            | HostEvent::NotificationsLost => None,
        }
    }
}
