//! Asking the server a page names for its models while the page is
//! edited or when the user asks, offering them in the model select,
//! choosing the only one a server offers, and trying the chosen model.

use std::time::{Duration, Instant};

use gpui_kit::*;
use limen_core::{ModelRole, Provider};
use tokio::sync::mpsc;

use super::{Address, EditedField, ModelForms, ProviderForm, save};
use crate::app::AppView;
use crate::bridge::{BridgeCommand, ModelRequest};
use crate::model::{Action, Toast};
use crate::pages::models::choices::ModelChoices;

/// How long an endpoint that could not list models waits before it is asked
/// again while its page is open.
const RETRY: Duration = Duration::from_secs(15);

/// How long the server's address, key, or service must stay unedited before
/// the server is asked for its models, so a half-typed address is not.
const SETTLE: Duration = Duration::from_millis(600);

/// What the GUI knows of the models the fields' server offers. Each state
/// names the fields it is about; a request in flight also has the ID its
/// reply carries, so a reply to any other request, even one asked of the
/// same fields, is dropped. Any edit to the service, address, or key
/// returns to [`Listing::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Listing {
    /// Not asked since the fields last changed.
    Unknown,
    Asked(String, u64),
    Listed(String, Vec<SharedString>),
    /// The endpoint failed or listed no models; asked again after [`RETRY`].
    Failed(String, Instant),
}

impl ModelForms {
    /// Record the models listed in reply to `request`, if it is still the
    /// one in flight. Failures and empty lists are retried while the page
    /// stays open.
    pub fn listed(&mut self, role: ModelRole, request: u64, models: Result<Vec<String>, String>) {
        let form = self.form(role);
        if let Listing::Asked(endpoint, asked) = &form.listing
            && *asked == request
        {
            let endpoint = endpoint.clone();
            if form.detecting {
                form.detecting = false;
                form.error = match &models {
                    Ok(models) if models.is_empty() => {
                        Some(rust_i18n::t!("models.no_models").to_string())
                    }
                    Ok(_) => None,
                    Err(message) => Some(message.clone()),
                };
            }
            // A server that offers exactly one model has decided it, unless
            // the user or the saved settings chose one.
            if let Ok([only]) = models.as_deref()
                && (form.chosen.is_none() || form.detected)
                && form.chosen.as_deref() != Some(only.as_str())
            {
                form.chosen = Some(SharedString::from(only.clone()));
                form.detected = true;
                form.edited(EditedField::Model);
            }
            form.listing = match models {
                Ok(models) if !models.is_empty() => Listing::Listed(
                    endpoint,
                    models.into_iter().map(SharedString::from).collect(),
                ),
                Ok(_) => Listing::Failed(endpoint, Instant::now()),
                Err(_) => {
                    // Errors are deliberately not logged: endpoints can have
                    // sensitive hand-edited queries even though display_url
                    // already keeps those out of the core's messages.
                    tracing::debug!(?role, "the model server listed no models");
                    Listing::Failed(endpoint, Instant::now())
                }
            };
        }
    }
}

impl ModelForms {
    /// Record how the model check `id` ended, if it is the one in flight.
    pub fn checked(&mut self, role: ModelRole, id: u64, result: Result<(), String>) -> Vec<Action> {
        let form = self.form(role);
        if form.checking != Some(id) {
            return Vec::new();
        }
        form.checking = None;
        match result {
            Ok(()) => vec![Action::Toast(Toast::ModelWorks(role))],
            Err(message) => {
                form.error = Some(message);
                Vec::new()
            }
        }
    }
}

impl ProviderForm {
    /// The server is being asked for its models.
    pub fn detecting(&self) -> bool {
        matches!(self.listing, Listing::Asked(..))
    }

    /// A model is chosen and no check of one is in flight.
    pub fn can_check(&self) -> bool {
        self.loaded.is_some() && self.chosen.is_some() && self.checking.is_none()
    }
}

/// The page's fields, or why they name no server to ask.
fn named(form: &ProviderForm, cx: &App) -> Result<(Provider, limen_core::KeyChange), String> {
    let address = match &form.address {
        Address::Server { server } => server.read(cx).value(),
        Address::Url { url, .. } => url.read(cx).value(),
    };
    match (form.chosen_service.uses_url(), address.trim().is_empty()) {
        (true, true) => Err(rust_i18n::t!("models.enter_server").to_string()),
        (true, false) | (false, _) => save::fields(form, cx).map_err(|error| format!("{error:#}")),
    }
}

