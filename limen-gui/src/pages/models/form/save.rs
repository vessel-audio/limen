//! Turning a provider dialog's fields into a save request, or into the
//! server its models are listed from; and saving the polishing switch.

use anyhow::{Result, bail};
use gpui_kit::*;
use limen_core::{ApiKey, KeyChange, ModelRole, Polishing, Provider};
use tokio::sync::mpsc;

use super::ProviderForm;
use crate::app::AppView;
use crate::bridge::{BridgeCommand, ModelRequest};

/// The provider and key change the dialog's fields describe. A vendor's own
/// API keeps the saved custom server's format and URL, which it does not
/// use, and polishing stays as the switch has it; the core validates the
/// rest.
pub(super) fn fields(form: &ProviderForm, cx: &App) -> Result<(Provider, KeyChange)> {
    let Some((loaded, _)) = &form.loaded else {
        bail!("the settings have not loaded yet");
    };
    let service = form.chosen_service;
    let model = form.chosen.clone().map(String::from).unwrap_or_default();
    let (format, url) = match (service.uses_url(), loaded) {
        (true, _) => (
            form.chosen_format,
            form.url.read(cx).value().trim().to_string(),
        ),
        (false, Provider::Asr { format, url, .. } | Provider::Polish { format, url, .. }) => {
            (*format, url.clone())
        }
    };
    let provider = match loaded {
        Provider::Asr { .. } => Provider::Asr {
            service,
            format,
            url,
            model,
        },
        Provider::Polish { .. } => Provider::Polish {
            polishing: form.polishing,
            service,
            format,
            url,
            model,
        },
    };
    let typed = form.key.read(cx).value().trim().to_string();
    let key = match (typed.is_empty(), form.clear_key) {
        (false, _) => KeyChange::Set(ApiKey::new(typed)),
        (true, true) => KeyChange::Clear,
        (true, false) => KeyChange::Keep,
    };
    Ok((provider, key))
}

/// Send `provider` and `key` to be saved for `form`.
fn send_with(
    commands: &mpsc::Sender<BridgeCommand>,
    form: &mut ProviderForm,
    provider: Provider,
    key: KeyChange,
) {
    form.next_request += 1;
    let id = form.next_request;
    match commands.try_send(BridgeCommand::Model(ModelRequest::SaveProvider {
        id,
        provider,
        key,
    })) {
        Ok(()) => {
            form.saving = Some(id);
            // Only edits after this point are unsaved when the reply arrives.
            form.touched = false;
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}

/// Save the dialog for `role`.
pub fn save(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = match role {
        ModelRole::Asr => &mut view.models.asr,
        ModelRole::Polish => &mut view.models.polish,
    };
    if !form.open || !form.touched || form.saving.is_some() {
        return;
    }
    match fields(form, cx) {
        Ok((provider, key)) => {
            let commands = view.commands.clone();
            send_with(&commands, form, provider, key);
            form.saved_from = Some(form.opening);
        }
        Err(error) => form.error = Some(format!("{error:#}")),
    }
}

/// Turn polishing on or off with the saved provider, as the page's switch
/// asks; the switch shows the new state until the save says otherwise.
pub fn set_polishing(view: &mut AppView, polishing: Polishing) {
    let commands = view.commands.clone();
    let form = &mut view.models.polish;
    let Some((
        Provider::Polish {
            service,
            format,
            url,
            model,
            ..
        },
        _,
    )) = form.loaded.clone()
    else {
        return;
    };
    if form.saving.is_some() || polishing == form.polishing {
        return;
    }
    form.polishing = polishing;
    let provider = Provider::Polish {
        polishing,
        service,
        format,
        url,
        model,
    };
    // A dialog's edits stay unsaved: the switch saves only polishing.
    let touched = form.touched;
    send_with(&commands, form, provider, KeyChange::Keep);
    form.touched = touched;
    form.saved_from = None;
}
