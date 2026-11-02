//! State of the model provider pages: the fields of each provider, filled
//! from the config file when a page opens, and saved together.

use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::*;
use limen_core::{
    ConfigChange, KeyState, ModelRole, Polishing, Provider, ProviderEndpoint, Service,
    split_provider_url,
};

use super::choices::{ModelChoices, ServiceChoice};
use crate::app::{AppView, Page};
use crate::bridge::BridgeCommand;
use crate::model::{Action, Toast};

pub type ServiceSelect = SelectState<SearchableVec<ServiceChoice>>;

/// Fields edited before the first settings load; all of them on later loads.
#[derive(Default, Clone, Copy)]
struct DirtyFields {
    service: bool,
    address: bool,
    key: bool,
    model: bool,
    polishing: bool,
}

/// How long an endpoint that could not list models waits before it is asked
/// again while its page is open.
const RETRY: Duration = Duration::from_secs(15);

/// What the GUI knows of the models the saved endpoint offers. Each state
/// names the endpoint it is about, so a list that arrives for an endpoint
/// saved over since is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Listing {
    /// Not asked since the settings were last loaded.
    Unknown,
    Asked(String),
    Listed(String, Vec<SharedString>),
    /// The endpoint failed or listed no models; asked again after [`RETRY`].
    Failed(String, Instant),
}

/// One provider page. Its fields are filled from the config file each time
/// the page opens, unless the user has edited them, and once more after
/// they are saved.
pub struct ProviderForm {
    pub service: Entity<ServiceSelect>,
    /// The service chosen in the select, which decides the rows shown.
    pub chosen_service: Service,
    pub address: Address,
    pub key: Entity<InputState>,
    pub model: Entity<SelectState<ModelChoices>>,
    /// The model select's focus while closed; it moves into the list while
    /// open, when changing the items would clear what the user types.
    closed_focus: FocusHandle,
    /// The settings as last loaded, and where their key comes from.
    pub loaded: Option<(Provider, KeyState)>,
    /// Settings that arrived and fill the fields on the next frame.
    arrived: Option<(Provider, KeyState)>,
    /// Whether saved settings have filled the fields once, and which fields
    /// the user edited before that first fill.
    filled: bool,
    dirty: DirtyFields,
    /// The settings must be loaded again when the page shows.
    stale: bool,
    /// The page showed on the last frame.
    visible: bool,
    listing: Listing,
    /// The model chosen in the select and the models it offers, as last set.
    chosen: Option<SharedString>,
    offered: Vec<SharedString>,
    /// The saved ASR route when it is not the one its service uses.
    pub unlisted_route: Option<String>,
    /// The saved key is to be removed.
    pub clear_key: bool,
    /// The user edited a field since it was filled or last sent to be saved.
    pub touched: bool,
    /// Identifies load and save requests, so only the latest reply applies.
    next_request: u64,
    loading: Option<u64>,
    /// The save in flight; the fields remain editable meanwhile.
    pub saving: Option<u64>,
    pub error: Option<String>,
}

/// The ASR and polishing pages.
pub struct ModelForms {
    pub asr: ProviderForm,
    pub polish: ProviderForm,
}

