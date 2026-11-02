//! The model provider pages' requests: reading and saving a provider,
//! listing the models its server offers, and trying one.

use std::time::Duration;

use limen_core::{
    KeyChange, ModelRole, Provider, check_model, list_models, load_provider, save_provider,
};
use tokio::sync::mpsc;

use super::{BridgeEvent, Env};

/// A request from a model provider page.
#[derive(Debug, Clone)]
pub enum ModelRequest {
    /// Read a provider's settings from the config file; `id` identifies the
    /// request in the reply so an older read cannot replace a newer one.
    LoadProvider { role: ModelRole, id: u64 },
    /// Save a provider and its key, and apply them in a running daemon.
    SaveProvider {
        id: u64,
        provider: Provider,
        key: KeyChange,
    },
    /// Ask the server a page's unsaved fields name which models it
    /// offers; the reply carries `request` back.
    ListModels {
        role: ModelRole,
        request: u64,
        provider: Provider,
        key: KeyChange,
    },
    /// Ask the model a page's unsaved fields name to answer one request;
    /// the reply carries `id` back.
    CheckModel {
        role: ModelRole,
        id: u64,
        provider: Provider,
        key: KeyChange,
    },
}

/// Start `request` off the bridge loop, so snapshots and levels keep flowing
/// meanwhile; its reply arrives on `events`.
pub(super) fn handle(env: &Env, events: &mpsc::Sender<BridgeEvent>, request: ModelRequest) {
    let paths = &env.paths;
    match request {
        ModelRequest::LoadProvider { role, id } => {
            let (paths, events) = (paths.clone(), events.clone());
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || load_provider(&paths, role))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = events
                    .send(BridgeEvent::ProviderLoaded { role, id, result })
                    .await;
            });
        }
        ModelRequest::SaveProvider { id, provider, key } => {
            // Off the loop, so snapshots and levels keep flowing meanwhile.
            let (paths, events) = (paths.clone(), events.clone());
            let role = match provider {
                Provider::Asr { .. } => ModelRole::Asr,
                Provider::Polish { .. } => ModelRole::Polish,
            };
            tokio::spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    save_provider(&paths, provider, key),
                )
                .await
                .map_err(|_| "daemon request timed out".to_string())
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = events
                    .send(BridgeEvent::ProviderSaved { role, id, result })
                    .await;
            });
        }
        ModelRequest::ListModels {
            role,
            request,
            provider,
            key,
        } => {
            let (paths, events) = (paths.clone(), events.clone());
            let backend = env.models.clone();
            tokio::spawn(async move {
                let models = list_models(&paths, &provider, &key, &*backend)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = events
                    .send(BridgeEvent::Models {
                        role,
                        request,
                        models,
                    })
                    .await;
            });
        }
        ModelRequest::CheckModel {
            role,
            id,
            provider,
            key,
        } => {
            let (paths, events) = (paths.clone(), events.clone());
            let backend = env.models.clone();
            tokio::spawn(async move {
                let result = check_model(&paths, &provider, &key, &*backend)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = events
                    .send(BridgeEvent::ModelChecked { role, id, result })
                    .await;
            });
        }
    }
}
