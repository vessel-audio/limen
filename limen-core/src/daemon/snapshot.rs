//! Serializable, resource-free snapshots exposed by the daemon.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::dictation::DictationState;

pub(crate) const FIELD_CAP: usize = 64 * 1024;
pub(crate) const ERROR_CAP: usize = 4 * 1024;
pub(crate) const CONFIG_CAP: usize = 256;
pub(crate) const MAX_PUSH_BYTES: usize = 8 * 1024 * 1024;

/// A complete view of the daemon at one actor-message boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// The most recently assigned take ID, or zero before the first take.
    pub take: u64,
    /// The current dictation phase and its elapsed-time origin.
    pub dictation: DictationStatus,
    /// Whether another take is queued behind the current output.
    pub queued: bool,
    /// Transcript history from oldest to newest.
    pub history: Vec<HistoryEntry>,
    /// The most recent daemon error, cleared when a new take starts.
    pub last_error: Option<ErrorEntry>,
    /// Finished transcription requests from oldest to newest.
    #[serde(default)]
    pub requests: Vec<RequestEntry>,
    /// Non-secret configuration useful for status display.
    pub config: ConfigSummary,
}

/// The public dictation phase and the timestamp from which elapsed time is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationStatus {
    /// The current phase and control mode.
    #[serde(flatten)]
    pub state: DictationState,
    /// Unix milliseconds when recording or transcribing began.
    pub since_ms: Option<u64>,
}

/// One queued transcript and its eventual delivery result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// A daemon-lifetime identifier, monotonically increasing.
    pub id: u64,
    /// The take that produced this transcript.
    pub take: u64,
    /// Unix milliseconds when the transcript was queued.
    pub at_ms: u64,
    /// The original transcript, sanitized and bounded for history storage.
    pub original: String,
    /// The polished text selected for input or retention, when completed.
    pub text: Option<String>,
    /// The current or final delivery result.
    pub outcome: HistoryOutcome,
    /// Whether any stored history field was shortened.
    pub truncated: bool,
}

/// The state of one transcript's delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HistoryOutcome {
    /// The input worker has not reported this entry yet.
    Pending,
    /// The text was committed to the focused application.
    Committed,
    /// The text was intentionally retained rather than typed.
    Kept,
    /// The input method failed; the text remains available in history.
    CommitFailed { error: String },
    /// The actor refused the job because input was not making progress.
    Dropped,
}

/// One finished transcription request to the configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEntry {
    /// The take whose audio was transcribed.
    pub take: u64,
    /// Unix milliseconds when the request began.
    pub started_ms: u64,
    /// Milliseconds from the request's start until the take left transcription.
    pub duration_ms: u64,
    /// Whether the request produced a transcript.
    pub outcome: RequestOutcome,
}

/// How a transcription request ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequestOutcome {
    /// The provider finished without reporting an error.
    Succeeded,
    /// The take failed; `error` is the daemon error reported meanwhile.
    Failed { error: String },
}

/// A timestamped daemon error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEntry {
    /// Unix milliseconds when the error was reported.
    pub at_ms: u64,
    /// The daemon's original English error, bounded only for transport safety.
    pub message: String,
}

/// Non-secret provider and input settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSummary {
    /// The input-method backend name.
    pub input_method: String,
    /// The configured ASR model.
    pub provider_model: String,
    /// The configured ASR endpoint URL.
    pub provider_url: String,
    /// Whether transcript polishing is enabled.
    pub polish_enabled: bool,
    /// The configured polishing model.
    #[serde(default)]
    pub polish_model: String,
    /// The configured polishing endpoint URL.
    #[serde(default)]
    pub polish_url: String,
}

impl ConfigSummary {
    pub(crate) fn from_config(config: &Config) -> Self {
        let backend = match crate::input_method::default_backend(config.input_method.backend) {
            crate::input_method::Backend::Dotool => "dotool",
            crate::input_method::Backend::Wtype => "wtype",
            crate::input_method::Backend::Fcitx5 => "fcitx5",
        };
        Self {
            input_method: capped(backend, CONFIG_CAP).0,
            provider_model: capped(&config.provider.model, CONFIG_CAP).0,
            provider_url: capped(&config.provider.url, CONFIG_CAP).0,
            polish_enabled: config.polish.enabled,
            polish_model: capped(&config.polish.model, CONFIG_CAP).0,
            polish_url: capped(&config.polish.url, CONFIG_CAP).0,
        }
    }
}

/// Shorten to `cap` UTF-8 bytes at a character boundary and append an
/// ellipsis within that same budget when content was removed.
pub(crate) fn capped(text: &str, cap: usize) -> (String, bool) {
    match text.len() <= cap {
        true => (text.to_string(), false),
        false => {
            let budget = cap.saturating_sub('…'.len_utf8());
            let boundary = text
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index <= budget)
                .last()
                .unwrap_or(0);
            let mut shortened = text[..boundary].to_string();
            shortened.push('…');
            (shortened, true)
        }
    }
}
