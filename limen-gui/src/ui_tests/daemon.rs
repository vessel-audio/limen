//! The daemon as the bridge reports it. A test edits its state and
//! publishes it as the next snapshot, in place of the daemon's own
//! subscription; what the view asks of the daemon arrives as commands.

use limen_core::{
    Backend, ConfigSummary, DictationState, DictationStatus, HistoryEntry, HistoryOutcome,
    PerAction, RequestEntry, RequestOutcome, SessionState, Shortcut, ShortcutStatus, StateSnapshot,
    WireSnapshot,
};

use crate::bridge::BridgeEvent;
use crate::model::Msg;

/// A fixed point in time so cases render the same clock everywhere.
const EPOCH_MS: u64 = 1_767_225_600_000;

/// The state a stand-in daemon publishes.
pub(crate) struct Daemon {
    pub(super) state: StateSnapshot,
    pub(super) shortcut: ShortcutStatus,
    seq: u64,
}

impl Default for Daemon {
    /// An idle daemon with no history and no shortcut backend.
    fn default() -> Self {
        Self {
            state: StateSnapshot {
                take: 0,
                dictation: idle(),
                queued: false,
                history: Vec::new(),
                last_error: None,
                requests: Vec::new(),
                config: ConfigSummary {
                    input_method: "fcitx5".to_string(),
                    provider_model: "test-model".to_string(),
                    provider_url: "http://127.0.0.1:9/v1".to_string(),
                    polish_enabled: false,
                },
            },
            shortcut: ShortcutStatus {
                backend: Backend::None,
                session: SessionState::Disabled,
                preferred: PerAction {
                    toggle: Shortcut::DEFAULT_TOGGLE,
                    hold: Shortcut::DEFAULT_HOLD,
                },
                actual: PerAction::default(),
                last_error: None,
            },
            seq: 0,
        }
    }
}

impl Daemon {
    /// A daemon whose history and request list start with these.
    pub(super) fn with(history: Vec<HistoryEntry>, requests: Vec<RequestEntry>) -> Self {
        let mut daemon = Self::default();
        daemon.state.take = history.iter().map(|entry| entry.take).max().unwrap_or(0);
        daemon.state.history = history;
        daemon.state.requests = requests;
        daemon
    }

    /// The hello a subscription opens with, from a daemon of this version.
    pub(super) fn hello() -> BridgeEvent {
        BridgeEvent::Msg(Msg::Hello {
            protocol: limen_core::v1::VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            daemon_pid: 1,
        })
    }

    /// The current state as the subscription's next snapshot.
    pub(super) fn snapshot(&mut self) -> BridgeEvent {
        self.seq += 1;
        BridgeEvent::Msg(Msg::Snapshot(Box::new(WireSnapshot {
            seq: self.seq,
            state: self.state.clone(),
            shortcut: self.shortcut.clone(),
        })))
    }
}

/// No take under way.
pub(super) fn idle() -> DictationStatus {
    DictationStatus {
        state: DictationState::Idle,
        since_ms: None,
    }
}

/// A dictation phase that began at the fixed epoch.
pub(super) fn since(state: DictationState) -> DictationStatus {
    DictationStatus {
        state,
        since_ms: Some(EPOCH_MS),
    }
}

/// A transcript the daemon kept in history without typing it.
pub(super) fn kept(id: u64, text: &str) -> HistoryEntry {
    HistoryEntry {
        id,
        take: id,
        at_ms: EPOCH_MS + id * 60_000,
        original: text.to_string(),
        text: Some(text.to_string()),
        outcome: HistoryOutcome::Kept,
        truncated: false,
    }
}

/// A transcription request that failed with `error`.
pub(super) fn failed_request(take: u64, error: &str) -> RequestEntry {
    RequestEntry {
        take,
        started_ms: EPOCH_MS + take * 60_000,
        duration_ms: 1_200,
        outcome: RequestOutcome::Failed {
            error: error.to_string(),
        },
    }
}
