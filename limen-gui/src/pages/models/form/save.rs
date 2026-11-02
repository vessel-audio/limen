//! Turning one provider page's fields into a save request.

use gpui_kit::*;
use limen_core::{
    ApiKey, KeyChange, ModelRole, Provider, ProviderEndpoint, provider_server_url,
    split_provider_url,
};

use super::Address;
use crate::app::AppView;
use crate::bridge::BridgeCommand;

/// Save the page for `role`. The ASR server is checked here, so its error
/// shows at once; the core validates the rest.
pub fn save(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = view.models.form(role);
    let Some((loaded, _)) = form.loaded.clone() else {
        return;
    };
    if !form.touched || form.saving.is_some() {
        return;
    }
    let model = form.chosen.clone().map(String::from).unwrap_or_default();
    let provider = match (&form.address, loaded) {
        (Address::Server { server, route }, Provider::Asr { url: saved, .. }) => {
            let server = server.read(cx).value().trim().to_string();
            let endpoint = route
                .read(cx)
                .selected_value()
                .and_then(|path| ProviderEndpoint::from_path(path));
            let unchanged = split_provider_url(&saved).is_some_and(|(origin, _)| origin == server);
            let url = match (endpoint, unchanged) {
                (Some(endpoint), _) => provider_server_url(&server, endpoint),
                // A hand-edited route the list does not offer stays.
                (None, true) => Ok(saved),
                (None, false) => Err(anyhow::anyhow!(
                    rust_i18n::t!("models.choose_route").to_string()
                )),
            };
            match url {
                Ok(url) => Provider::Asr { url, model },
                Err(error) => {
                    form.error = Some(format!("{error:#}"));
                    return;
                }
            }
        }
        (Address::Url { polishing, url }, Provider::Polish { .. }) => Provider::Polish {
            polishing: *polishing,
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
