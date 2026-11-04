//! The Network page's requests: reading and saving the proxy.

use limen_core::{KeyChange, Network, load_network, save_network};
use tokio::sync::mpsc;

use super::{BridgeEvent, Env};

/// A request from the Network page; `id` comes back in the reply so an
/// older one cannot replace a newer one.
#[derive(Debug, Clone)]
pub enum NetworkRequest {
    /// Read the proxy settings from the config file.
    Load { id: u64 },
    /// Save the proxy settings and its password, and apply them in a
    /// running daemon.
    Save {
        id: u64,
        network: Network,
        password: KeyChange,
    },
}

/// Start `request` off the bridge loop; its reply arrives on `events`.
pub(super) fn handle(env: &Env, events: &mpsc::Sender<BridgeEvent>, request: NetworkRequest) {
    let (paths, events) = (env.paths.clone(), events.clone());
    match request {
        NetworkRequest::Load { id } => {
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || load_network(&paths))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = events.send(BridgeEvent::NetworkLoaded { id, result }).await;
            });
        }
        NetworkRequest::Save {
            id,
            network,
            password,
        } => {
            tokio::spawn(async move {
                let result = save_network(&paths, network, password)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = events.send(BridgeEvent::NetworkSaved { id, result }).await;
            });
        }
    }
}
