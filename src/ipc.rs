//! Thin line-based IPC between the `toggle`/`status` subcommands and the
//! daemon, over the unix socket at `$XDG_RUNTIME_DIR/voice-input.sock`.

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::config::Paths;

/// One line from a client; replies are one line back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Toggle,
    Status,
}

impl Request {
    fn as_line(self) -> &'static str {
        match self {
            Request::Toggle => "toggle",
            Request::Status => "status",
        }
    }

    fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "toggle" => Some(Request::Toggle),
            "status" => Some(Request::Status),
            _ => None,
        }
    }
}

pub fn bind(paths: &Paths) -> Result<UnixListener> {
    if paths.socket.exists() {
        // A leftover socket from a dead daemon; nobody can connect to it.
        std::fs::remove_file(&paths.socket)
            .with_context(|| format!("failed to remove stale socket {}", paths.socket.display()))?;
    }
    UnixListener::bind(&paths.socket)
        .with_context(|| format!("failed to bind {}", paths.socket.display()))
}

/// Daemon side: read one request line.
pub async fn recv(stream: UnixStream) -> Result<(Request, OwnedReadHalf, OwnedWriteHalf)> {
    let (read, write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read);
    reader.read_line(&mut line).await?;
    let req = Request::parse(&line).with_context(|| format!("unknown request {line:?}"))?;
    Ok((req, reader.into_inner(), write))
}

/// Daemon side: one-line reply, then the socket closes on drop.
pub async fn reply(mut write: OwnedWriteHalf, text: &str) -> Result<()> {
    write.write_all(text.as_bytes()).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

/// Client side (`toggle` / `status` subcommands).
pub async fn send(paths: &Paths, req: Request) -> Result<String> {
    let stream = UnixStream::connect(&paths.socket).await.with_context(|| {
        format!(
            "cannot connect to {}; is `voice-input daemon` running?",
            paths.socket.display()
        )
    })?;
    let (read, mut write) = stream.into_split();
    write.write_all(req.as_line().as_bytes()).await?;
    write.write_all(b"\n").await?;
    let mut line = String::new();
    let n = BufReader::new(read).read_line(&mut line).await?;
    if n == 0 {
        bail!("daemon closed the connection without a reply");
    }
    Ok(line.trim().to_string())
}
