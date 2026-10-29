//! Pure GUI state transitions, independent of GPUI and asynchronous I/O.

use std::collections::BTreeSet;

use voice_input_core::{DictationState, Level, StateSnapshot, WireSnapshot};

/// Connection and daemon-launch state shown across every page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    NotRunning,
    Connecting,
    Spawning,
    Connected,
    Reconnecting { attempt: u32, reason: String },
    SpawnFailed { log_tail: String },
    ProtocolMismatch { message: String },
}

/// A nonfatal daemon identity difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    DaemonVersion { running: String, expected: String },
}

/// The primary dictation action available to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationButton {
    Start,
    Stop,
    RecordAnother,
    Queued,
    Disabled(DisabledReason),
}

/// Why the primary button is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    Offline,
    OpeningMicrophone,
    ReleaseShortcut,
}

/// GUI-owned notification text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toast {
    CopiedToClipboard,
}

/// Side effects requested by a pure state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SpawnDaemon,
    Reconnect,
    CopyToClipboard(String),
    Toast(Toast),
}

/// Inputs from the bridge or from user intents.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Connecting,
    NotRunning,
    Disconnected {
        reason: String,
    },
    SpawnFailed {
        log_tail: String,
    },
    Hello {
        protocol: u64,
        daemon_version: String,
    },
    DaemonError {
        message: String,
    },
    Snapshot(Box<WireSnapshot>),
    CommandError {
        message: String,
    },
    DaemonStopped,
    StopAccepted {
        take: u64,
    },
    Level(Level),
}

/// Complete view-model state; tests compare it as a whole.
#[derive(Debug, Clone, PartialEq)]
pub struct GuiState {
    pub connection: Connection,
    pub warning: Option<Warning>,
    pub snapshot: Option<WireSnapshot>,
    pub level: Level,
    pub daemon_version: Option<String>,
    pub command_error: Option<String>,
    pub pending_copies: BTreeSet<u64>,
    pub copied_entries: BTreeSet<u64>,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            connection: Connection::Connecting,
            warning: None,
            snapshot: None,
            level: Level::SILENCE,
            daemon_version: None,
            command_error: None,
            pending_copies: BTreeSet::new(),
            copied_entries: BTreeSet::new(),
        }
    }
}

impl GuiState {
    /// Apply one message without performing I/O.
    pub fn apply(mut self, msg: Msg) -> (Self, Vec<Action>) {
        match msg {
            Msg::Connecting => {
                self.connection = Connection::Connecting;
                (self, Vec::new())
            }
            Msg::NotRunning => {
                let fresh = Self {
                    connection: Connection::Spawning,
                    ..Self::default()
                };
                (fresh, vec![Action::SpawnDaemon])
            }
            Msg::Disconnected { reason } => {
                let attempt = match self.connection {
                    Connection::Reconnecting { attempt, .. } => attempt + 1,
                    _ => 1,
                };
                self.connection = Connection::Reconnecting { attempt, reason };
                (self, vec![Action::Reconnect])
            }
            Msg::SpawnFailed { log_tail } => {
                self.connection = Connection::SpawnFailed { log_tail };
                (self, Vec::new())
            }
            Msg::Hello {
                protocol,
                daemon_version,
            } => {
                if protocol != 1 {
                    self.connection = Connection::ProtocolMismatch {
                        message: format!("unsupported protocol {protocol}; supported: 1"),
                    };
                    return (self, Vec::new());
                }
                self.warning = (daemon_version != env!("CARGO_PKG_VERSION")).then_some(
                    Warning::DaemonVersion {
                        running: daemon_version.clone(),
                        expected: env!("CARGO_PKG_VERSION").to_string(),
                    },
                );
                self.daemon_version = Some(daemon_version);
                self.connection = Connection::Connected;
                (self, Vec::new())
            }
            Msg::DaemonError { message } => {
                self.connection = Connection::ProtocolMismatch { message };
                (self, Vec::new())
            }
            Msg::CommandError { message } => {
                self.command_error = Some(message);
                (self, Vec::new())
            }
            Msg::DaemonStopped => {
                self.connection = Connection::NotRunning;
                (self, Vec::new())
            }
            Msg::StopAccepted { take } => {
                self.pending_copies.insert(take);
                (self, Vec::new())
            }
            Msg::Snapshot(snapshot) => {
                let actions = self.copy_actions(&snapshot.state);
                self.snapshot = Some(*snapshot);
                (self, actions)
            }
            Msg::Level(level) => {
                self.level = level;
                (self, Vec::new())
            }
        }
    }

    fn copy_actions(&mut self, snapshot: &StateSnapshot) -> Vec<Action> {
        let copies = snapshot
            .history
            .iter()
            .filter(|entry| {
                self.pending_copies.contains(&entry.take)
                    && matches!(entry.outcome, voice_input_core::HistoryOutcome::Kept)
                    && !self.copied_entries.contains(&entry.id)
            })
            .filter_map(|entry| entry.text.clone().map(|text| (entry.id, entry.take, text)))
            .collect::<Vec<_>>();
        copies
            .into_iter()
            .flat_map(|(id, take, text)| {
                self.pending_copies.remove(&take);
                self.copied_entries.insert(id);
                [
                    Action::CopyToClipboard(text),
                    Action::Toast(Toast::CopiedToClipboard),
                ]
            })
            .collect()
    }

    /// The primary button for the current connection and daemon phase.
    pub fn button(&self) -> DictationButton {
        if self.connection != Connection::Connected {
            return DictationButton::Disabled(DisabledReason::Offline);
        }
        let Some(snapshot) = &self.snapshot else {
            return DictationButton::Disabled(DisabledReason::Offline);
        };
        match snapshot.state.dictation.state {
            DictationState::Idle => DictationButton::Start,
            DictationState::Starting { .. } => {
                DictationButton::Disabled(DisabledReason::OpeningMicrophone)
            }
            DictationState::Recording {
                control: voice_input_core::Control::Toggle,
            } => DictationButton::Stop,
            DictationState::Recording {
                control: voice_input_core::Control::Hold,
            } => DictationButton::Disabled(DisabledReason::ReleaseShortcut),
            DictationState::Transcribing | DictationState::Committing => {
                match snapshot.state.queued {
                    true => DictationButton::Queued,
                    false => DictationButton::RecordAnother,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
