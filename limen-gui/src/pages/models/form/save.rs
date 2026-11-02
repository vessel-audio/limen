//! Turning one provider page's fields into a save request, or into the
//! server its models are listed from.

use anyhow::{Result, bail};
use gpui_kit::*;
use limen_core::{
    ApiKey, KeyChange, ModelRole, Provider, ProviderEndpoint, provider_server_url,
    provider_server_url_with_route, split_provider_url,
};

use super::{Address, ProviderForm};
use crate::app::AppView;
use crate::bridge::BridgeCommand;

/// The provider and key change the page's fields describe. The ASR server
/// is checked here, so its error shows at once; the core validates the
/// rest. A vendor's own API keeps the saved URL, which it does not use.
pub(super) fn fields(form: &ProviderForm, cx: &App) -> Result<(Provider, KeyChange)> {
    let Some((loaded, _)) = &form.loaded else {
        bail!("the settings have not loaded yet");
    };
    let model = form.chosen.clone().map(String::from).unwrap_or_default();
    let service = form.chosen_service;
    let provider = match (&form.address, loaded) {
        (
            Address::Server { server },
            Provider::Asr {
                service: saved_service,
                url: saved,
                ..
            },
        ) => {
            let server = server.read(cx).value().trim().to_string();
            let unchanged = split_provider_url(saved).is_some_and(|(origin, _)| origin == server);
            // A hand-edited route stays with its service, even when only
            // the server changes; another service takes its own route.
            let kept = form
                .unlisted_route
                .as_ref()
                .filter(|_| service == *saved_service);
            let url = match (ProviderEndpoint::of(service), kept, unchanged) {
                (None, _, _) | (Some(_), Some(_), true) => saved.clone(),
                (Some(_), Some(route), false) => provider_server_url_with_route(&server, route)?,
                (Some(endpoint), None, _) => provider_server_url(&server, endpoint)?,
            };
            Provider::Asr {
                service,
                url,
                model,
            }
        }
        (Address::Url { polishing, url }, Provider::Polish { .. }) => Provider::Polish {
            polishing: *polishing,
            service,
            url: url.read(cx).value().to_string(),
            model,
        },
        (Address::Server { .. }, Provider::Polish { .. })
        | (Address::Url { .. }, Provider::Asr { .. }) => {
            unreachable!("each page loads its own provider")
        }
    };
    let typed = form.key.read(cx).value().trim().to_string();
    let key = match (typed.is_empty(), form.clear_key) {
        (false, _) => KeyChange::Set(ApiKey::new(typed)),
        (true, true) => KeyChange::Clear,
        (true, false) => KeyChange::Keep,
    };
    Ok((provider, key))
}

/// Save the page for `role`.
pub fn save(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = view.models.form(role);
    if form.loaded.is_none() || !form.touched || form.saving.is_some() {
        return;
    }
    let (provider, key) = match fields(form, cx) {
        Ok(fields) => fields,
        Err(error) => {
            form.error = Some(format!("{error:#}"));
            return;
        }
    };
    form.next_request += 1;
    let id = form.next_request;
    match view
        .commands
        .try_send(BridgeCommand::SaveProvider { id, provider, key })
    {
        Ok(()) => {
            form.saving = Some(id);
            // Only edits after this point are unsaved when the reply arrives.
            form.touched = false;
            form.dirty = Default::default();
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}
