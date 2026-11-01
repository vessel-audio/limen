//! The ASR and polishing models: one row each in the dictation control
//! strip, a searchable list of the models their servers offer that also
//! takes any typed name. Choosing one applies it at once.

use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt};
use gpui_kit::*;
use limen_core::{ConfigChange, ConfigSummary, ModelRole};

use crate::a11y;
use crate::app::AppView;
use crate::bridge::BridgeCommand;
use crate::model::{Action, Toast};

/// The models a select offers: the current one first, then those the server
/// lists. While searching it shows the ones that match and, last, the typed
/// name when no model is exactly that.
#[derive(Debug, Clone)]
pub struct ModelChoices {
    models: Vec<SharedString>,
    shown: Vec<SharedString>,
}

impl ModelChoices {
    fn new(models: Vec<SharedString>) -> Self {
        Self {
            shown: models.clone(),
            models,
        }
    }
}

impl SearchableListDelegate for ModelChoices {
    type Item = SharedString;

    fn items_count(&self, _: usize) -> usize {
        self.shown.len()
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.shown.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.shown
            .iter()
            .position(|model| model.value() == value)
            .map(|row| IndexPath::default().row(row))
    }

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        let query = query.trim();
        let needle = query.to_lowercase();
        let typed = (!query.is_empty() && !self.models.iter().any(|model| model == query))
            .then(|| SharedString::from(query.to_string()));
        self.shown = self
            .models
            .iter()
            .filter(|model| model.to_lowercase().contains(&needle))
            .cloned()
            .chain(typed)
            .collect();
        Task::ready(())
    }
}

/// State of one model row. The select follows the daemon except while a
/// choice is being applied; a failed choice stays shown until the daemon's
/// model or the server's list changes.
pub struct ModelForm {
    select: Entity<SelectState<ModelChoices>>,
    /// The models the server listed.
    listed: Vec<SharedString>,
    /// The endpoint whose models were last asked for.
    listed_for: Option<String>,
    /// The daemon's model and the models the select offers, as last set.
    followed: (Option<SharedString>, Vec<SharedString>),
    pending: bool,
    error: Option<String>,
}

/// The rows for both models.
pub struct ModelForms {
    asr: ModelForm,
    polish: ModelForm,
}

impl ModelForms {
    /// Both rows' selects, and the subscriptions that apply a choice.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, [Subscription; 2]) {
        let mut form = |role: ModelRole| {
            let select = cx.new(|cx| {
                SelectState::new(ModelChoices::new(Vec::new()), None, window, cx).searchable(true)
            });
            let subscription = cx.subscribe_in(
                &select,
                window,
                move |this, _, event: &SelectEvent<ModelChoices>, _, _| match event {
                    SelectEvent::Confirm(Some(model)) => choose(this, role, model.to_string()),
                    SelectEvent::Confirm(None) => {}
                },
            );
            let form = ModelForm {
                select,
                listed: Vec::new(),
                listed_for: None,
                followed: (None, Vec::new()),
                pending: false,
                error: None,
            };
            (form, subscription)
        };
        let (asr, asr_subscription) = form(ModelRole::Asr);
        let (polish, polish_subscription) = form(ModelRole::Polish);
        (
            Self { asr, polish },
            [asr_subscription, polish_subscription],
        )
    }

    fn form(&mut self, role: ModelRole) -> &mut ModelForm {
        match role {
            ModelRole::Asr => &mut self.asr,
            ModelRole::Polish => &mut self.polish,
        }
    }

    /// Forget the lists and errors of a daemon that was replaced.
    pub fn reset(&mut self) {
        for form in [&mut self.asr, &mut self.polish] {
            form.listed_for = None;
            form.error = None;
        }
    }

    /// Record the models a server listed.
    pub fn listed(&mut self, role: ModelRole, models: Vec<String>) {
        self.form(role).listed = models.into_iter().map(SharedString::from).collect();
    }

    /// Record how applying a choice ended.
    pub fn finish(&mut self, role: ModelRole, result: Result<ConfigChange, String>) -> Vec<Action> {
        let form = self.form(role);
        form.pending = false;
        match result {
            Ok(change) => {
                form.error = None;
                vec![Action::Toast(match change {
                    ConfigChange::Applied => Toast::ModelApplied(role),
                    ConfigChange::Saved => Toast::ModelSaved(role),
                })]
            }
            Err(message) => {
                form.error = Some(message);
                Vec::new()
            }
        }
    }
}