impl ModelForms {
    /// Both pages' fields, and the subscriptions that track the user's edits.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let mut subscriptions = Vec::new();
        let mut form = |role: ModelRole, cx: &mut Context<AppView>| {
            let (services, address) = match role {
                ModelRole::Asr => (
                    Service::TRANSCRIBING.to_vec(),
                    Address::Server {
                        server: cx.new(|cx| {
                            InputState::new(window, cx).placeholder("http://127.0.0.1:8000")
                        }),
                    },
                ),
                ModelRole::Polish => (
                    Service::POLISHING.to_vec(),
                    Address::Url {
                        polishing: Polishing::Off,
                        url: cx.new(|cx| {
                            InputState::new(window, cx)
                                .placeholder("http://127.0.0.1:8080/v1/chat/completions")
                        }),
                    },
                ),
            };
            let service = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(
                        services
                            .into_iter()
                            .map(ServiceChoice::new)
                            .collect::<Vec<_>>(),
                    ),
                    None,
                    window,
                    cx,
                )
            });
            let key = cx.new(|cx| InputState::new(window, cx).masked(true));
            let model = cx.new(|cx| {
                SelectState::new(ModelChoices::new(Vec::new()), None, window, cx).searchable(true)
            });
            let inputs = match &address {
                Address::Server { server } => {
                    vec![
                        (server.clone(), EditedField::Address),
                        (key.clone(), EditedField::Key),
                    ]
                }
                Address::Url { url, .. } => {
                    vec![
                        (url.clone(), EditedField::Address),
                        (key.clone(), EditedField::Key),
                    ]
                }
            };
            for (input, field) in inputs {
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    move |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => this.models.form(role).edited(field),
                        InputEvent::PressEnter { .. } => save(this, role, cx),
                        _ => {}
                    },
                ));
            }
            subscriptions.push(cx.subscribe_in(
                &service,
                window,
                move |this, _, event: &SelectEvent<SearchableVec<ServiceChoice>>, _, _| match event
                {
                    SelectEvent::Confirm(Some(service)) => {
                        let form = this.models.form(role);
                        form.chosen_service = *service;
                        form.edited(EditedField::Service);
                    }
                    SelectEvent::Confirm(None) => {}
                },
            ));
            subscriptions.push(cx.subscribe_in(
                &model,
                window,
                move |this, _, event: &SelectEvent<ModelChoices>, _, _| match event {
                    SelectEvent::Confirm(Some(model)) => {
                        let form = this.models.form(role);
                        form.chosen = Some(model.clone());
                        form.edited(EditedField::Model);
                    }
                    SelectEvent::Confirm(None) => {}
                },
            ));
            ProviderForm {
                closed_focus: model.read(cx).focus_handle(cx),
                service,
                chosen_service: Service::OpenAiCompatible,
                address,
                key,
                model,
                loaded: None,
                arrived: None,
                filled: false,
                dirty: DirtyFields::default(),
                stale: true,
                visible: false,
                listing: Listing::Unknown,
                chosen: None,
                offered: Vec::new(),
                unlisted_route: None,
                clear_key: false,
                touched: false,
                next_request: 0,
                loading: None,
                saving: None,
                error: None,
            }
        };
        let asr = form(ModelRole::Asr, cx);
        let polish = form(ModelRole::Polish, cx);
        (Self { asr, polish }, subscriptions)
    }

    pub fn form(&mut self, role: ModelRole) -> &mut ProviderForm {
        match role {
            ModelRole::Asr => &mut self.asr,
            ModelRole::Polish => &mut self.polish,
        }
    }

    /// Load the settings again when their page next shows, as the daemon
    /// that was replaced may have left another config behind. Older reads
    /// are ignored; a save still ends with its own reply before another can
    /// start, so concurrent saves cannot overwrite each other.
    pub fn reset(&mut self) {
        for form in [&mut self.asr, &mut self.polish] {
            form.stale = true;
            form.loading = None;
            form.arrived = None;
            form.error = None;
        }
    }

    /// Record settings read by the current request. Before the first fill,
    /// they complete fields the user has not edited; after it, edits in
    /// progress are left alone and only the saved baseline is updated.
    pub fn loaded(
        &mut self,
        role: ModelRole,
        id: u64,
        result: Result<(Provider, KeyState), String>,
    ) {
        let form = self.form(role);
        if form.loading == Some(id) {
            form.loading = None;
            match (result, form.touched, form.filled) {
                (Ok(settings), true, true) => form.loaded = Some(settings),
                (Ok(settings), _, _) => form.arrived = Some(settings),
                (Err(message), _, _) => form.error = Some(message),
            }
        }
    }

    /// Record the models `endpoint` listed, if it is still the one asked.
    /// Failures and empty lists are retried while the page stays open.
    pub fn listed(
        &mut self,
        role: ModelRole,
        endpoint: String,
        models: Result<Vec<String>, String>,
    ) {
        let form = self.form(role);
        if form.listing == Listing::Asked(endpoint.clone()) {
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

    /// Record how the current save ended. With no edits since it started,
    /// load the saved settings again; otherwise keep those edits for another
    /// save instead of replacing them with the older saved values.
    pub fn saved(
        &mut self,
        role: ModelRole,
        id: u64,
        result: Result<ConfigChange, String>,
    ) -> Vec<Action> {
        let form = self.form(role);
        if form.saving != Some(id) {
            return Vec::new();
        }
        form.saving = None;
        match result {
            Ok(change) => {
                form.error = None;
                if !form.touched {
                    form.clear_key = false;
                    form.stale = true;
                }
                vec![Action::Toast(match change {
                    ConfigChange::Applied => Toast::ProviderApplied(role),
                    ConfigChange::Saved => Toast::ProviderSaved(role),
                })]
            }
            Err(message) => {
                form.touched = true;
                form.error = Some(message);
                Vec::new()
            }
        }
    }
}

impl ProviderForm {
    pub fn edited(&mut self, field: EditedField) {
        self.touched = true;
        match field {
            EditedField::Service => self.dirty.service = true,
            EditedField::Address => self.dirty.address = true,
            EditedField::Key => self.dirty.key = true,
            EditedField::Model => self.dirty.model = true,
            EditedField::Polishing => self.dirty.polishing = true,
        }
        self.error = None;
    }
}

/// Keep both pages in step with the config; runs every frame. A page that
/// opens loads its settings unless they were edited; arrived settings fill
/// the fields; the saved endpoint is asked for its models; and the model
/// select offers the chosen model and the listed ones, where they changed
/// and the select is closed.
pub fn follow(view: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
    for role in [ModelRole::Asr, ModelRole::Polish] {
        let visible = view.page == Page::Models(role);
        let commands = view.commands.clone();
        let form = view.models.form(role);
        if visible && !form.visible && !form.touched {
            form.stale = true;
        }
        form.visible = visible;
        if visible && form.stale && form.saving.is_none() && form.loading.is_none() {
            form.next_request += 1;
            let id = form.next_request;
            if commands
                .try_send(BridgeCommand::LoadProvider { role, id })
                .is_ok()
            {
                form.loading = Some(id);
                form.stale = false;
            }
        }
        match (form.touched, form.filled, form.arrived.take()) {
            (true, true, Some(settings)) => form.loaded = Some(settings),
            (_, _, Some((provider, key))) => {
                fill(form, &provider, &key, window, cx);
                form.loaded = Some((provider, key));
            }
            (_, _, None) => {}
        }
        // The saved service and its URL name what the list is about.
        let endpoint = form.loaded.as_ref().map(|(provider, _)| match provider {
            Provider::Asr { service, url, .. } | Provider::Polish { service, url, .. } => {
                format!("{} {url}", service.as_str())
            }
        });
        let ask = match (&form.listing, endpoint.as_deref()) {
            (_, None) => false,
            (Listing::Unknown, Some(_)) => true,
            (
                Listing::Asked(asked) | Listing::Listed(asked, _) | Listing::Failed(asked, _),
                Some(endpoint),
            ) if asked != endpoint => true,
            (Listing::Failed(_, at), Some(_)) => at.elapsed() >= RETRY,
            (Listing::Asked(_) | Listing::Listed(..), Some(_)) => false,
        };
        if visible
            && ask
            && let Some(endpoint) = endpoint
            && commands
                .try_send(BridgeCommand::ListModels {
                    role,
                    endpoint: endpoint.clone(),
                })
                .is_ok()
        {
            form.listing = Listing::Asked(endpoint);
        }
        let listed = match &form.listing {
            Listing::Listed(_, models) => models.as_slice(),
            Listing::Unknown | Listing::Asked(_) | Listing::Failed(..) => &[],
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
}

/// Fill the fields with loaded settings and ask for the endpoint's models.
/// On the first fill, fields already edited keep the user's values; later
/// fills only happen when no edit is in progress and replace every field.
fn fill(
    form: &mut ProviderForm,
    provider: &Provider,
    key: &KeyState,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    let preserve = !form.filled;
    let dirty = form.dirty;
    let service = match provider {
        Provider::Asr { service, .. } | Provider::Polish { service, .. } => *service,
    };
    if !(preserve && dirty.service) {
        form.chosen_service = service;
        form.service.update(cx, |select, cx| {
            select.set_selected_value(&service, window, cx)
        });
    }
    let model = match (provider, &mut form.address) {
        (Provider::Asr { url, model, .. }, Address::Server { server }) => {
            // A URL without a host shows whole, so the user sees what to repair.
            let (origin, path) =
                split_provider_url(url).unwrap_or_else(|| (url.clone(), String::new()));
            if !(preserve && dirty.address) {
                server.update(cx, |input, cx| input.set_value(origin, window, cx));
            }
            if !(preserve && dirty.service) {
                let route = ProviderEndpoint::of(service).map(ProviderEndpoint::path);
                form.unlisted_route =
                    (route != Some(path.as_str()) && !path.is_empty()).then(|| path.clone());
            }
            model
        }
        (
            Provider::Polish {
                polishing,
                url,
                model,
                ..
            },
            Address::Url {
                polishing: shown,
                url: input,
            },
        ) => {
            if !(preserve && dirty.polishing) {
                *shown = *polishing;
            }
            if !(preserve && dirty.address) {
                input.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
            }
            model
        }
        (Provider::Asr { .. }, Address::Url { .. })
        | (Provider::Polish { .. }, Address::Server { .. }) => {
            unreachable!("each page loads its own provider")
        }
    };
    let placeholder = match key {
        KeyState::Saved => rust_i18n::t!("models.key_saved").to_string(),
        KeyState::Environment(variable) => format!("${variable}"),
        KeyState::Missing => rust_i18n::t!("models.key_missing").to_string(),
    };
    form.key.update(cx, |input, cx| {
        if !(preserve && dirty.key) {
            input.set_value("", window, cx);
        }
        input.set_placeholder(placeholder, window, cx);
    });
    if !(preserve && dirty.model) {
        form.chosen = Some(SharedString::from(model.clone()));
    }
    // Each load may follow a new endpoint or key: ask again.
    form.listing = Listing::Unknown;
    form.filled = true;
    if !preserve {
        form.clear_key = false;
        form.touched = false;
        form.dirty = DirtyFields::default();
    }
}

mod fields;
mod save;

pub use fields::{Address, EditedField};
pub use save::save;
