//! Core daemon, IPC, configuration, and shortcut operations for Limen.
//!
//! Implementation modules stay private; frontends depend only on the
//! documented re-exports in this module.

mod audio;
mod backend;
mod config;
mod connection;
mod daemon;
mod dictation;
mod input_method;
mod ipc;
mod models;
mod notify;
mod polish;
mod provider;
mod reload;
mod session;
mod shortcut;
mod task;

pub use audio::Level;
pub use backend::{
    Api, BoxFuture, ChatModel, Connection, Exchange, Finish, ModelBackend, Reply, Take, Transcriber,
};
pub use config::{ApiKey, Config, KeyChange, Paths, Service, ShortcutConfig};
pub use daemon::run as run_daemon;
pub use daemon::snapshot::{
    ConfigSummary, DictationStatus, ErrorEntry, HistoryEntry, HistoryOutcome, RequestEntry,
    RequestOutcome, StateSnapshot,
};
pub use daemon::{DaemonTermination, daemon_peer_pid, spawn_daemon, terminate_daemon};
pub use dictation::{Control, DictationState, DictationView, MIN_HOLD};
pub use ipc::v1::Delivery;
pub use ipc::wire::{Push, WireSnapshot};
pub use ipc::{DaemonNotRunning, SocketIdentity, request, send, subscribe, v1};
pub use models::{
    KeySource, KeyState, ModelRole, Polishing, Provider, list_models, load_provider, save_provider,
};
pub use provider::{
    Endpoint as ProviderEndpoint, probe, server_url as provider_server_url,
    server_url_with_route as provider_server_url_with_route, split_url as split_provider_url,
};
pub use reload::ConfigChange;
pub use shortcut::desktop_entry::{APP_ID, Launch};
pub use shortcut::program::Program;
pub use shortcut::setup::{GnomeBinding, GnomeStatus, configured as shortcut_config};
pub use shortcut::setup::{
    Keys, Note, Report, SetupError, clear, daemon_status as shortcut_status, edit, gnome_status,
    set, setup,
};
pub use shortcut::status::{PerAction, SessionState, ShortcutStatus, Trigger};
pub use shortcut::{Action, Backend, HoldEdge, Shortcut, ShortcutError};
