//! State of the model provider pages: each role's settings as loaded from
//! the config file, and the dialog that edits one service, whose fields
//! are filled when it opens and saved together.

use std::time::Instant;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::*;
use limen_core::{
    ConfigChange, Format, KeySource, KeyState, ModelRole, Polishing, Provider, Service,
};

use super::choices::{FormatChoice, ModelChoices, url_example};
use crate::app::{AppView, Page};
use crate::bridge::{BridgeCommand, ModelRequest};
use crate::model::{Action, Toast};
use discovery::{Check, Listing};

pub type FormatSelect = SelectState<SearchableVec<FormatChoice>>;

/// One provider page: its settings as loaded, and the dialog that edits
/// the service of a card.
pub struct ProviderForm {
    /// The service the dialog edits.
    pub chosen_service: Service,
    /// What a custom server speaks, as chosen in the dialog.
    pub chosen_format: Format,
    pub format: Entity<FormatSelect>,
    pub url: Entity<InputState>,
    pub key: Entity<InputState>,
    pub model: Entity<SelectState<ModelChoices>>,
    /// The model select's focus while closed; it moves into the list while
    /// open, when changing the items would clear what the user types.
    closed_focus: FocusHandle,
    /// The settings as last loaded, and where their key comes from.
    pub loaded: Option<(Provider, KeySource)>,
    /// Settings that arrived and become the loaded ones on the next frame.
    arrived: Option<(Provider, KeySource)>,
    /// Polishing as the page's switch shows it: the saved state, or the one
    /// the switch asked to save.
    pub polishing: Polishing,
    /// The dialog is showing.
    pub open: bool,
    /// A save from the dialog succeeded with no edits since, so it closes.
    close_requested: bool,
    /// The settings must be loaded again when the page shows.
    stale: bool,
    /// The page showed on the last frame.
    visible: bool,
    listing: Listing,
    /// The model chosen in the select and the models it offers, as last set.
    chosen: Option<SharedString>,
    /// The chosen model is the only one a server listed, not the user's
    /// choice or the saved one, so another such server may replace it.
    detected: bool,
    offered: Vec<SharedString>,
    /// The saved key is to be removed.
    pub clear_key: bool,
    /// When the format, address, or key was last edited.
    edited_at: Option<Instant>,
    /// The user asked for the models, so a failure to list them shows.
    detecting: bool,
    /// The model check in flight.
    checking: Check,
    /// Counts key edits, so a list asked with an older key is told apart
    /// without keeping the key itself.
    key_edits: u64,
    /// The dialog holds something to save: an edit, or a service other than
    /// the saved one.
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
    /// Both pages' dialog fields, and the subscriptions that track the
    /// user's edits.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let mut subscriptions = Vec::new();
        let mut form = |role: ModelRole, cx: &mut Context<AppView>| {
            let formats = match role {
                ModelRole::Asr => Format::TRANSCRIBING.to_vec(),
                ModelRole::Polish => Format::POLISHING.to_vec(),
            };
            let format = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(
                        formats
                            .into_iter()
                            .map(FormatChoice::new)
                            .collect::<Vec<_>>(),
                    ),
                    None,
                    window,
                    cx,
                )
            });
            let url = cx.new(|cx| InputState::new(window, cx));
            let key = cx.new(|cx| InputState::new(window, cx).masked(true));
            let model = cx.new(|cx| {
                SelectState::new(ModelChoices::new(Vec::new()), None, window, cx).searchable(true)
            });
            for (input, field) in [
                (url.clone(), EditedField::Address),
                (key.clone(), EditedField::Key),
            ] {
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
                &format,
                window,
                move |this, _, event: &SelectEvent<SearchableVec<FormatChoice>>, window, cx| {
                    match event {
                        SelectEvent::Confirm(Some(format)) => {
                            let form = this.models.form(role);
                            // A model belongs to the API that listed it.
                            if *format != form.chosen_format {
                                form.chosen = None;
                            }
                            form.chosen_format = *format;
                            form.edited(EditedField::Format);
                            form.url.update(cx, |input, cx| {
                                input.set_placeholder(url_example(role, *format), window, cx)
                            });
                        }
                        SelectEvent::Confirm(None) => {}
                    }
                },
            ));
            subscriptions.push(cx.subscribe_in(
                &model,
                window,
                move |this, _, event: &SelectEvent<ModelChoices>, _, _| match event {
                    SelectEvent::Confirm(Some(model)) => {
                        let form = this.models.form(role);
                        form.chosen = Some(model.clone());
                        form.detected = false;
                        form.edited(EditedField::Model);
                    }
                    SelectEvent::Confirm(None) => {}
                },
            ));
            ProviderForm {
                closed_focus: model.read(cx).focus_handle(cx),
                chosen_service: Service::Custom,
                chosen_format: Format::OpenAiCompatible,
                format,
                url,
                key,
                model,
                loaded: None,
                arrived: None,
                polishing: Polishing::Off,
                open: false,
                close_requested: false,
                stale: true,
                visible: false,
                listing: Listing::Unknown,
                detected: false,
                chosen: None,
                offered: Vec::new(),
                clear_key: false,
                edited_at: None,
                detecting: false,
                checking: Check::Idle,
                key_edits: 0,
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
            form.checking = form.checking.superseded();
        }
    }

    /// Record settings read by the current request; they become the loaded
    /// ones on the next frame. A dialog that is open keeps its fields.
    pub fn loaded(
        &mut self,
        role: ModelRole,
        id: u64,
        result: Result<(Provider, KeySource), String>,
    ) {
        let form = self.form(role);
        if form.loading == Some(id) {
            form.loading = None;
            match result {
                Ok(settings) => form.arrived = Some(settings),
                Err(message) => form.error = Some(message),
            }
        }
    }

    /// Record how the current save ended. A dialog with no edits since it
    /// saved closes, and the saved settings load again; one with edits
    /// stays open with them, for another save.
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
                    form.close_requested = form.open;
                }
                vec![Action::Toast(match change {
                    ConfigChange::Applied => Toast::ProviderApplied(role),
                    ConfigChange::Saved => Toast::ProviderSaved(role),
                })]
            }
            Err(message) => {
                // The switch shows what is saved again.
                if let Some((Provider::Polish { polishing, .. }, _)) = &form.loaded {
                    form.polishing = *polishing;
                }
                form.touched = form.open;
                form.error = Some(message);
                Vec::new()
            }
        }
    }
}

