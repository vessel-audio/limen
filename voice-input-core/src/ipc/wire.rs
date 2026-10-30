//! Versioned push messages and their bounded wire representation.

use serde::{Deserialize, Serialize};

use super::v1::VERSION;
use crate::audio::Level;
use crate::daemon::snapshot::{ERROR_CAP, StateSnapshot, capped};
use crate::shortcut::status::ShortcutStatus;

/// A complete state update sent on one subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSnapshot {
    /// Per-connection sequence, starting at one and increasing by one.
    pub seq: u64,
    /// Daemon-owned state without transport metadata.
    #[serde(flatten)]
    pub state: StateSnapshot,
    /// The current global-shortcut state with bounded variable fields.
    pub shortcut: ShortcutStatus,
}

impl WireSnapshot {
    pub(crate) fn new(seq: u64, state: StateSnapshot, mut shortcut: ShortcutStatus) -> Self {
        for trigger in [&mut shortcut.actual.toggle, &mut shortcut.actual.hold]
            .into_iter()
            .flatten()
        {
            trigger.description = capped(&trigger.description, 256).0;
            trigger.shortcuts.truncate(4);
        }
        shortcut.last_error = shortcut.last_error.map(|error| capped(&error, ERROR_CAP).0);
        Self {
            seq,
            state,
            shortcut,
        }
    }
}

/// One message on a subscription stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Push {
    /// A structured rejection, including an outdated daemon or capacity error.
    Error { message: String },
    /// Protocol and daemon identity sent once at connection start.
    Hello {
        protocol: u64,
        daemon_version: String,
    },
    /// A complete coalesced snapshot.
    Snapshot(Box<WireSnapshot>),
    /// A rate-limited recording level.
    Level { rms: f32, dbfs: f32 },
}

impl Push {
    pub(crate) fn hello() -> Self {
        Self::Hello {
            protocol: VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub(crate) fn level(level: Level) -> Self {
        Self::Level {
            rms: level.rms,
            dbfs: level.dbfs,
        }
    }
}
