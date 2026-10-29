//! IPC between the CLI and the daemon over the unix socket at
//! `$XDG_RUNTIME_DIR/voice-input.sock`: one request and one reply per
//! connection.
//!
//! A request is the bytes before the first `\n`, or before EOF, and at most
//! [`MAX_REQUEST_BYTES`] long. If its first non-whitespace character is `{` it
//! is a [`v1`] JSON request; anything else is a [`legacy`] `toggle` / `status`
//! line, whose plain-text replies are unchanged.
//!
//! Daemon side spawns one task per connection: a garbage or stalled client
//! can neither block nor kill the daemon's main loop.

mod legacy;
pub(crate) mod v1;

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::config::Paths;
use crate::shortcut::HoldEdge;

/// How long a connection may take to send its request / collect its reply.
const CONN_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest request accepted, not counting its trailing `\n`.
pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// A parsed client request; the reply goes back through the oneshot.
#[derive(Debug)]
pub enum Command {
    Toggle(oneshot::Sender<String>),
    Status(oneshot::Sender<String>),
    Hold(HoldEdge, oneshot::Sender<v1::Reply>),
    ShortcutRebind(oneshot::Sender<v1::Reply>),
    ShortcutStatus(oneshot::Sender<v1::Reply>),
}

/// The daemon does not listen on its socket.
#[derive(Debug, thiserror::Error)]
#[error("cannot connect to {}; is `voice-input daemon` running?", .0.display())]
pub(crate) struct NotRunning(PathBuf, #[source] std::io::Error);

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
    let request = tokio::time::timeout(CONN_TIMEOUT, read_request(read))
        .await
        .context("read timeout")??;
    let reply = match request {
        Framed::TooLong => v1::Reply::Error {
            message: format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        }
        .to_line(),
        Framed::Request(bytes) => match String::from_utf8(bytes) {
            Err(_) => v1::Reply::Error {
                message: "request is not valid UTF-8".to_string(),
            }
            .to_line(),
            Ok(text) => {
                let text = text.trim_ascii();
                match text.starts_with('{') {
                    true => v1::answer(text, &cmd_tx).await.to_line(),
                    false => legacy::answer(text, &cmd_tx).await?,
                }
            }
        },
    };
    write.write_all(reply.as_bytes()).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

/// One request as read off a connection.
enum Framed {
    Request(Vec<u8>),
    TooLong,
}

/// Read the bytes before the first `\n`, or before EOF.
async fn read_request(read: impl AsyncRead + Unpin) -> std::io::Result<Framed> {
    let mut bytes = Vec::new();
    // One byte past the limit tells a request that ends right at it from a longer one.
    BufReader::new(read.take(MAX_REQUEST_BYTES as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .await?;
    match bytes.last() {
        Some(b'\n') => {
            bytes.pop();
            Ok(Framed::Request(bytes))
        }
        Some(_) | None if bytes.len() > MAX_REQUEST_BYTES => Ok(Framed::TooLong),
        Some(_) | None => Ok(Framed::Request(bytes)),
    }
}

/// Hand `command` to the daemon's main loop and wait for its answer.
async fn forward<T>(
    cmd_tx: &mpsc::Sender<Command>,
    command: Command,
    answer: oneshot::Receiver<T>,
    reply_timeout: Duration,
) -> Result<T> {
    tokio::time::timeout(CONN_TIMEOUT, cmd_tx.send(command))
        .await
        .context("daemon command queue full")?
        .context("daemon not accepting commands")?;
    tokio::time::timeout(reply_timeout, answer)
        .await
        .context("reply timeout")?
        .context("daemon dropped the reply channel")
}

/// Client side of the legacy protocol (`toggle` / `status` subcommands).
pub async fn send(paths: &Paths, request: &str) -> Result<String> {
    let stream = UnixStream::connect(&paths.socket)
        .await
        .map_err(|error| match error.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                anyhow::Error::new(NotRunning(paths.socket.clone(), error))
            }
            _ => anyhow::Error::new(error)
                .context(format!("cannot connect to {}", paths.socket.display())),
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

/// Client side of protocol v1.
pub(crate) async fn request(paths: &Paths, request: &v1::Request) -> Result<v1::Reply> {
    let line = serde_json::to_string(&v1::Envelope::new(request))?;
    let reply = send(paths, &line).await?;
    serde_json::from_str(&reply).with_context(|| format!("invalid reply from the daemon: {reply}"))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;

    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::{Command, MAX_REQUEST_BYTES, NotRunning, handle_conn, send, v1};
    use crate::config::Paths;

    /// Answer every command the way an idle daemon would.
    fn idle_daemon() -> mpsc::Sender<Command> {
        let (commands, mut received) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(command) = received.recv().await {
                match command {
                    Command::Toggle(reply) => {
                        let _ = reply.send("recording".to_string());
                    }
                    Command::Status(reply) => {
                        let _ = reply.send("idle".to_string());
                    }
                    Command::Hold(_, reply)
                    | Command::ShortcutRebind(reply)
                    | Command::ShortcutStatus(reply) => {
                        let _ = reply.send(v1::Reply::Ok);
                    }
                }
            }
        });
        commands
    }

    /// Send `request` over a fresh connection, then everything the daemon
    /// wrote back before closing it.
    async fn exchange(request: &[u8]) -> String {
        let (mut client, server) = UnixStream::pair().unwrap();
        tokio::spawn(handle_conn(server, idle_daemon()));
        client.write_all(request).await.unwrap();
        client.shutdown().await.unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).await.unwrap();
        reply
    }

    fn error_line(message: &str) -> String {
        format!(
            "{}\n",
            serde_json::to_string(&v1::Reply::Error {
                message: message.to_string()
            })
            .unwrap()
        )
    }

    #[tokio::test]
    async fn legacy_requests_keep_their_plain_text_replies() {
        assert_eq!(
            [
                exchange(b"status\n").await,
                exchange(b"toggle").await,
                exchange(b"dance\n").await,
            ],
            ["idle\n", "recording\n", ""]
        );
    }

    #[tokio::test]
    async fn v1_requests_get_json_replies() {
        let hold = r#"{"v":1,"type":"hold","edge":"press"}"#;
        assert_eq!(
            [
                exchange(format!("{hold}\n").as_bytes()).await,
                exchange(format!(" \t{hold}  \n").as_bytes()).await,
                exchange(br#"{"v":2,"type":"hold","edge":"press"}"#).await,
                exchange(br#"{"type":"hold","edge":"press"}"#).await,
                exchange(b"\x0c{\"v\":1,\"type\":\"hold\",\"edge\":\"press\"}\x0c").await,
            ],
            [
                "{\"type\":\"ok\"}\n".to_string(),
                "{\"type\":\"ok\"}\n".to_string(),
                error_line("unsupported protocol version 2; supported: 1"),
                error_line("missing protocol version"),
                "{\"type\":\"ok\"}\n".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_v1_requests_get_escaped_error_replies() {
        for request in [
            r#"{"v":1,"type":"say \"hi\"	"}"#,
            r#"{"v":1,"type":"hold","edge":"down"}"#,
            r#"{"v":1,"#,
        ] {
            let reply = exchange(request.as_bytes()).await;
            assert!(
                matches!(
                    serde_json::from_str(&reply),
                    Ok(v1::Reply::Error { message }) if message.starts_with("invalid request: ")
                ),
                "{reply}"
            );
        }
    }

    #[tokio::test]
    async fn requests_are_limited_in_length() {
        let hold = r#"{"v":1,"type":"hold","edge":"press"}"#;
        let longest = format!("{hold}{}", " ".repeat(MAX_REQUEST_BYTES - hold.len()));
        assert_eq!(
            [
                exchange(format!("{longest}\n").as_bytes()).await,
                exchange(format!("{longest} ").as_bytes()).await,
            ],
            [
                "{\"type\":\"ok\"}\n".to_string(),
                error_line(&format!("request exceeds {MAX_REQUEST_BYTES} bytes")),
            ]
        );
    }

    #[tokio::test]
    async fn requests_must_be_utf8() {
        assert_eq!(
            exchange(b"to\xffggle\n").await,
            error_line("request is not valid UTF-8")
        );
    }

    #[tokio::test]
    async fn only_a_missing_daemon_counts_as_not_running() {
        let dir = std::env::temp_dir().join(format!("voice-input-ipc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("locked")).unwrap();
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let paths = |socket: PathBuf| Paths {
            config_file: dir.join("config.toml"),
            socket,
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
        };

        let missing = send(&paths(dir.join("voice-input.sock")), "status").await;
        let locked = send(
            &paths(dir.join("locked").join("voice-input.sock")),
            "status",
        )
        .await;
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o700))
            .unwrap();

        assert!(missing.is_err_and(|error| error.downcast_ref::<NotRunning>().is_some()));
        assert!(locked.is_err_and(|error| error.downcast_ref::<NotRunning>().is_none()));
    }
}