impl ProviderForm {
    pub fn edited(&mut self, field: EditedField) {
        self.touched = true;
        // A model check in flight tried other fields: drop its reply.
        self.checking = self.checking.superseded();
        // What the server was asked no longer describes the fields: drop
        // its reply when it comes.
        let restart = |form: &mut Self| {
            form.edited_at = Some(Instant::now());
            form.listing = Listing::Unknown;
            form.detecting = false;
        };
        match field {
            EditedField::Format | EditedField::Address => restart(self),
            EditedField::Key => {
                self.key_edits += 1;
                restart(self);
            }
            EditedField::Model => {}
        }
        self.error = None;
    }

    /// The saved service, once the settings have loaded.
    pub fn active(&self) -> Option<Service> {
        self.loaded.as_ref().map(|(provider, _)| match provider {
            Provider::Asr { service, .. } | Provider::Polish { service, .. } => *service,
        })
    }
}

/// Keep both pages in step with the config; runs every frame. A page that
/// opens loads its settings; arrived settings become the loaded ones; a
/// dialog that saved closes; and an open dialog's server is asked for its
/// models.
pub fn follow(view: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
    for role in [ModelRole::Asr, ModelRole::Polish] {
        let visible = view.page == Page::Models(role);
        let commands = view.commands.clone();
        let form = view.models.form(role);
        if visible && !form.visible && !form.open {
            form.stale = true;
        }
        form.visible = visible;
        if visible && form.stale && form.saving.is_none() && form.loading.is_none() {
            form.next_request += 1;
            let id = form.next_request;
            if commands
                .try_send(BridgeCommand::Model(ModelRequest::LoadProvider {
                    role,
                    id,
                }))
                .is_ok()
            {
                form.loading = Some(id);
                form.stale = false;
            }
        }
        if let Some(settings) = form.arrived.take() {
            if let (Provider::Polish { polishing, .. }, None) = (&settings.0, form.saving) {
                form.polishing = *polishing;
            }
            form.loaded = Some(settings);
        }
        // Escape, the close button, and the backdrop close the dialog too.
        match (
            form.close_requested,
            form.open && !window.has_active_dialog(cx),
        ) {
            (true, _) => {
                window.close_dialog(cx);
                form.open = false;
                form.close_requested = false;
            }
            (false, true) => form.open = false,
            (false, false) => {}
        }
        discovery::discover(form, role, visible && form.open, &commands, window, cx);
    }
}

/// Fill the dialog for `service` from the loaded settings: a custom
/// server's format and URL as saved, the saved model if `service` is the
/// saved one, and an empty key showing where the key would come from.
pub fn fill(
    form: &mut ProviderForm,
    role: ModelRole,
    service: Service,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((provider, key)) = form.loaded.clone() else {
        return;
    };
    let (saved, format, url, model) = match &provider {
        Provider::Asr {
            service,
            format,
            url,
            model,
        }
        | Provider::Polish {
            service,
            format,
            url,
            model,
            ..
        } => (*service, *format, url.clone(), model.clone()),
    };
    form.chosen_service = service;
    form.chosen_format = format;
    form.format.update(cx, |select, cx| {
        select.set_selected_value(&format, window, cx)
    });
    form.url.update(cx, |input, cx| {
        input.set_value(url, window, cx);
        input.set_placeholder(url_example(role, format), window, cx);
    });
    let placeholder = key_placeholder(&key, service);
    form.key.update(cx, |input, cx| {
        input.set_value("", window, cx);
        input.set_placeholder(placeholder, window, cx);
    });
    // The saved model belongs to the saved service; an empty one is none.
    form.chosen = (service == saved && !model.trim().is_empty()).then(|| SharedString::from(model));
    form.detected = false;
    form.listing = Listing::Unknown;
    form.edited_at = None;
    form.detecting = false;
    form.clear_key = false;
    form.error = None;
    // Choosing another service is itself something to save.
    form.touched = service != saved;
    form.open = true;
    form.close_requested = false;
}

/// What the key field shows while empty, once `service` is chosen.
pub fn key_placeholder(key: &KeySource, service: Service) -> String {
    match key.state(service) {
        KeyState::Saved => rust_i18n::t!("models.key_saved").to_string(),
        KeyState::Environment(variable) => format!("${variable}"),
        KeyState::Missing => rust_i18n::t!("models.key_missing").to_string(),
    }
}

mod discovery;
mod fields;
mod save;

pub use discovery::{check, detect};
pub use fields::EditedField;
pub use save::{save, set_polishing};
