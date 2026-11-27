//! A loaded plugin's editor, state, and health.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use vst_audio::{Command, HostEvent, Instance};

use super::host::Vst3Error;

/// How long saving a plugin's state may take.
const STATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Something that happened to a loaded plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEvent {
    /// The user closed the plugin's editor window.
    EditorClosed,
    /// The plugin's worker failed; the processor reports it too.
    Failed(String),
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

    /// Show the plugin's own editor in a window of its own. Fails right away
    /// when the worker cannot reach a display; a plugin with no editor says
    /// so through [`Self::events`].
    pub fn open_editor(&self) -> Result<(), Vst3Error> {
        self.instance.controller().request(Command::OpenEditor)?;
        Ok(())
    }

    pub fn close_editor(&self) -> Result<(), Vst3Error> {
        self.instance.controller().request(Command::CloseEditor)?;
        Ok(())
    }

    /// The plugin's state, to pass to [`super::Vst3Host::load`] later.
    pub fn save_state(&mut self) -> Result<Vec<u8>, Vst3Error> {
        let request = self.instance.controller().request(Command::SaveState)?;
        let deadline = Instant::now() + STATE_TIMEOUT;
        while Instant::now() < deadline {
            match self.instance.try_next_event() {
                Some(HostEvent::State {
                    request: Some(id),
                    bytes,
                }) if id == request => return Ok(bytes),
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
        Err(Vst3Error::Timeout(STATE_TIMEOUT))
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
            HostEvent::Failed { reason } | HostEvent::RequestFailed { reason, .. } => {
                Some(PluginEvent::Failed(reason))
            }
            HostEvent::Completed { .. }
            | HostEvent::State { .. }
            | HostEvent::Parameters { .. }
            | HostEvent::EditorStatus { .. }
            | HostEvent::EditorImage { .. }
            | HostEvent::EditorResized { .. }
            | HostEvent::ParameterEdited { .. }
            | HostEvent::ParametersChanged
            | HostEvent::LatencyChanged(_)
            | HostEvent::RecoveryRequired
            | HostEvent::Recovered { .. }
            | HostEvent::Stopped
            | HostEvent::NotificationsLost => None,
        }
    }
}
