//! Core daemon, IPC, configuration, and shortcut operations for voice-input.
//!
//! Implementation modules stay private; frontends depend only on the
//! documented re-exports in this module.

mod audio;
mod config;
mod daemon;
mod dictation;
mod input_method;
mod ipc;
mod notify;
mod polish;
mod provider;
mod session;
mod shortcut;

pub use audio::Level;
pub use config::{Config, Paths, ShortcutConfig};
pub use daemon::run as run_daemon;
pub use daemon::snapshot::{
    ConfigSummary, DictationStatus, ErrorEntry, HistoryEntry, HistoryOutcome, StateSnapshot,
};
pub use dictation::{Control, DictationState, DictationView, MIN_HOLD};
pub use ipc::v1::Delivery;
pub use ipc::{request, send, v1};
pub use provider::probe;
pub use shortcut::desktop_entry::APP_ID;
pub use shortcut::program::Program;
pub use shortcut::setup::{GnomeBinding, GnomeStatus, configured as shortcut_config};
pub use shortcut::setup::{
    Keys, Note, Report, SetupError, clear, daemon_status as shortcut_status, edit, gnome_status,
    set, setup,
};
pub use shortcut::status::{PerAction, SessionState, ShortcutStatus, Trigger};
pub use shortcut::{Action, Backend, HoldEdge, Shortcut, ShortcutError};
