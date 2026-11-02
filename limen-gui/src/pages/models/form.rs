//! State of the model provider pages: the fields of each provider, filled
//! from the config file when a page opens, and saved together.

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::*;
use limen_core::{
    ApiKey, ConfigChange, KeyChange, KeyState, ModelRole, Polishing, Provider, ProviderEndpoint,
    provider_server_url, split_provider_url,
};

use super::choices::ModelChoices;
use crate::app::{AppView, Page};
use crate::bridge::BridgeCommand;
use crate::model::{Action, Toast};

pub type EndpointSelect = SelectState<SearchableVec<&'static str>>;

/// Where the provider's requests go.
pub enum Address {
    /// The ASR server's origin and one of the known routes.
    Server {
        server: Entity<InputState>,
        route: Entity<EndpointSelect>,
    },
    /// The polisher's chat-completions URL, and whether it is used at all.
    Url {
        polishing: Polishing,
        url: Entity<InputState>,
    },
}

/// What the GUI knows of the models the saved endpoint offers. Each state
/// names the endpoint it is about, so a list that arrives for an endpoint
/// saved over since is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Listing {
    /// Not asked since the settings were last loaded.
    Unknown,
    Asked(String),
    Listed(String, Vec<SharedString>),
}

/// One provider page. Its fields are filled from the config file each time
/// the page opens, unless the user has edited them, and once more after
/// they are saved.
pub struct ProviderForm {
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
    /// The settings must be loaded again when the page shows.
    stale: bool,
    /// The page showed on the last frame.
    visible: bool,
    listing: Listing,
    /// The model chosen in the select and the models it offers, as last set.
    chosen: Option<SharedString>,
    offered: Vec<SharedString>,
    /// The saved ASR route when the route list does not offer it.
    pub unlisted_route: Option<String>,
    /// The saved key is to be removed.
    pub clear_key: bool,
    /// The user edited a field since it was filled.
    pub touched: bool,
    pub pending: bool,
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
            let address = match role {
                ModelRole::Asr => Address::Server {
                    server: cx
                        .new(|cx| InputState::new(window, cx).placeholder("http://127.0.0.1:8000")),
                    route: cx.new(|cx| {
                        SelectState::new(
                            SearchableVec::new(
                                ProviderEndpoint::ALL.map(ProviderEndpoint::path).to_vec(),
                            ),
                            None,
                            window,
                            cx,
                        )
                    }),
                },
                ModelRole::Polish => Address::Url {
                    polishing: Polishing::Off,
                    url: cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder("https://api.deepseek.com/chat/completions")
                    }),
                },
            };
            let key = cx.new(|cx| InputState::new(window, cx).masked(true));
            let model = cx.new(|cx| {
                SelectState::new(ModelChoices::new(Vec::new()), None, window, cx).searchable(true)
            });
            let inputs = match &address {
                Address::Server { server, .. } => vec![server.clone(), key.clone()],
                Address::Url { url, .. } => vec![url.clone(), key.clone()],
            };
            for input in inputs {
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    move |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => this.models.form(role).edited(),
                        InputEvent::PressEnter { .. } => save(this, role, cx),
                        _ => {}
                    },
                ));
            }
            if let Address::Server { route, .. } = &address {
                subscriptions.push(cx.subscribe_in(
                    route,
                    window,
                    move |this, _, event: &SelectEvent<SearchableVec<&'static str>>, _, _| {
                        match event {
                            SelectEvent::Confirm(_) => this.models.form(role).edited(),
                        }
                    },
                ));
            }
            subscriptions.push(cx.subscribe_in(
                &model,
                window,
                move |this, _, event: &SelectEvent<ModelChoices>, _, _| match event {
                    SelectEvent::Confirm(Some(model)) => {
                        let form = this.models.form(role);
                        form.chosen = Some(model.clone());
                        form.edited();
                    }
                    SelectEvent::Confirm(None) => {}
                },
            ));
            ProviderForm {
                closed_focus: model.read(cx).focus_handle(cx),
                address,
                key,
                model,
                loaded: None,
                arrived: None,
                stale: true,
                visible: false,
                listing: Listing::Unknown,
                chosen: None,
                offered: Vec::new(),
                unlisted_route: None,
                clear_key: false,
                touched: false,
                pending: false,
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
    /// that was replaced may have left another config behind.
    pub fn reset(&mut self) {
        for form in [&mut self.asr, &mut self.polish] {
            form.stale = true;
            form.error = None;
        }
    }

    /// Record settings read from the config file; the fields take them on
    /// the next frame, unless the user edited them meanwhile.
    pub fn loaded(&mut self, role: ModelRole, result: Result<(Provider, KeyState), String>) {
        let form = self.form(role);
        match (result, form.touched) {
            (Ok(settings), false) => form.arrived = Some(settings),
            (Ok(_), true) => {}
            (Err(message), _) => form.error = Some(message),
        }
    }

    /// Record the models `endpoint` listed, if it is still the one asked.
    /// Failures leave the select with the chosen model, which still works.
    pub fn listed(
        &mut self,
        role: ModelRole,
        endpoint: String,
        models: Result<Vec<String>, String>,
    ) {
        let form = self.form(role);
        if form.listing == Listing::Asked(endpoint.clone()) {
            let models = models.unwrap_or_else(|error| {
                tracing::debug!(?role, %error, "no model list");
                Vec::new()
            });
            form.listing = Listing::Listed(
                endpoint,
                models.into_iter().map(SharedString::from).collect(),
            );
        }
    }

    /// Record how saving ended; a saved page loads its settings again, so
    /// the key's state and the model list follow what was written.
    pub fn saved(&mut self, role: ModelRole, result: Result<ConfigChange, String>) -> Vec<Action> {
        let form = self.form(role);
        form.pending = false;
        match result {
            Ok(change) => {
                form.touched = false;
                form.clear_key = false;
                form.stale = true;
                vec![Action::Toast(match change {
                    ConfigChange::Applied => Toast::ProviderApplied(role),
                    ConfigChange::Saved => Toast::ProviderSaved(role),
                })]
            }
            Err(message) => {
                form.error = Some(message);
                Vec::new()
            }
        }
    }
}

