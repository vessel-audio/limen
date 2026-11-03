use std::sync::Arc;
use std::time::Duration;

use limen_core::{
    BoxFuture, ChatModel, Connection, ModelBackend, SignInPrompt, Subscription, Transcriber,
};

use super::{Backoff, is_outdated};

/// A model backend for bridge tests, which never reach a model.
struct NoModels;

impl ModelBackend for NoModels {
    fn transcriber(&self, _: Connection, _: String) -> anyhow::Result<Box<dyn Transcriber>> {
        anyhow::bail!("bridge tests transcribe nothing")
    }

    fn chat(&self, _: Connection, _: String) -> anyhow::Result<Box<dyn ChatModel>> {
        anyhow::bail!("bridge tests polish nothing")
    }

    fn models(&self, _: Connection) -> BoxFuture<'_, anyhow::Result<Vec<String>>> {
        Box::pin(async { anyhow::bail!("bridge tests list no models") })
    }

    fn sign_in(&self, _: Subscription, _: SignInPrompt) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { anyhow::bail!("bridge tests sign in to nothing") })
    }

    fn sign_out(&self, _: Subscription) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn signed_in(&self, _: Subscription) -> bool {
        false
    }
}

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

    use limen_core::Paths;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::{BridgeCommand, Env, SpawnGate, handle_command};

    let root = std::env::temp_dir().join(format!("limen-bridge-shutdown-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
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
        tokio::time::sleep(Duration::from_millis(50)).await;
        fs::remove_file(socket).unwrap();
    });
    let (events, mut event_rx) = tokio::sync::mpsc::channel(8);
    let mut stream = None;
    let mut spawn = SpawnGate { task: None };
    let mut stopped = false;
    let mut shortcuts = tokio::task::JoinSet::new();

    let env = Env {
        paths: paths.clone(),
        models: Arc::new(NoModels),
        signing: Default::default(),
    };
    handle_command(
        &env,
        &events,
        BridgeCommand::ShutdownDaemon,
        &mut stream,
        &mut spawn,
        &mut stopped,
        &mut shortcuts,
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

#[tokio::test]
async fn termination_removes_a_stale_socket_after_process_exit() {
    use limen_core::Paths;
    use std::fs;

    let root = std::env::temp_dir().join(format!("limen-bridge-terminate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    drop(tokio::net::UnixListener::bind(&paths.socket).unwrap());
    let socket = limen_core::SocketIdentity::for_path(&paths).unwrap();
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 0.05"])
        .spawn()
        .unwrap();

    super::wait_for_termination(
        &paths,
        limen_core::DaemonTermination {
            pid: child.id(),
            socket,
        },
    )
    .await
    .unwrap();

    child.wait().unwrap();
    assert!(!paths.socket.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn quit_stops_the_bridge_while_another_sender_is_alive() {
    use limen_core::Paths;
    use std::io::{BufRead, Write};

    let root = std::env::temp_dir().join(format!("limen-bridge-quit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufReader::new(&mut stream)
            .read_line(&mut request)
            .unwrap();
        assert!(request.contains("subscribe"));
        stream
            .write_all(b"{\"type\":\"hello\",\"protocol\":1,\"daemon_version\":\"test\"}\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    });
    let (commands, mut events, bridge) = super::start(paths, Arc::new(NoModels));
    let retained = commands.clone();
    assert!(matches!(
        events.blocking_recv(),
        Some(super::BridgeEvent::Msg(crate::model::Msg::Hello { .. }))
    ));

    commands.blocking_send(super::BridgeCommand::Quit).unwrap();

    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = bridge.join();
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(1)).is_ok(),
        "bridge stayed alive after Quit"
    );
    drop(retained);
    server.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn termination_does_not_remove_a_replacement_socket() {
    use limen_core::Paths;
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "limen-bridge-terminate-owner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    drop(tokio::net::UnixListener::bind(&paths.socket).unwrap());
    let socket = limen_core::SocketIdentity::for_path(&paths).unwrap();
    fs::remove_file(&paths.socket).unwrap();
    let _replacement = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 0.05"])
        .spawn()
        .unwrap();

    super::wait_for_termination(
        &paths,
        limen_core::DaemonTermination {
            pid: child.id(),
            socket,
        },
    )
    .await
    .unwrap();

    child.wait().unwrap();
    assert!(paths.socket.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hello_protocol_mismatch_offers_termination_without_keeping_the_stream() {
    use limen_core::Paths;
    use std::io::{BufRead, Write};

    let root = std::env::temp_dir().join(format!("limen-bridge-protocol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufReader::new(&mut stream)
            .read_line(&mut request)
            .unwrap();
        assert!(request.contains("subscribe"));
        stream
            .write_all(b"{\"type\":\"hello\",\"protocol\":2,\"daemon_version\":\"future\"}\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    });
    let (commands, mut events, bridge) = super::start(paths, Arc::new(NoModels));
    let first = events.blocking_recv();
    commands.blocking_send(super::BridgeCommand::Quit).unwrap();
    let _ = bridge.join();
    server.join().unwrap();

    assert!(matches!(
        first,
        Some(super::BridgeEvent::Msg(crate::model::Msg::DaemonError { message }))
            if message.contains("protocol 2")
    ));
    assert!(matches!(
        events.blocking_recv(),
        Some(super::BridgeEvent::OfferTerminate)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_waits_for_the_full_cleanup_budget() {
    use limen_core::Paths;
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "limen-bridge-shutdown-budget-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        config_file: root.join("config.toml"),
        socket: root.join("limen.sock"),
        log_file: root.join("daemon.log"),
        cache_dir: root.join("cache"),
    };
    drop(tokio::net::UnixListener::bind(&paths.socket).unwrap());
    let socket = paths.socket.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(6)).await;
        fs::remove_file(socket).unwrap();
    });

    super::wait_for_socket_removal(&paths).await.unwrap();

    assert!(!paths.socket.exists());
    fs::remove_dir_all(root).unwrap();
}
