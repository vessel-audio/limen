//! Bounded daemon requests and process lifecycle transitions.

use std::time::Duration;

use anyhow::Result;
use limen_core::{DaemonTermination, Paths, spawn_daemon, v1};

// Four bounded task joins plus time for polling the final socket unlink.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(25);

pub(super) type StartResult = Result<(), StartError>;

#[derive(Debug)]
pub(super) enum StartError {
    Outdated(String),
    Failed(String),
}

pub(super) async fn request_with_timeout(
    paths: &Paths,
    request: &v1::Request,
) -> Result<v1::Reply> {
    tokio::time::timeout(Duration::from_secs(3), limen_core::request(paths, request))
        .await
        .map_err(|_| anyhow::anyhow!("daemon request timed out"))?
}

pub(super) async fn restart(paths: &Paths) -> StartResult {
    match request_with_timeout(paths, &v1::Request::Shutdown).await {
        Ok(v1::Reply::Ok) => {
            wait_for_socket_removal(paths).await?;
            spawn_daemon(paths)
                .await
                .map_err(|error| StartError::Failed(format!("{error:#}")))
        }
        Ok(v1::Reply::Error { message }) if is_outdated(&message) => {
            Err(StartError::Outdated(message))
        }
        Ok(v1::Reply::Error { message }) => Err(StartError::Failed(message)),
        Ok(_) => Err(StartError::Failed("unexpected shutdown reply".to_string())),
        Err(error) => Err(StartError::Failed(format!("{error:#}"))),
    }
}

pub(super) async fn wait_for_socket_removal(paths: &Paths) -> Result<(), StartError> {
    tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        loop {
            match tokio::net::UnixStream::connect(&paths.socket).await {
                Ok(stream) => drop(stream),
                // ConnectionRefused can precede the old daemon's unlink; only
                // a missing path proves that its socket cleanup has finished.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
                    ) => {}
                Err(error) => return Err(StartError::Failed(error.to_string())),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        StartError::Failed(format!(
            "daemon did not stop within {}s",
            SHUTDOWN_TIMEOUT.as_secs()
        ))
    })?
}

pub(super) fn is_outdated(message: &str) -> bool {
    message.contains("invalid request") || message.contains("unknown variant")
}

pub(super) async fn wait_for_termination(
    paths: &Paths,
    termination: DaemonTermination,
) -> Result<(), StartError> {
    let pid = termination.pid;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match process_exited(pid) {
                false => tokio::time::sleep(Duration::from_millis(50)).await,
                true => {
                    return termination
                        .socket
                        .remove_if_owned(paths)
                        .map_err(|error| StartError::Failed(format!("{error:#}")));
                }
            }
        }
    })
    .await
    .map_err(|_| StartError::Failed("daemon did not exit within 5s".to_string()))?
}

/// A zombie still has a /proc entry, so read the state rather than testing
/// for existence.
#[cfg(target_os = "linux")]
fn process_exited(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, state)| state.starts_with('Z')),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// A zombie child still answers kill(2) probes, so ask the kernel for the
/// process status instead; reaping here would break the caller's own wait().
#[cfg(target_os = "macos")]
fn process_exited(pid: u32) -> bool {
    // SAFETY: info is zeroed and the call writes at most one struct into it.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    // SAFETY: proc_pidinfo only writes into the buffer it is pointed at,
    // sized by the final argument.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdshortinfo as *mut libc::c_void,
            std::mem::size_of::<libc::proc_bsdshortinfo>() as i32,
        )
    };
    match written > 0 {
        // The kernel says whether the process exists; SZOMB counts as gone.
        true => info.pbsi_status == libc::SZOMB,
        // ESRCH (or any lookup failure) means the process is gone.
        false => true,
    }
}
