//! What the providers served: each request the daemon sends to a model is
//! counted in a ledger kept beside the daemon's log, and a provider that
//! reports its balance is asked for it.
//!
//! The daemon is the ledger's only writer; the GUI reads the file as it
//! stands, so it shows the usage whether or not the daemon runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::config::{Paths, Service};
use crate::models::ModelRole;

mod balance;
mod ledger;
mod metered;
mod order;
mod prices;
mod providers;

pub use balance::{Balance, Money, balance};
pub use ledger::{Ledger, Tally, UsageRow};
pub(crate) use metered::{MeteredChat, MeteredTranscriber};
pub use order::{read_card_order, save_card_order};
pub use providers::ProviderUsage;

/// How many days of usage the ledger keeps.
const KEPT_DAYS: i64 = 90;

impl Paths {
    /// The usage ledger, beside the daemon's log.
    pub fn usage_file(&self) -> PathBuf {
        self.log_file.with_file_name("usage.json")
    }
}

/// The ledger as the daemon last wrote it; empty before its first request.
pub fn read_usage(paths: &Paths) -> Result<Ledger> {
    let path = paths.usage_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("failed to read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// The local day of `unix_secs`, counted in days since the Unix epoch, so
/// that "today" ends at the user's midnight rather than UTC's.
pub fn local_day(unix_secs: i64) -> i64 {
    let time = unix_secs as libc::time_t;
    // SAFETY: localtime_r only writes the zeroed `tm` it is lent, and reads
    // `time`; both outlive the call.
    let offset = unsafe {
        let mut tm = std::mem::zeroed::<libc::tm>();
        match libc::localtime_r(&time, &mut tm).is_null() {
            true => 0,
            false => tm.tm_gmtoff,
        }
    };
    (unix_secs + offset).div_euclid(86_400)
}

/// Today, as [`local_day`] counts it.
pub fn today() -> i64 {
    local_day(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64),
    )
}

/// Where the daemon counts its requests: the ledger it loaded at start, and
/// the file each count is written back to.
#[derive(Clone)]
pub(crate) struct Meter(Option<Arc<Mutex<Book>>>);

struct Book {
    path: PathBuf,
    ledger: Ledger,
}

impl Meter {
    /// The meter that keeps its counts in `paths`'s ledger. A ledger that
    /// cannot be read is started again, as losing old counts costs less
    /// than counting nothing.
    pub(crate) fn open(paths: &Paths) -> Self {
        let ledger = read_usage(paths).unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "starting a new usage ledger");
            Ledger::default()
        });
        Self(Some(Arc::new(Mutex::new(Book {
            path: paths.usage_file(),
            ledger,
        }))))
    }

    /// A meter that counts nothing, for requests not made for dictation.
    pub(crate) fn discarding() -> Self {
        Self(None)
    }

    /// Add `tally` to what `model` of `service` served `role` today, and
    /// write the ledger back. A failed write is logged; dictation goes on.
    pub(crate) fn record(&self, role: ModelRole, service: Service, model: &str, tally: Tally) {
        let Some(book) = &self.0 else {
            return;
        };
        let mut book = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let today = today();
        book.ledger.record(today, role, service, model, tally);
        book.ledger.forget_before(today - KEPT_DAYS);
        let written = (|| -> Result<()> {
            if let Some(dir) = book.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            crate::config::replace(&book.path, &serde_json::to_string(&book.ledger)?, None)
        })();
        if let Err(error) = written {
            tracing::warn!(error = %format!("{error:#}"), "failed to write the usage ledger");
        }
    }
}
