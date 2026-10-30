//! Bounded ownership of resident asynchronous tasks.

use tokio::task::JoinHandle;

const JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How a task owner requests termination before joining it.
pub(crate) enum StopMode {
    /// Let the task observe its closed mailbox or finish its current operation.
    Graceful,
    /// Request cancellation immediately.
    Abort,
}

/// The terminal ownership state of a resident task.
pub(crate) enum JoinOutcome {
    /// The task has ended and can no longer use its resources.
    Joined,
    /// The join deadline elapsed; the task remains detached.
    Abandoned,
}

/// Stop and join a task for at most five seconds, logging an abandoned task.
pub(crate) async fn join_task(name: &str, handle: JoinHandle<()>, mode: StopMode) -> JoinOutcome {
    if matches!(mode, StopMode::Abort) {
        handle.abort();
    }
    match tokio::time::timeout(JOIN_TIMEOUT, handle).await {
        Ok(Ok(())) => JoinOutcome::Joined,
        Ok(Err(error)) if error.is_cancelled() => JoinOutcome::Joined,
        Ok(Err(error)) => {
            tracing::warn!(name, %error, "resident task failed");
            JoinOutcome::Joined
        }
        Err(_) => {
            tracing::warn!(name, "resident task did not stop within 5s; abandoning it");
            JoinOutcome::Abandoned
        }
    }
}
