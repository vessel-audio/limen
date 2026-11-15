//! Where a frontend's daemon runs: a detached `limen daemon` process, or a
//! runtime thread inside the frontend's own process.
//!
//! The macOS app hosts the daemon itself. Carbon hotkey events reach only a
//! process whose main thread runs an application event loop, and the
//! microphone and Accessibility grants belong to the app bundle, so a
//! detached child would get neither.

#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::Duration;

use anyhow::Result;

use super::spawn::spawn_daemon;
#[cfg(target_os = "macos")]
use crate::backend::ModelBackend;
use crate::config::Paths;

/// How long a stopping in-app daemon gets before a restart gives up on it.
#[cfg(target_os = "macos")]
const RESTART_TIMEOUT: Duration = Duration::from_secs(5);
/// Four bounded task joins plus the socket unlink, as for a detached daemon.
#[cfg(target_os = "macos")]
const STOP_TIMEOUT: Duration = Duration::from_secs(25);
#[cfg(target_os = "macos")]
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long one shutdown request may take.
#[cfg(target_os = "macos")]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a stop asks a runtime that is still binding its socket again.
#[cfg(target_os = "macos")]
const SHUTDOWN_RETRY: Duration = Duration::from_millis(100);

/// Where the daemon a frontend starts runs.
#[derive(Clone)]
pub enum DaemonHost {
    /// A detached `limen daemon` process that outlives the frontend.
    Detached,
    /// A runtime thread inside this process, stopped with it.
    #[cfg(target_os = "macos")]
    Embedded(EmbeddedDaemon),
}

impl DaemonHost {
    /// Host the daemon in this process. Call once, on the main thread; the
    /// caller's event loop must call [`crate::EventPump::drain_commands`]
    /// regularly, or no hotkey is ever registered.
    #[cfg(target_os = "macos")]
    pub fn embedded(backend: Arc<dyn ModelBackend>) -> (Self, crate::EventPump) {
        let pump = crate::shortcut::carbon::install_event_pump();
        let daemon = EmbeddedDaemon {
            backend,
            thread: Arc::default(),
        };
        (Self::Embedded(daemon), pump)
    }

    /// Start the daemon and wait until it answers.
    pub async fn start(&self, paths: &Paths) -> Result<()> {
        match self {
            Self::Detached => spawn_daemon(paths).await,
            #[cfg(target_os = "macos")]
            Self::Embedded(daemon) => daemon.start(paths).await,
        }
    }

    /// Stop the daemon this process hosts. A detached daemon, or one another
    /// process started, keeps running.
    pub async fn stop(&self, paths: &Paths) -> Result<()> {
        match self {
            Self::Detached => {
                let _ = paths;
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Embedded(daemon) => daemon.stop(paths).await,
        }
    }
}

/// The daemon runtime thread of this process, shared by every clone.
#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct EmbeddedDaemon {
    backend: Arc<dyn ModelBackend>,
    /// The runtime thread of the latest start, until it is joined.
    thread: Arc<Mutex<Option<std::thread::JoinHandle<Result<()>>>>>,
}

#[cfg(target_os = "macos")]
impl EmbeddedDaemon {
    async fn start(&self, paths: &Paths) -> Result<()> {
        use anyhow::Context;

        // A restart shuts the previous runtime down first; it has unlinked
        // its socket by now and only needs to return.
        self.join(RESTART_TIMEOUT).await?;
        let cfg = crate::config::Config::load_or_create(paths)?;
        let backend = self.backend.clone();
        let runtime_paths = paths.clone();
        let thread = std::thread::Builder::new()
            .name("limen-runtime".into())
            .spawn(move || {
                super::lifecycle::run_runtime(runtime_paths, cfg, backend, super::Lifetime::Hosted)
            })
            .context("failed to spawn the daemon runtime thread")?;
        *self.thread.lock().unwrap() = Some(thread);
        super::spawn::wait_for_hello(paths, || {
            let exited = self
                .thread
                .lock()
                .unwrap()
                .take_if(|thread| thread.is_finished());
            match exited.map(std::thread::JoinHandle::join) {
                None => Ok(()),
                Some(Ok(Ok(()))) => Err(anyhow::anyhow!("the in-app daemon exited while starting")),
                Some(Ok(Err(error))) => Err(error.context("the in-app daemon failed to start")),
                Some(Err(_)) => Err(anyhow::anyhow!("the in-app daemon panicked while starting")),
            }
        })
        .await
    }

    async fn stop(&self, paths: &Paths) -> Result<()> {
        // A start the bridge abandoned may still be binding its socket, so
        // ask until the runtime thread is gone. Only a socket this process
        // serves is ours to shut down: a thread that lost the bind to
        // another process's daemon returns on its own.
        let shut_down = async {
            let mut asked = false;
            while self.running() {
                match asked {
                    true => {}
                    false => {
                        asked = match super::spawn::daemon_peer_pid(paths).await {
                            Ok(pid) if pid == std::process::id() => {
                                let shutdown =
                                    crate::ipc::request(paths, &crate::ipc::v1::Request::Shutdown);
                                matches!(
                                    tokio::time::timeout(REQUEST_TIMEOUT, shutdown).await,
                                    Ok(Ok(crate::ipc::v1::Reply::Ok))
                                )
                            }
                            Ok(_) | Err(_) => false,
                        };
                    }
                }
                tokio::time::sleep(SHUTDOWN_RETRY).await;
            }
        };
        tokio::time::timeout(STOP_TIMEOUT, shut_down)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the in-app daemon did not stop within {}s",
                    STOP_TIMEOUT.as_secs()
                )
            })?;
        self.join(STOP_TIMEOUT).await
    }

    /// Whether the runtime thread of the latest start is still running.
    fn running(&self) -> bool {
        self.thread
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    /// Wait for the runtime thread to return, then join it. Only running
    /// out of time is an error; how the daemon ended is logged.
    async fn join(&self, limit: Duration) -> Result<()> {
        tokio::time::timeout(limit, async {
            while self.running() {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!("the in-app daemon did not stop within {}s", limit.as_secs())
        })?;
        let finished = self.thread.lock().unwrap().take();
        match finished.map(std::thread::JoinHandle::join) {
            None | Some(Ok(Ok(()))) => {}
            Some(Ok(Err(error))) => {
                tracing::warn!(error = %format!("{error:#}"), "in-app daemon ended with an error");
            }
            Some(Err(_)) => tracing::warn!("in-app daemon runtime thread panicked"),
        }
        Ok(())
    }
}
