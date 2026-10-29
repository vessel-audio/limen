//! Bounded daemon requests and process lifecycle transitions.

use std::time::Duration;

use anyhow::Result;
use voice_input_core::{Paths, spawn_daemon, v1};

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
                Ok(stream) => {
                    drop(stream);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                // ConnectionRefused can precede the old daemon's unlink; only
                // a missing path proves that its socket cleanup has finished.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(StartError::Failed(error.to_string())),
            }
        }
    })
    .await
    .map_err(|_| StartError::Failed("daemon did not stop within 5s".to_string()))?
}

pub(super) fn is_outdated(message: &str) -> bool {
    message.contains("invalid request") || message.contains("unknown variant")
}
