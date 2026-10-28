//! The original protocol: a bare `toggle` or `status` line, answered with one
//! line of plain text. Unknown requests get no reply.

use anyhow::{Result, bail};
use tokio::sync::{mpsc, oneshot};

use super::Command;

/// Answer a legacy request; an unknown one closes the connection unanswered.
pub(super) async fn answer(line: &str, commands: &mpsc::Sender<Command>) -> Result<String> {
    let (reply, answer) = oneshot::channel();
    let command = match line.trim() {
        "toggle" => Command::Toggle(reply),
        "status" => Command::Status(reply),
        other => bail!("unknown request {other:?}"),
    };
    super::forward(commands, command, answer).await
}