/// The daemon's model for `role` and the endpoint that lists its models;
/// `None` for values capped with an ellipsis, which are not the real ones.
fn configured(config: &ConfigSummary, role: ModelRole) -> (Option<&str>, Option<&str>) {
    let (model, url) = match role {
        ModelRole::Asr => (&config.provider_model, &config.provider_url),
        ModelRole::Polish => (&config.polish_model, &config.polish_url),
    };
    let [model, url] =
        [model, url].map(|value| Some(value.as_str()).filter(|value| !value.ends_with('…')));
    (model, url)
}

/// Ask for the models of every endpoint that changed since they were last
/// asked for, so the lists are ready before a select opens.
pub fn request_lists(view: &mut AppView) {
    let Some(config) = view.snapshot_config() else {
        return;
    };
    for role in [ModelRole::Asr, ModelRole::Polish] {
        let (_, url) = configured(&config, role);
        let form = view.models.form(role);
        if let Some(url) = url
            && form.listed_for.as_deref() != Some(url)
            && view
                .commands
                .try_send(BridgeCommand::ListModels(role))
                .is_ok()
        {
            form.listed_for = Some(url.to_string());
            form.listed.clear();
        }
    }
}

/// Offer the daemon's model and the listed ones in each select, where they
/// changed: setting the items resets the select, and this runs every frame.
pub fn follow(view: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
    let Some(config) = view.snapshot_config() else {
        return;
    };
    for role in [ModelRole::Asr, ModelRole::Polish] {
        let current = configured(&config, role).0.map(SharedString::from);
        let form = view.models.form(role);
        let offered: Vec<SharedString> = current
            .iter()
            .chain(
                form.listed
                    .iter()
                    .filter(|model| Some(*model) != current.as_ref()),
            )
            .cloned()
            .collect();
        let followed = (current, offered);
        if !form.pending && form.followed != followed {
            form.select.update(cx, |select, cx| {
                select.set_items(ModelChoices::new(followed.1.clone()), window, cx);
                match &followed.0 {
                    Some(model) => select.set_selected_value(model, window, cx),
                    None => select.set_selected_index(None, window, cx),
                }
            });
            form.followed = followed;
        }
    }
}

pub fn models(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let rows = [
        (
            ModelRole::Asr,
            &view.models.asr,
            "models.asr",
            "asr-model-error",
        ),
        (
            ModelRole::Polish,
            &view.models.polish,
            "models.polish",
            "polish-model-error",
        ),
    ];
    let fields = rows.map(|(role, form, label, _)| {
        let label = rust_i18n::t!(label).to_string();
        let caption = div()
            .flex_shrink_0()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label.clone());
        // The first caption lines up with the server's above it.
        let caption = match role {
            ModelRole::Asr => caption.w(super::provider::LABEL_WIDTH),
            ModelRole::Polish => caption,
        };
        div()
            .h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(caption)
            .child(
                div().flex_1().min_w_0().child(
                    Select::new(&form.select)
                        .id(SharedString::from(format!("model-{role:?}")))
                        .small()
                        .accessibility_label(label.clone())
                        .placeholder(label)
                        .search_placeholder(rust_i18n::t!("models.search").to_string())
                        .disabled(form.pending),
                ),
            )
    });
    let errors = rows.map(|(_, form, _, error_id)| {
        form.error.as_ref().map(|error| {
            a11y::alert(error_id, error.clone())
                .text_xs()
                .text_color(cx.theme().danger)
                .child(error.clone())
        })
    });
    div()
        .v_flex()
        .gap_1()
        .child(div().h_flex().items_center().gap_4().children(fields))
        .children(errors.into_iter().flatten())
}

/// Apply the chosen model unless the daemon already uses it.
fn choose(view: &mut AppView, role: ModelRole, model: String) {
    let form = view.models.form(role);
    let unchanged = form.followed.0.as_deref() == Some(model.as_str());
    if let (false, false) = (form.pending, unchanged) {
        match view
            .commands
            .try_send(BridgeCommand::SetModel { role, model })
        {
            Ok(()) => {
                form.pending = true;
                form.error = None;
            }
            Err(error) => form.error = Some(error.to_string()),
        }
    }
}
