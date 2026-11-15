//! Termination signals end the app the way its Quit does. The daemon a
//! macOS app hosts leaves SIGTERM and SIGINT to its host, so without this
//! `kill` or Ctrl-C would end the process with the daemon's socket still
//! bound on disk.

use anyhow::{Context, Result};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;

/// Exit status for a second signal, after the first one's quit stalled.
const FORCED_EXIT: i32 = 130;

/// Take SIGTERM and SIGINT from the default handler. The receiver resolves
/// on the first one so the app can quit; a second ends the process at once.
pub(crate) fn listen() -> Result<oneshot::Receiver<()>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build the signal runtime")?;
    let (mut terminate, mut interrupt) = {
        let _entered = runtime.enter();
        (
            signal(SignalKind::terminate()).context("failed to watch SIGTERM")?,
            signal(SignalKind::interrupt()).context("failed to watch SIGINT")?,
        )
    };
    let (signalled, received) = oneshot::channel();
    std::thread::Builder::new()
        .name("limen-signals".to_string())
        .spawn(move || {
            runtime.block_on(async {
                let mut next = async || {
                    tokio::select! {
                        _ = terminate.recv() => "SIGTERM",
                        _ = interrupt.recv() => "SIGINT",
                    }
                };
                let first = next().await;
                tracing::info!(signal = first, "quitting on a termination signal");
                let _ = signalled.send(());
                let second = next().await;
                tracing::warn!(signal = second, "quitting at once on a second signal");
                std::process::exit(FORCED_EXIT);
            })
        })
        .context("failed to spawn the signal thread")?;
    Ok(received)
}
