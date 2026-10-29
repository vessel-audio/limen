//! Real-daemon shutdown coverage with no desktop portal or user state.

use futures_util::TryStreamExt as _;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT_SANDBOX: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "voice-input-daemon-lifecycle-{}-{}",
            std::process::id(),
            NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for directory in ["config/voice-input", "run", "state", "data", "cache", "bin"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(
            root.join("config/voice-input/config.toml"),
            "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"test\"\n\n[shortcut]\nbackend = \"none\"\ntoggle = \"Super+Z\"\nhold = \"Super+X\"\n",
        )
        .unwrap();
        Self { root }
    }

    fn socket(&self) -> PathBuf {
        self.root.join("run/voice-input.sock")
    }

    fn spawn(&self) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_voice-input"))
            .arg("daemon")
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CURRENT_DESKTOP", "GNOME")
            .envs(
                std::env::var_os("SSL_CERT_FILE").map(|certificate| ("SSL_CERT_FILE", certificate)),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Daemon {
            child,
            finished: false,
        }
    }
}

impl Sandbox {
    fn fake_capture(&self) {
        let program = self.root.join("bin/pw-record");
        let script = format!(
            "#!/bin/sh\necho $$ > {}\nwhile :; do\n  dd if=/dev/zero bs=3200 count=1 2>/dev/null\n  sleep 1\ndone\n",
            self.root.join("capture.pid").display()
        );
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn wait_capture(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(pid) = fs::read_to_string(self.root.join("capture.pid")) {
                let pid = pid.trim().parse::<u32>().unwrap();
                // SAFETY: signal zero only checks for the fake capture process.
                assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0);
                return pid;
            }
            assert!(Instant::now() < deadline, "capture did not start");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn toggle(&self, socket: &PathBuf) {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(
                b"toggle
",
            )
            .unwrap();
        let mut reply = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut reply)
            .unwrap();
        assert_eq!(
            reply,
            "recording
"
        );
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Daemon {
    child: Child,
    finished: bool,
}

impl Daemon {
    fn wait_ready(&mut self, socket: &PathBuf) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(!self.finished, "daemon exited before becoming ready");
            if let Some(status) = self.child.try_wait().unwrap() {
                self.finished = true;
                panic!("daemon exited before becoming ready: {status}");
            }
            if socket.exists()
                && let Ok(mut stream) = UnixStream::connect(socket)
            {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream.write_all(b"status\n").unwrap();
                let mut reply = String::new();
                std::io::BufReader::new(stream)
                    .read_line(&mut reply)
                    .unwrap();
                assert_eq!(reply, "idle\n");
                assert_eq!(
                    fs::metadata(socket).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not become ready");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.finished = true;
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("daemon did not exit within the shutdown deadline");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn signal(&mut self, signal: &str) {
        let signal = match signal {
            "-TERM" => libc::SIGTERM,
            "-INT" => libc::SIGINT,
            other => panic!("unsupported test signal {other}"),
        };
        // SAFETY: this targets only the child process created by this test.
        let result = unsafe { libc::kill(self.child.id() as i32, signal) };
        assert_eq!(result, 0, "failed to signal daemon");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[tokio::test]
async fn sigterm_and_shutdown_clean_the_socket_and_allow_immediate_restart() {
    let sandbox = Sandbox::new();
    let socket = sandbox.socket();
    let mut daemon = sandbox.spawn();
    daemon.wait_ready(&socket);
    let first_pid = daemon.child.id();
    daemon.signal("-TERM");
    assert!(daemon.wait_exit().success());
    assert!(!socket.exists());
    let children = fs::read_to_string(format!("/proc/{first_pid}/task/{first_pid}/children"));
    for child in children
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pid| fs::read_to_string(format!("/proc/{pid}/comm")).ok())
    {
        assert!(
            !child.contains("pw-record"),
            "capture child survived: {child}"
        );
    }

    let mut restarted = sandbox.spawn();
    restarted.wait_ready(&socket);
    let paths = voice_input_core::Paths {
        config_file: sandbox.root.join("config/voice-input/config.toml"),
        socket: socket.clone(),
        log_file: sandbox.root.join("state/voice-input/daemon.log"),
        cache_dir: sandbox.root.join("cache/voice-input"),
    };
    let mut subscription = voice_input_core::subscribe(&paths).await.unwrap();
    assert!(matches!(
        subscription.try_next().await.unwrap(),
        Some(voice_input_core::Push::Hello { protocol: 1, .. })
    ));
    assert!(matches!(
        subscription.try_next().await.unwrap(),
        Some(voice_input_core::Push::Snapshot(snapshot))
            if snapshot.seq == 1
                && matches!(
                    snapshot.state.dictation.state,
                    voice_input_core::DictationState::Idle
                )
    ));
    drop(subscription);

    let reply = voice_input_core::request(&paths, &voice_input_core::v1::Request::Shutdown)
        .await
        .unwrap();
    assert_eq!(reply, voice_input_core::v1::Reply::Ok);
    assert!(restarted.wait_exit().success());
    assert!(!socket.exists());
}

#[test]
fn sigint_reaps_an_active_capture() {
    let sandbox = Sandbox::new();
    sandbox.fake_capture();
    let socket = sandbox.socket();
    let mut daemon = sandbox.spawn();
    daemon.wait_ready(&socket);
    sandbox.toggle(&socket);
    let capture_pid = sandbox.wait_capture();

    daemon.signal("-INT");

    assert!(daemon.wait_exit().success());
    assert!(!socket.exists());
    // SAFETY: the fake capture must have been killed and waited for, not
    // merely detached. Signal zero must fail with ESRCH after reaping.
    assert_ne!(unsafe { libc::kill(capture_pid as i32, 0) }, 0);
}