impl ProviderForm {
    pub fn edited(&mut self) {
        self.touched = true;
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
        if visible
            && form.stale
            && !form.pending
            && commands.try_send(BridgeCommand::LoadProvider(role)).is_ok()
        {
            form.stale = false;
        }
        if let Some((provider, key)) = form.arrived.take() {
            fill(form, &provider, &key, window, cx);
            form.loaded = Some((provider, key));
        }
        let endpoint = form.loaded.as_ref().map(|(provider, _)| match provider {
            Provider::Asr { url, .. } | Provider::Polish { url, .. } => url.clone(),
        });
        if let (Listing::Unknown, Some(endpoint)) = (&form.listing, endpoint)
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
            Listing::Unknown | Listing::Asked(_) => &[],
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
fn fill(
    form: &mut ProviderForm,
    provider: &Provider,
    key: &KeyState,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    let model = match (provider, &mut form.address) {
        (Provider::Asr { url, model }, Address::Server { server, route }) => {
            // A URL without a host shows whole, so the user sees what to repair.
            let (origin, path) =
                split_provider_url(url).unwrap_or_else(|| (url.clone(), String::new()));
            server.update(cx, |input, cx| input.set_value(origin, window, cx));
            let known = ProviderEndpoint::from_path(&path).map(ProviderEndpoint::path);
            form.unlisted_route = known
                .is_none()
                .then(|| path.clone())
                .filter(|path| !path.is_empty());
            route.update(cx, |select, cx| match known {
                Some(path) => select.set_selected_value(&path, window, cx),
                None => select.set_selected_index(None, window, cx),
            });
            model
        }
        (
            Provider::Polish {
                polishing,
                url,
                model,
            },
            Address::Url {
                polishing: shown,
                url: input,
            },
        ) => {
            *shown = *polishing;
            input.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
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
        input.set_value("", window, cx);
        input.set_placeholder(placeholder, window, cx);
    });
    form.chosen = Some(SharedString::from(model.clone()));
    // Each load may follow a new endpoint or key: ask again.
    form.listing = Listing::Unknown;
    form.clear_key = false;
    form.touched = false;
}

/// Save the page for `role`. The ASR server is checked here, so its error
/// shows at once; the core validates the rest.
pub fn save(view: &mut AppView, role: ModelRole, cx: &mut Context<AppView>) {
    let form = view.models.form(role);
    let Some((loaded, _)) = form.loaded.clone() else {
        return;
    };
    if !form.touched || form.pending {
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
    match view
        .commands
        .try_send(BridgeCommand::SaveProvider { provider, key })
    {
        Ok(()) => {
            form.pending = true;
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}