/// Ask the server the fields name for its models now, without waiting for
/// them to settle or for a failed list to be retried.
pub fn detect(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = view.models.form(role);
    if form.loaded.is_none() {
        return;
    }
    match named(form, cx) {
        Ok(_) => {
            form.listing = Listing::Unknown;
            form.edited_at = None;
            form.detecting = true;
            form.error = None;
        }
        Err(message) => form.error = Some(message),
    }
}

/// Ask the chosen model to answer one request with the fields as they are.
pub fn check(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = view.models.form(role);
    if !form.can_check() {
        return;
    }
    let (provider, key) = match named(form, cx) {
        Ok(fields) => fields,
        Err(message) => {
            form.error = Some(message);
            return;
        }
    };
    form.next_request += 1;
    let id = form.next_request;
    match view
        .commands
        .try_send(BridgeCommand::Model(ModelRequest::CheckModel {
            role,
            id,
            provider,
            key,
        })) {
        Ok(()) => {
            form.checking = Some(id);
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}

/// Ask the server the fields name for its models once they settle, and
/// have the model select offer the chosen model and the listed ones, where
/// they changed and the select is closed.
pub(super) fn discover(
    form: &mut ProviderForm,
    role: ModelRole,
    visible: bool,
    commands: &mpsc::Sender<BridgeCommand>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    // The fields as they are name what the list is about: a server
    // without an address, or one that does not parse, has no models.
    let query = form
        .filled
        .then(|| save::fields(form, cx).ok())
        .flatten()
        .filter(|(provider, _)| match provider {
            Provider::Asr { service, url, .. } | Provider::Polish { service, url, .. } => {
                !service.uses_url() || !url.trim().is_empty()
            }
        })
        .map(|(provider, key)| {
            let endpoint = match &provider {
                Provider::Asr { service, url, .. } | Provider::Polish { service, url, .. } => {
                    format!("{} {} {}", service.as_str(), url.trim(), form.key_edits)
                }
            };
            (endpoint, provider, key)
        });
    if query.is_none() {
        form.listing = Listing::Unknown;
    }
    let settled = form.edited_at.is_none_or(|at| at.elapsed() >= SETTLE);
    let endpoint = query.as_ref().map(|(endpoint, ..)| endpoint.clone());
    let ask = settled
        && match (&form.listing, endpoint.as_deref()) {
            (_, None) => false,
            (Listing::Unknown, Some(_)) => true,
            (
                Listing::Asked(asked, _) | Listing::Listed(asked, _) | Listing::Failed(asked, _),
                Some(endpoint),
            ) if asked != endpoint => true,
            (Listing::Failed(_, at), Some(_)) => at.elapsed() >= RETRY,
            (Listing::Asked(..) | Listing::Listed(..), Some(_)) => false,
        };
    if visible
        && ask
        && let Some((endpoint, provider, key)) = query
    {
        // Load, save, and list requests share one increasing ID.
        form.next_request += 1;
        let request = form.next_request;
        if commands
            .try_send(BridgeCommand::Model(ModelRequest::ListModels {
                role,
                request,
                provider,
                key,
            }))
            .is_ok()
        {
            form.listing = Listing::Asked(endpoint, request);
        }
    }
    let listed = match &form.listing {
        Listing::Listed(_, models) => models.as_slice(),
        Listing::Unknown | Listing::Asked(..) | Listing::Failed(..) => &[],
    };
    let offered: Vec<SharedString> = form
        .chosen
        .iter()
        .chain(
            listed
                .iter()
                .filter(|model| Some(*model) != form.chosen.as_ref()),
        )
        .cloned()
        .collect();
    let open = form.model.read(cx).focus_handle(cx) != form.closed_focus;
    if !open && offered != form.offered {
        let chosen = form.chosen.clone();
        form.model.update(cx, |select, cx| {
            select.set_items(ModelChoices::new(offered.clone()), window, cx);
            match &chosen {
                Some(model) => select.set_selected_value(model, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
        form.offered = offered;
    }
}
