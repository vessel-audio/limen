use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::spawn_with_program;
use crate::config::Paths as CorePaths;
use crate::daemon::snapshot::{ConfigSummary, DictationStatus, StateSnapshot};
use crate::dictation::DictationState;
use crate::ipc::wire::{Push, WireSnapshot};
use crate::ipc::{request, v1};
use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus};
use crate::shortcut::{Backend, Shortcut};

static NEXT_SANDBOX: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    root: PathBuf,
    paths: CorePaths,
    program: PathBuf,
    mode: OsString,
}

impl Sandbox {
    fn new(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "voice-input-spawn-{mode}-{}-{}",
            std::process::id(),
            NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("run")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        let paths = CorePaths {
            config_file: root.join("config.toml"),
            socket: root.join("run/voice-input.sock"),
            log_file: root.join("state/daemon.log"),
            cache_dir: root.join("cache"),
        };
        let program = root.join("fake-daemon.sh");
        fs::write(
            &program,
            "#!/bin/sh\nexec \"$VOICE_INPUT_TEST_BIN\" --exact daemon::spawn::tests::fake_daemon_process --nocapture\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            root,
            paths,
            program,
            mode: mode.into(),
        }
    }

    fn environment(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("VOICE_INPUT_FAKE_MODE".into(), self.mode.clone()),
            (
                "VOICE_INPUT_TEST_BIN".into(),
                std::env::current_exe().unwrap().into_os_string(),
            ),
            (
                "VOICE_INPUT_FAKE_PROGRAM".into(),
                self.program.clone().into(),
            ),
            (
                "VOICE_INPUT_FAKE_SOCKET".into(),
                self.paths.socket.clone().into(),
            ),
            (
                "VOICE_INPUT_FAKE_LOG".into(),
                self.paths.log_file.clone().into(),
            ),
            (
                "VOICE_INPUT_FAKE_PID_FILE".into(),
                self.root.join("daemon.pid").into(),
            ),
        ]
    }

    fn stop(&self) -> v1::Reply {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(request(&self.paths, &v1::Request::Shutdown))
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Reap(u32);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.0.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[tokio::test]
async fn a_spawned_daemon_answers_hello_in_a_new_session() {
    let sandbox = Sandbox::new("ready");
    fs::write(&sandbox.paths.log_file, "existing log line\n").unwrap();
    let pid = spawn_with_program(&sandbox.paths, &sandbox.program, &sandbox.environment())
        .await
        .unwrap();
    let _reap = Reap(pid);
    let session = Command::new("ps")
        .args(["-o", "sid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&session.stdout).trim(),
        pid.to_string()
    );
    assert_eq!(
        request(&sandbox.paths, &v1::Request::Shutdown)
            .await
            .unwrap(),
        v1::Reply::Ok
    );
    assert!(
        fs::read_to_string(&sandbox.paths.log_file)
            .unwrap()
            .contains("fake daemon started")
    );
    assert_eq!(
        fs::metadata(&sandbox.paths.log_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn an_exiting_daemon_returns_its_log_tail() {
    let sandbox = Sandbox::new("exit");
    let error = spawn_with_program(&sandbox.paths, &sandbox.program, &sandbox.environment())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("fake daemon started"));
}

#[tokio::test]
async fn a_timeout_returns_the_last_twenty_log_lines() {
    let sandbox = Sandbox::new("timeout");
    let error = spawn_with_program(&sandbox.paths, &sandbox.program, &sandbox.environment())
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("line-25"));
    assert!(!message.contains("line-1\n"));
    let pid = fs::read_to_string(sandbox.root.join("daemon.pid"))
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(
        !Command::new("ps")
            .args(["-p", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn a_missing_log_is_reported_when_startup_fails() {
    let sandbox = Sandbox::new("silent");
    let error = spawn_with_program(&sandbox.paths, &sandbox.program, &sandbox.environment())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot read"));
}

#[test]
fn a_detached_daemon_outlives_the_gui_side_process() {
    let sandbox = Sandbox::new("ready");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::spawn::tests::fake_gui_process",
            "--nocapture",
        ])
        .envs(sandbox.environment())
        .status()
        .unwrap();
    assert!(status.success());
    let pid: u32 = fs::read_to_string(sandbox.root.join("daemon.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let _reap = Reap(pid);
    // The GUI-side process has exited, but its detached daemon still answers.
    assert_eq!(sandbox.stop(), v1::Reply::Ok);
}

#[test]
fn fake_gui_process() {
    let Ok(program) = std::env::var("VOICE_INPUT_FAKE_PROGRAM") else {
        return;
    };
    let paths = CorePaths {
        config_file: PathBuf::from("/nonexistent/config.toml"),
        socket: PathBuf::from(std::env::var("VOICE_INPUT_FAKE_SOCKET").unwrap()),
        log_file: PathBuf::from(std::env::var("VOICE_INPUT_FAKE_LOG").unwrap()),
        cache_dir: PathBuf::from("/nonexistent/cache"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pid = runtime
        .block_on(spawn_with_program(&paths, Path::new(&program), &[]))
        .unwrap();
    fs::write(
        std::env::var("VOICE_INPUT_FAKE_PID_FILE").unwrap(),
        pid.to_string(),
    )
    .unwrap();
}

#[test]
fn fake_daemon_process() {
    let Ok(mode) = std::env::var("VOICE_INPUT_FAKE_MODE") else {
        return;
    };
    let paths = CorePaths {
        config_file: PathBuf::from("/nonexistent/config.toml"),
        socket: PathBuf::from(std::env::var("VOICE_INPUT_FAKE_SOCKET").unwrap()),
        log_file: PathBuf::from(std::env::var("VOICE_INPUT_FAKE_LOG").unwrap()),
        cache_dir: PathBuf::from("/nonexistent/cache"),
    };
    if let Ok(pid_file) = std::env::var("VOICE_INPUT_FAKE_PID_FILE") {
        fs::write(pid_file, std::process::id().to_string()).unwrap();
    }
    match mode.as_str() {
        "ready" => fake_server(&paths),
        "exit" => eprintln!("fake daemon started"),
        "timeout" => {
            for index in 1..=25 {
                eprintln!("line-{index}");
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        "silent" => {
            let _ = fs::remove_file(&paths.log_file);
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        other => panic!("unknown fake daemon mode {other}"),
    }
}

fn fake_server(paths: &CorePaths) {
    eprintln!("fake daemon started");
    let _ = fs::remove_file(&paths.socket);
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    for expected in ["subscribe", "shutdown"] {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufReader::new(&mut stream)
            .read_line(&mut request)
            .unwrap();
        assert!(request.contains(expected), "{request}");
        match expected {
            "subscribe" => {
                let state = StateSnapshot {
                    take: 0,
                    dictation: DictationStatus {
                        state: DictationState::Idle,
                        since_ms: None,
                    },
                    queued: false,
                    history: Vec::new(),
                    last_error: None,
                    config: ConfigSummary {
                        input_method: "fcitx5".to_string(),
                        provider_model: "test".to_string(),
                        provider_url: "http://127.0.0.1".to_string(),
                        polish_enabled: false,
                    },
                };
                let shortcut = ShortcutStatus {
                    backend: Backend::None,
                    session: SessionState::Disabled,
                    preferred: PerAction {
                        toggle: Shortcut::DEFAULT_TOGGLE,
                        hold: Shortcut::DEFAULT_HOLD,
                    },
                    actual: PerAction::default(),
                    last_error: None,
                };
                for push in [
                    Push::hello(),
                    Push::Snapshot(Box::new(WireSnapshot::new(1, state, shortcut))),
                ] {
                    stream
                        .write_all(
                            format!("{}\n", serde_json::to_string(&push).unwrap()).as_bytes(),
                        )
                        .unwrap();
                }
            }
            "shutdown" => {
                stream.write_all(b"{\"type\":\"ok\"}\n").unwrap();
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn another_uid_is_never_signalled() {
    let error = super::peer_pid(123, u32::MAX).unwrap_err();
    assert!(error.to_string().contains("another UID"));
}
