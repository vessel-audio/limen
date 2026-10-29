//! Actor-owned state projection with bounded history and message-boundary publishing.

use tokio::sync::watch;

use super::snapshot::{
    ConfigSummary, DictationStatus, ERROR_CAP, ErrorEntry, FIELD_CAP, HistoryEntry, HistoryOutcome,
    StateSnapshot, capped,
};
use crate::config::Config;
use crate::dictation::{DictationState, DictationView};
use crate::input_method::sanitize_control_characters;

const HISTORY_LEN: usize = 50;
const HISTORY_BUDGET: usize = 1024 * 1024;

/// A completed input-worker result suitable for history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Completion {
    Committed,
    CommitFailed(String),
}

/// Internal mutable state; the exported snapshot is rebuilt at publication boundaries.
pub(crate) struct Projection {
    snapshot: StateSnapshot,
    sender: watch::Sender<StateSnapshot>,
    next_entry: u64,
}

impl Projection {
    pub(crate) fn new(config: &Config) -> (Self, watch::Receiver<StateSnapshot>) {
        let snapshot = StateSnapshot {
            take: 0,
            dictation: DictationStatus {
                state: DictationState::Idle,
                since_ms: None,
            },
            queued: false,
            history: Vec::new(),
            last_error: None,
            config: ConfigSummary::from_config(config),
        };
        let (sender, receiver) = watch::channel(snapshot.clone());
        (
            Self {
                snapshot,
                sender,
                next_entry: 1,
            },
            receiver,
        )
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> &StateSnapshot {
        &self.snapshot
    }

    /// Apply a complete view and publish only if the exported content changed.
    pub(crate) fn publish(&mut self, view: DictationView, at_ms: u64) {
        let previous = self.snapshot.dictation.state;
        let entering = |state: DictationState| {
            std::mem::discriminant(&previous) != std::mem::discriminant(&state)
        };
        if matches!(view.state, DictationState::Starting { .. })
            && !matches!(previous, DictationState::Starting { .. })
        {
            self.snapshot.take += 1;
            self.snapshot.last_error = None;
        }
        let since_ms = match view.state {
            DictationState::Recording { .. } if entering(view.state) => Some(at_ms),
            DictationState::Transcribing if entering(view.state) => Some(at_ms),
            DictationState::Idle | DictationState::Starting { .. } => None,
            DictationState::Recording { .. }
            | DictationState::Transcribing
            | DictationState::Committing => self.snapshot.dictation.since_ms,
        };
        self.snapshot.dictation = DictationStatus {
            state: view.state,
            since_ms,
        };
        self.snapshot.queued = view.queued;
        let next = self.snapshot.clone();
        self.sender
            .send_if_modified(|current| match current != &next {
                true => {
                    *current = next;
                    true
                }
                false => false,
            });
    }

    pub(crate) fn record_error(&mut self, message: String, at_ms: u64) {
        self.snapshot.last_error = Some(ErrorEntry {
            at_ms,
            message: capped(&message, ERROR_CAP).0,
        });
    }

    /// Queue a nonblank transcript and return its optional history correlation ID.
    pub(crate) fn queue(&mut self, text: &str, at_ms: u64) -> Option<u64> {
        let original = sanitize_control_characters(text);
        if original.trim().is_empty() {
            return None;
        }
        let (original, truncated) = capped(&original, FIELD_CAP);
        let id = self.next_entry;
        self.next_entry += 1;
        self.snapshot.history.push(HistoryEntry {
            id,
            take: self.snapshot.take,
            at_ms,
            original,
            text: None,
            outcome: HistoryOutcome::Pending,
            truncated,
        });
        Some(id)
    }

    pub(crate) fn complete(&mut self, entry: Option<u64>, text: String, completion: Completion) {
        let Some(id) = entry else {
            return;
        };
        let Some(entry) = self
            .snapshot
            .history
            .iter_mut()
            .find(|entry| entry.id == id)
        else {
            debug_assert!(false, "input report for missing history entry {id}");
            return;
        };
        let text = sanitize_control_characters(&text);
        let (text, text_truncated) = capped(text.trim(), FIELD_CAP);
        entry.text = Some(text);
        entry.truncated |= text_truncated;
        entry.outcome = match completion {
            Completion::Committed => HistoryOutcome::Committed,
            Completion::CommitFailed(error) => {
                let error = sanitize_control_characters(&error);
                let (error, error_truncated) = capped(&error, FIELD_CAP);
                entry.truncated |= error_truncated;
                HistoryOutcome::CommitFailed { error }
            }
        };
        self.evict_completed();
    }

    pub(crate) fn drop(&mut self, entry: Option<u64>) {
        let Some(id) = entry else {
            return;
        };
        if let Some(entry) = self
            .snapshot
            .history
            .iter_mut()
            .find(|entry| entry.id == id)
        {
            entry.outcome = HistoryOutcome::Dropped;
            self.evict_completed();
        } else {
            debug_assert!(false, "dropped input for missing history entry {id}");
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_history(&mut self) {
        self.snapshot
            .history
            .retain(|entry| matches!(entry.outcome, HistoryOutcome::Pending));
    }

    fn evict_completed(&mut self) {
        loop {
            let completed = self
                .snapshot
                .history
                .iter()
                .filter(|entry| !matches!(entry.outcome, HistoryOutcome::Pending))
                .collect::<Vec<_>>();
            let bytes = completed
                .iter()
                .map(|entry| entry_size(entry))
                .sum::<usize>();
            if completed.len() <= HISTORY_LEN && bytes <= HISTORY_BUDGET {
                return;
            }
            let Some(oldest) = self
                .snapshot
                .history
                .iter()
                .position(|entry| !matches!(entry.outcome, HistoryOutcome::Pending))
            else {
                return;
            };
            self.snapshot.history.remove(oldest);
        }
    }
}

fn entry_size(entry: &HistoryEntry) -> usize {
    let text = entry.text.as_deref().unwrap_or_default().len();
    let error = match &entry.outcome {
        HistoryOutcome::CommitFailed { error } => error.len(),
        HistoryOutcome::Pending
        | HistoryOutcome::Committed
        | HistoryOutcome::Kept
        | HistoryOutcome::Dropped => 0,
    };
    entry.original.len() + text + error
}

#[cfg(test)]
mod tests;
