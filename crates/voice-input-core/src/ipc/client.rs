//! Clients for legacy requests, protocol v1, and subscription streams.

use std::io::ErrorKind;

use anyhow::{Context, Result, bail};
use futures_util::Stream;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{NotRunning, v1};
use crate::config::Paths;
use crate::daemon::snapshot::MAX_PUSH_BYTES;
use crate::ipc::wire::Push;

async fn connect(paths: &Paths) -> Result<UnixStream> {
    UnixStream::connect(&paths.socket)
        .await
        .map_err(|error| match error.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                anyhow::Error::new(NotRunning(paths.socket.clone(), error))
            }
            _ => anyhow::Error::new(error)
                .context(format!("cannot connect to {}", paths.socket.display())),
        })
}

/// Client side of the legacy protocol (`toggle` / `status` subcommands).
pub async fn send(paths: &Paths, request: &str) -> Result<String> {
    let stream = connect(paths).await?;
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
pub async fn request(paths: &Paths, request: &v1::Request) -> Result<v1::Reply> {
    let line = serde_json::to_string(&v1::Envelope::new(request))?;
    let reply = send(paths, &line).await?;
    serde_json::from_str(&reply).with_context(|| format!("invalid reply from the daemon: {reply}"))
}

/// Open a long-lived subscription and parse each bounded push line.
pub async fn subscribe(paths: &Paths) -> Result<impl Stream<Item = Result<Push>>> {
    let stream = connect(paths).await?;
    let (read, mut write) = stream.into_split();
    write
        .write_all(b"{\"v\":1,\"type\":\"subscribe\"}\n")
        .await?;
    write.flush().await?;
    let reader = BufReader::new(read);
    Ok(futures_util::stream::try_unfold(
        (reader, write),
        |(mut reader, write)| {
            Box::pin(async move {
                let Some(line) = read_push_line(&mut reader).await? else {
                    return Ok(None);
                };
                let push = serde_json::from_slice(&line)
                    .with_context(|| format!("invalid push from the daemon: {}", line.len()))?;
                Ok(Some((push, (reader, write))))
            })
        },
    ))
}

async fn read_push_line<R>(reader: &mut BufReader<R>) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len() + newline > MAX_PUSH_BYTES {
                bail!("push exceeds {MAX_PUSH_BYTES} bytes");
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(Some(line));
        }
        if line.len() + available.len() > MAX_PUSH_BYTES {
            bail!("push exceeds {MAX_PUSH_BYTES} bytes");
        }
        let consumed = available.len();
        line.extend_from_slice(available);
        reader.consume(consumed);
    }
}

#[cfg(test)]
mod tests {
    use futures_util::TryStreamExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use crate::config::Paths;
    use crate::ipc::wire::Push;

    use super::{MAX_PUSH_BYTES, read_push_line};

    #[tokio::test]
    async fn subscription_rejections_keep_the_daemon_message() {
        let rejection =
            r#"{"type":"error","message":"invalid request: unknown variant `subscribe`"}"#;
        let push: Push = serde_json::from_str(rejection).unwrap();
        assert_eq!(
            push,
            Push::Error {
                message: "invalid request: unknown variant `subscribe`".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn push_lines_are_bounded_before_buffering() {
        let bytes = vec![b'x'; MAX_PUSH_BYTES + 1];
        let mut reader = BufReader::new(bytes.as_slice());
        let error = read_push_line(&mut reader).await.unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[tokio::test]
    async fn push_lines_accept_the_exact_cap() {
        let mut bytes = vec![b'x'; MAX_PUSH_BYTES];
        bytes.push(b'\n');
        let mut reader = BufReader::new(bytes.as_slice());
        let line = read_push_line(&mut reader).await.unwrap();
        assert_eq!(line, Some(vec![b'x'; MAX_PUSH_BYTES]));
    }

    #[tokio::test]
    async fn a_subscription_parses_hello_from_a_real_socket() {
        let socket = std::env::temp_dir().join(format!(
            "voice-input-subscribe-client-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = String::new();
            BufReader::new(&mut stream)
                .read_line(&mut request)
                .await
                .unwrap();
            assert_eq!(request, "{\"v\":1,\"type\":\"subscribe\"}\n");
            stream
                .write_all(
                    format!("{}\n", serde_json::to_string(&Push::hello()).unwrap()).as_bytes(),
                )
                .await
                .unwrap();
            let mut byte = [0];
            let read = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                tokio::io::AsyncReadExt::read(&mut stream, &mut byte),
            )
            .await;
            assert!(read.is_err(), "client closed the subscription write half");
            stream
                .write_all(
                    format!(
                        "{}\n",
                        serde_json::to_string(&Push::Error {
                            message: "still connected".to_string(),
                        })
                        .unwrap()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let paths = Paths {
            config_file: socket.with_extension("toml"),
            socket: socket.clone(),
            log_file: socket.with_extension("log"),
            cache_dir: socket.with_extension("cache"),
        };
        let mut subscription = super::subscribe(&paths).await.unwrap();
        assert!(matches!(
            subscription.try_next().await.unwrap(),
            Some(Push::Hello { protocol: 1, .. })
        ));
        assert_eq!(
            subscription.try_next().await.unwrap(),
            Some(Push::Error {
                message: "still connected".to_string(),
            })
        );
        drop(subscription);
        server.await.unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
