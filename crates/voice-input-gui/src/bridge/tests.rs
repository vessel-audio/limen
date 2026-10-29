use std::time::Duration;

use super::{Backoff, is_outdated};

#[test]
fn reconnect_backoff_doubles_to_a_cap_and_resets_after_hello() {
    let mut backoff = Backoff::default();
    let intervals = std::iter::from_fn(|| Some(backoff.next()))
        .take(5)
        .collect::<Vec<_>>();
    assert_eq!(
        intervals,
        [
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(5),
        ]
    );
    assert_eq!(backoff.next(), Duration::from_secs(5));
    backoff = Backoff::default();
    assert_eq!(backoff.next(), Duration::from_millis(500));
}

#[test]
fn outdated_errors_are_limited_to_protocol_rejections() {
    assert!(is_outdated("invalid request: unknown variant `subscribe`"));
    assert!(is_outdated("unknown variant `toggle`"));
    assert!(!is_outdated("too many subscribers"));
    assert!(!is_outdated("not recording"));
}

#[tokio::test]
async fn shutdown_waits_for_socket_cleanup_before_reporting_stopped() {
    use std::fs;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use voice_input_core::Paths;

    use super::{BridgeCommand, SpawnGate, handle_command};

    let root = std::env::temp_dir().join(format!(
        "voice-input-bridge-shutdown-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("voice-input.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let socket = paths.socket.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut request)
            .await
            .unwrap();
        assert!(request.contains("shutdown"));
        stream.write_all(b"{\"type\":\"ok\"}\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(stream);
        drop(listener);
        fs::remove_file(socket).unwrap();
    });
    let (events, mut event_rx) = tokio::sync::mpsc::channel(8);
    let mut stream = None;
    let mut spawn = SpawnGate { task: None };
    let mut stopped = false;

    handle_command(
        &paths,
        &events,
        BridgeCommand::ShutdownDaemon,
        &mut stream,
        &mut spawn,
        &mut stopped,
    )
    .await;

    assert!(
        !paths.socket.exists(),
        "shutdown was reported before socket cleanup"
    );
    assert!(stopped);
    assert!(matches!(
        event_rx.recv().await,
        Some(super::BridgeEvent::Msg(crate::model::Msg::DaemonStopped))
    ));
    server.await.unwrap();
    fs::remove_dir_all(root).unwrap();
}
