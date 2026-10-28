//! What a take has handed to text input, and how its completion is reported.

use super::InputOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalOutcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct OutputTracker {
    pub(super) pending_segments: usize,
    pub(super) terminal_outcome: Option<TerminalOutcome>,
    pub(super) dropped_text: bool,
    pub(super) input_failed: bool,
}

impl OutputTracker {
    pub(super) fn segment_enqueued(&mut self) {
        self.pending_segments += 1;
    }

    pub(super) fn segment_dropped(&mut self) {
        self.dropped_text = true;
    }

    pub(super) fn segment_finished(&mut self, outcome: InputOutcome) {
        if self.pending_segments == 0 {
            return;
        }
        self.pending_segments -= 1;
        if matches!(outcome, InputOutcome::Failed(_)) {
            self.input_failed = true;
        }
    }

    pub(super) fn should_finish(self) -> bool {
        self.terminal_outcome.is_some() && self.pending_segments == 0
    }

    pub(super) fn completion_message(self) -> Option<&'static str> {
        match self.terminal_outcome {
            Some(TerminalOutcome::Success) if self.pending_segments == 0 => {
                match (self.dropped_text, self.input_failed) {
                    (true, true) | (true, false) => {
                        Some("dictation finished (some text was dropped)")
                    }
                    (false, true) => Some("dictation finished (some text failed)"),
                    (false, false) => Some("dictation finished"),
                }
            }
            Some(TerminalOutcome::Success) | Some(TerminalOutcome::Failure) | None => None,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}
