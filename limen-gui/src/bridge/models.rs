//! The model provider pages' requests: reading and saving a provider,
//! listing the models its server offers, trying one, and signing in to a
//! subscription.

use std::time::Duration;

use limen_core::{
    KeyChange, ModelRole, Provider, Subscription, check_model, list_models, load_provider,
    save_provider,
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
    /// Ask whether a sign-in to `subscription` is kept.
    SignedIn { subscription: Subscription },
    /// Sign in to `subscription`, abandoning any sign-in under way; its
    /// steps and end carry `id` back.
    SignIn { subscription: Subscription, id: u64 },
    /// Abandon the sign-in under way.
    CancelSignIn,
    /// Forget the sign-in to `subscription`; whether one is kept follows.
    SignOut { subscription: Subscription },
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
        ModelRequest::SignedIn { subscription } => {
            let signed_in = env.models.signed_in(subscription);
            let events = events.clone();
            tokio::spawn(async move {
                let _ = events
                    .send(BridgeEvent::SignedIn {
                        subscription,
                        signed_in,
                    })
                    .await;
            });
        }
        ModelRequest::SignIn { subscription, id } => {
            let (backend, events) = (env.models.clone(), events.clone());
            let steps = events.clone();
            let task = tokio::spawn(async move {
                // The prompt runs inside the backend's sign-in, which must
                // not wait on a full channel.
                let prompt = Box::new(move |step| {
                    let _ = steps.try_send(BridgeEvent::SignInStep { id, step });
                });
                let result = backend
                    .sign_in(subscription, prompt)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let signed_in = backend.signed_in(subscription);
                let _ = events.send(BridgeEvent::SignInEnded { id, result }).await;
                let _ = events
                    .send(BridgeEvent::SignedIn {
                        subscription,
                        signed_in,
                    })
                    .await;
            });
            let abandoned = env
                .signing
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replace(task.abort_handle());
            if let Some(abandoned) = abandoned {
                abandoned.abort();
            }
        }
        ModelRequest::CancelSignIn => {
            if let Some(signing) = env
                .signing
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                signing.abort();
            }
        }
        ModelRequest::SignOut { subscription } => {
            let (backend, events) = (env.models.clone(), events.clone());
            let paths = paths.clone();
            tokio::spawn(async move {
                let result = backend.sign_out(subscription).await;
                // A running daemon builds its polisher again, so it drops the
                // credential it may still hold; one not running has none.
                let _ = limen_core::request(&paths, &limen_core::v1::Request::PolishReload).await;
                let signed_in = backend.signed_in(subscription);
                if let Err(error) = result {
                    let _ = events
                        .send(BridgeEvent::Msg(crate::model::Msg::CommandError {
                            message: format!("{error:#}"),
                        }))
                        .await;
                }
                let _ = events
                    .send(BridgeEvent::SignedIn {
                        subscription,
                        signed_in,
                    })
                    .await;
            });
        }
    }
}
