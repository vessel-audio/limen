//! Line-based IPC between the `toggle`/`status` subcommands and the daemon,
//! over the unix socket at `$XDG_RUNTIME_DIR/voice-input.sock`.
//!
//! Daemon side spawns one task per connection: a garbage or stalled client
//! can neither block nor kill the daemon's main loop.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::config::Paths;

/// How long a connection may take to send its request / collect its reply.
const CONN_TIMEOUT: Duration = Duration::from_secs(2);

/// A parsed client request; the reply goes back through the oneshot.
#[derive(Debug)]
pub enum Command {
    Toggle(oneshot::Sender<String>),
    Status(oneshot::Sender<String>),
}

pub fn bind(paths: &Paths) -> Result<UnixListener> {
    if paths.socket.exists() {
        // Distinguish a live daemon (refuse to start a second one) from a
        // stale socket file left by a crashed one (safe to replace).
        if std::os::unix::net::UnixStream::connect(&paths.socket).is_ok() {
            bail!(
                "another voice-input daemon is already listening on {}",
                paths.socket.display()
            );
        }
        std::fs::remove_file(&paths.socket)
            .with_context(|| format!("failed to remove stale socket {}", paths.socket.display()))?;
    }
    UnixListener::bind(&paths.socket)
        .with_context(|| format!("failed to bind {}", paths.socket.display()))
}

/// Accept connections forever, forwarding parsed commands to the main loop.
pub fn spawn_accept_loop(listener: UnixListener, cmd_tx: mpsc::Sender<Command>) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(handle_conn(stream, cmd_tx.clone()));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
}

async fn handle_conn(stream: UnixStream, cmd_tx: mpsc::Sender<Command>) {
    if let Err(e) = handle_conn_inner(stream, cmd_tx).await {
        tracing::debug!(error = %format!("{e:#}"), "ipc connection failed");
    }
}

async fn handle_conn_inner(stream: UnixStream, cmd_tx: mpsc::Sender<Command>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    tokio::time::timeout(CONN_TIMEOUT, BufReader::new(read).read_line(&mut line))
        .await
        .context("read timeout")??;
    let (reply_tx, reply_rx) = oneshot::channel();
    let cmd = match line.trim() {
        "toggle" => Command::Toggle(reply_tx),
        "status" => Command::Status(reply_tx),
        other => bail!("unknown request {other:?}"),
    };
    tokio::time::timeout(CONN_TIMEOUT, cmd_tx.send(cmd))
        .await
        .context("daemon command queue full")?
        .context("daemon not accepting commands")?;
    let reply = tokio::time::timeout(CONN_TIMEOUT, reply_rx)
        .await
        .context("reply timeout")?
        .context("daemon dropped the reply channel")?;
    write.write_all(reply.as_bytes()).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

/// Client side (`toggle` / `status` subcommands).
pub async fn send(paths: &Paths, request: &str) -> Result<String> {
    let stream = UnixStream::connect(&paths.socket).await.with_context(|| {
        format!(
            "cannot connect to {}; is `voice-input daemon` running?",
            paths.socket.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    write.write_all(request.as_bytes()).await?;
    write.write_all(b"\n").await?;
    let mut line = String::new();
    let n = BufReader::new(read).read_line(&mut line).await?;
    if n == 0 {
        bail!("daemon closed the connection without a reply");
    }
    Ok(line.trim().to_string())
}
