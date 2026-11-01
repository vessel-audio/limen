//! Asking a running daemon to apply a section of the config file that was
//! just written.

use anyhow::{Result, bail};

use crate::config::Paths;
use crate::ipc::{self, v1};

/// Where a config change took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigChange {
    /// The running daemon uses it from its next take on.
    Applied,
    /// No daemon is running; the next one starts with it.
    Saved,
}

/// Send `request`, one of the reload requests, to the daemon, if it runs.
pub(crate) async fn reload(paths: &Paths, request: &v1::Request) -> Result<ConfigChange> {
    match ipc::request(paths, request).await {
        Ok(v1::Reply::Ok) => Ok(ConfigChange::Applied),
        Ok(v1::Reply::Error { message }) => {
            bail!("saved, but the daemon did not apply it: {message}")
        }
        Ok(v1::Reply::ShortcutStatus(_) | v1::Reply::Accepted { .. }) => {
            bail!("the daemon answered something else")
        }
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => Ok(ConfigChange::Saved),
        Err(error) => Err(error),
    }
}
