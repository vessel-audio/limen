//! Bounded daemon requests and process lifecycle transitions.

use std::time::Duration;

use anyhow::Result;
use voice_input_core::{DaemonTermination, Paths, spawn_daemon, v1};

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
    tokio::time::timeout(
        Duration::from_secs(3),
        voice_input_core::request(paths, request),
    )
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::UnixStream::connect(&paths.socket).await {
                Ok(stream) => drop(stream),
                // ConnectionRefused can precede the old daemon's unlink; only
                // a missing path proves that its socket cleanup has finished.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
                Err(error) => return Err(StartError::Failed(error.to_string())),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| StartError::Failed("daemon did not stop within 5s".to_string()))?
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
            let exited = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(stat) => stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, state)| state.starts_with('Z')),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
            };
            match exited {
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
