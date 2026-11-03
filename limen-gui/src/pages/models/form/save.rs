//! Turning a provider dialog's fields into a save request, or into the
//! server its models are listed from; and saving the polishing switch.

use anyhow::{Context as _, Result, bail};
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use limen_core::{ApiKey, KeyChange, ModelRole, Polishing, Provider, ProviderOptions};
use tokio::sync::mpsc;

use super::ProviderForm;
use crate::app::AppView;
use crate::bridge::{BridgeCommand, ModelRequest};

/// The provider and key change the dialog's fields describe. A vendor's own
/// API keeps the saved custom server's format and URL, which it does not
/// use, and saves its own URL or chat API only where they differ from its
/// defaults; polishing stays as the switch has it; the core validates the
/// rest.
pub(super) fn fields(form: &ProviderForm, cx: &App) -> Result<(Provider, KeyChange)> {
    let Some((loaded, _)) = &form.loaded else {
        bail!("the settings have not loaded yet");
    };
    let service = form.chosen_service;
    let model = form.chosen.clone().map(String::from).unwrap_or_default();
    let typed_url = form.url.read(cx).value().trim().to_string();
    let (format, url, vendor_url) = match (service.uses_url(), loaded) {
        (true, _) => (form.chosen_format, typed_url, None),
        (false, Provider::Asr { format, url, .. } | Provider::Polish { format, url, .. }) => (
            *format,
            url.clone(),
            // Only a vendor with a URL of its own can be sent elsewhere.
            Some(typed_url).filter(|typed| {
                service
                    .default_url()
                    .is_some_and(|own| !typed.is_empty() && typed.trim_end_matches('/') != own)
            }),
        ),
    };
    let chat_api = match loaded {
        Provider::Asr { .. } => None,
        Provider::Polish { .. } => service
            .chat_api(form.chosen_format)
            .filter(|own| *own != form.chosen_chat_api)
            .map(|_| form.chosen_chat_api),
    };
    // The chosen provider's settings as typed; the others' as saved.
    let mut options = match loaded {
        Provider::Asr { options, .. } | Provider::Polish { options, .. } => options.clone(),
    };
    let typed = ProviderOptions {
        url: vendor_url,
        chat_api,
        ..advanced(form, cx)?
    };
    match typed {
        typed if typed == ProviderOptions::default() => options.remove(&service),
        typed => options.insert(service, typed),
    };
    let provider = match loaded {
        Provider::Asr { .. } => Provider::Asr {
            service,
            format,
            url,
            model,
            options,
        },
        Provider::Polish { .. } => Provider::Polish {
            polishing: form.polishing,
            service,
            format,
            url,
            model,
            options,
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

/// The advanced settings as typed. Headers are written as `Name: value`
/// pairs separated by `;`; the core checks what they hold.
fn advanced(form: &ProviderForm, cx: &App) -> Result<ProviderOptions> {
    let text = |input: &Entity<InputState>| {
        Some(input.read(cx).value().trim().to_string()).filter(|text| !text.is_empty())
    };
    let fields = &form.advanced;
    let headers = text(&fields.headers)
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once(':') {
            Some((name, value)) => Ok((name.trim().to_string(), value.trim().to_string())),
            None => bail!("write headers as Name: value, separated by ;"),
        })
        .collect::<Result<_>>()?;
    Ok(ProviderOptions {
        key_header: text(&fields.key_header),
        timeout_ms: text(&fields.timeout)
            .map(|ms| {
                ms.parse()
                    .context("the timeout must be a number of milliseconds")
            })
            .transpose()?,
        retries: text(&fields.retries)
            .map(|retries| retries.parse().context("the retries must be a number"))
            .transpose()?,
        body: text(&fields.body),
        headers,
        ..ProviderOptions::default()
    })
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
/// asks; the switch shows the new state until the save says otherwise. With
/// no provider set up yet, turning it on saves nothing and only shows the
/// providers to choose from.
pub fn set_polishing(view: &mut AppView, polishing: Polishing) {
    let commands = view.commands.clone();
    let form = &mut view.models.polish;
    let Some((
        Provider::Polish {
            polishing: saved,
            service,
            format,
            url,
            model,
            options,
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
    form.error = None;
    // Polishing needs a provider to polish with; until one is saved, the
    // switch only shows the cards, and the dialog's save turns it on.
    let ready = !model.trim().is_empty() && (!service.uses_url() || !url.trim().is_empty());
    form.unsaved_on = match (polishing, ready, saved) {
        (Polishing::On, false, _) => true,
        (Polishing::On, true, _) | (Polishing::Off, _, _) => false,
    };
    match (polishing, ready, saved) {
        (Polishing::On, false, _) | (Polishing::Off, _, Polishing::Off) => return,
        (Polishing::On, true, _) | (Polishing::Off, _, Polishing::On) => {}
    }
    let provider = Provider::Polish {
        polishing,
        service,
        format,
        url,
        model,
        options,
    };
    // A dialog's edits stay unsaved: the switch saves only polishing.
    let touched = form.touched;
    send_with(&commands, form, provider, KeyChange::Keep);
    form.touched = touched;
    form.saved_from = None;
}
