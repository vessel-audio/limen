//! The models a model select offers, and a typed name it takes; and the
//! services a service select offers.

use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::*;
use limen_core::Service;

/// The models a select offers: the current one first, then those the server
/// lists. While searching it shows the ones that match and, last, the typed
/// name when no model is exactly that.
#[derive(Debug, Clone)]
pub struct ModelChoices {
    models: Vec<SharedString>,
    shown: Vec<SharedString>,
}

impl ModelChoices {
    pub fn new(models: Vec<SharedString>) -> Self {
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

/// A service a provider page offers, under the name the user knows it by.
#[derive(Debug, Clone)]
pub struct ServiceChoice {
    service: Service,
    label: SharedString,
}

impl ServiceChoice {
    pub fn new(service: Service) -> Self {
        let label = match service {
            Service::OpenAiCompatible => rust_i18n::t!("models.service_compatible").into(),
            Service::WhisperCpp => rust_i18n::t!("models.service_whisper").into(),
            Service::OpenAi => "OpenAI".into(),
            Service::Anthropic => "Anthropic".into(),
            Service::Gemini => "Gemini".into(),
            Service::DeepSeek => "DeepSeek".into(),
            Service::Mistral => "Mistral".into(),
            Service::Groq => "Groq".into(),
            Service::OpenRouter => "OpenRouter".into(),
            Service::XAi => "xAI".into(),
        };
        Self { service, label }
    }
}

impl SearchableListItem for ServiceChoice {
    type Value = Service;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Service {
        &self.service
    }
}
