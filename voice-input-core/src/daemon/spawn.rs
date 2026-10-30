//! Detached daemon spawning with bounded readiness and useful log failures.

use std::fs::OpenOptions;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::TryStreamExt;
use tokio::process::{Child, Command};

use crate::config::Paths;
use crate::ipc::{SocketIdentity, subscribe, wire::Push};
use crate::shortcut::program::Program;

const READY_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_INTERVAL: Duration = Duration::from_millis(50);
const LOG_TAIL_LINES: usize = 20;

/// Start the stable voice-input executable as a detached daemon.
pub async fn spawn_daemon(paths: &Paths) -> Result<()> {
    let program = Program::find()?;
    spawn_with_program(paths, program.path(), &[])
        .await
        .map(drop)
}

async fn spawn_with_program(
    paths: &Paths,
    program: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<u32> {
    let parent = paths
        .log_file
        .parent()
        .context("daemon log path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&paths.log_file)
        .with_context(|| format!("failed to open {}", paths.log_file.display()))?;
    std::fs::set_permissions(&paths.log_file, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict {}", paths.log_file.display()))?;
    let stdout = log.try_clone()?;
    let mut command = Command::new(program);
    command
        .arg("daemon")
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(log));
    // SAFETY: setsid has no pointers or borrowed data. The child calls it
    // before exec, with this single-threaded command setup as its only work.
    unsafe {
        command.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {}", program.display()))?;
    let pid = child.id().context("daemon process has no ID")?;
    let result = wait_for_hello(paths, &mut child).await;
    match result {
        Ok(()) => Ok(pid),
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error.context(format!(
                "daemon startup failed; last {LOG_TAIL_LINES} log lines:\n{}",
                log_tail(paths)
            )))
        }
    }
}

async fn wait_for_hello(paths: &Paths, child: &mut Child) -> Result<()> {
    tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            if let Some(status) = child.try_wait().context("failed to wait for daemon")? {
                bail!("daemon exited with {status}");
            }
            let error = match subscribe(paths).await {
                Ok(mut stream) => match stream.try_next().await {
                    Ok(Some(Push::Hello { .. })) => return Ok(()),
                    Ok(Some(other)) => anyhow!("expected hello, received {other:?}"),
                    Ok(None) => anyhow!("daemon closed the subscription"),
                    Err(error) => error,
                },
                Err(error) => error,
            };
            tracing::debug!(error = %format!("{error:#}"), "daemon is not ready");
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow!(
            "daemon did not answer hello within {}s",
            READY_TIMEOUT.as_secs()
        )
    })?
}

fn log_tail(paths: &Paths) -> String {
    match std::fs::read_to_string(&paths.log_file) {
        Ok(contents) => {
            let lines = contents
                .lines()
                .rev()
                .take(LOG_TAIL_LINES)
                .collect::<Vec<_>>();
            match lines.is_empty() {
                true => "(log is empty)".to_string(),
                false => lines.into_iter().rev().collect::<Vec<_>>().join("\n"),
            }
        }
        Err(error) => format!("(cannot read {}: {error})", paths.log_file.display()),
    }
}

#[cfg(test)]
mod tests;

/// Read and validate the daemon PID from a Unix peer's credentials.
pub async fn daemon_peer_pid(paths: &Paths) -> Result<u32> {
    daemon_peer(paths).await.map(|(pid, _)| pid)
}

async fn daemon_peer(paths: &Paths) -> Result<(u32, SocketIdentity)> {
    let socket = SocketIdentity::for_path(paths)?;
    let stream = tokio::net::UnixStream::connect(&paths.socket)
        .await
        .with_context(|| format!("cannot connect to {}", paths.socket.display()))?;
    let credentials = stream.peer_cred().context("cannot read peer credentials")?;
    let pid = peer_pid(
        credentials.pid().context("peer has no PID")?,
        credentials.uid(),
    )?;
    match SocketIdentity::for_path(paths)? == socket {
        true => Ok((pid, socket)),
        false => bail!("daemon socket changed while checking its owner; retry"),
    }
}

/// Validated process and socket identities captured before daemon termination.
#[derive(Debug)]
pub struct DaemonTermination {
    /// Same-UID daemon process that received SIGTERM.
    pub pid: u32,
    /// Socket path identity captured before signaling that process.
    pub socket: SocketIdentity,
}

/// SIGTERM an outdated daemon after the caller has obtained user confirmation.
/// Return identities needed to wait and clean up without touching a replacement.
pub async fn terminate_daemon(paths: &Paths) -> Result<DaemonTermination> {
    let (pid, socket) = daemon_peer(paths).await?;
    // SAFETY: kill is called with a validated positive PID and SIGTERM, with no
    // pointers or borrowed memory. UID was checked before this call.
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result == -1 {
        return Err(std::io::Error::last_os_error()).context("failed to terminate daemon");
    }
    Ok(DaemonTermination { pid, socket })
}

fn peer_pid(pid: i32, uid: u32) -> Result<u32> {
    // SAFETY: geteuid has no arguments, pointers, or memory-safety preconditions.
    let effective = unsafe { libc::geteuid() };
    if uid != effective {
        bail!("refusing to signal a daemon owned by another UID");
    }
    if pid <= 0 {
        bail!("peer has an invalid PID");
    }
    Ok(pid as u32)
}
