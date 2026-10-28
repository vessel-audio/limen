//! The `voice-input hold` command.

use anyhow::{Result, bail};

use super::HoldEdge;
use crate::config::Paths;
use crate::ipc::{self, v1};

/// `voice-input hold <press|release>`: report an edge of the hold-to-talk key,
/// e.g. from a compositor's key-press and key-release bindings.
pub(crate) async fn hold(paths: &Paths, edge: HoldEdge) -> Result<()> {
    match ipc::request(paths, &v1::Request::Hold { edge }).await? {
        v1::Reply::Ok => Ok(()),
        v1::Reply::Error { message } => bail!("the daemon refused: {message}"),
    }
}
