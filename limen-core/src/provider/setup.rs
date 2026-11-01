//! Changing the ASR endpoint: the config file first, then a running daemon.

use anyhow::{Result, bail};

use crate::config::{self, Paths};
use crate::ipc::{self, v1};

/// Where a new ASR endpoint took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlChange {
    /// The running daemon uses it from its next take on.
    Applied,
    /// No daemon is running; the next one starts with it.
    Saved,
}

/// Write `url` as `provider.url` and have a running daemon apply it.
pub async fn set_url(paths: &Paths, url: &str) -> Result<UrlChange> {
    config::update_provider_url(&paths.config_file, url)?;
    match ipc::request(paths, &v1::Request::ProviderReload).await {
        Ok(v1::Reply::Ok) => Ok(UrlChange::Applied),
        Ok(v1::Reply::Error { message }) => {
            bail!("saved, but the daemon did not apply it: {message}")
        }
        Ok(v1::Reply::ShortcutStatus(_) | v1::Reply::Accepted { .. }) => {
            bail!("the daemon answered something else")
        }
        Err(error) if error.downcast_ref::<ipc::NotRunning>().is_some() => Ok(UrlChange::Saved),
        Err(error) => Err(error),
    }
}
