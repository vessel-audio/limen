//! The models a model select offers, and a typed name it takes; the names
//! of the services and formats a provider page offers.

use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::*;
use limen_core::{Format, ModelRole, Service};

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

/// The name the user knows `service` by, as its card shows it.
pub fn service_label(service: Service) -> SharedString {
    match service {
        Service::Custom => rust_i18n::t!("models.custom").into(),
        Service::OpenAi => "OpenAI".into(),
        Service::Anthropic => "Anthropic".into(),
        Service::Gemini => "Gemini".into(),
        Service::DeepSeek => "DeepSeek".into(),
        Service::Mistral => "Mistral".into(),
        Service::Groq => "Groq".into(),
        Service::OpenRouter => "OpenRouter".into(),
        Service::XAi => "xAI".into(),
    }
}

/// An example of the URL a custom server speaking `format` is reached at,
/// shown while the URL is empty.
pub fn url_example(role: ModelRole, format: Format) -> &'static str {
    match (role, format) {
        (ModelRole::Asr, Format::OpenAiCompatible) => "http://127.0.0.1:8000/v1",
        (ModelRole::Polish, Format::OpenAiCompatible) => "http://127.0.0.1:8080/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::WhisperCpp) => {
            "http://127.0.0.1:8080/inference"
        }
        (ModelRole::Asr | ModelRole::Polish, Format::OpenAi) => "https://api.openai.com/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::Anthropic) => "https://api.anthropic.com",
        (ModelRole::Asr | ModelRole::Polish, Format::Gemini) => {
            "https://generativelanguage.googleapis.com"
        }
        (ModelRole::Asr | ModelRole::Polish, Format::DeepSeek) => "https://api.deepseek.com",
        (ModelRole::Asr | ModelRole::Polish, Format::Mistral) => "https://api.mistral.ai/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::Groq) => "https://api.groq.com/openai/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::OpenRouter) => "https://openrouter.ai/api/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::XAi) => "https://api.x.ai/v1",
    }
}

/// A format a custom server may speak, under the name the user knows it by.
#[derive(Debug, Clone)]
pub struct FormatChoice {
    format: Format,
    label: SharedString,
}

impl FormatChoice {
    pub fn new(format: Format) -> Self {
        let label = match format {
            Format::OpenAiCompatible => rust_i18n::t!("models.format_compatible").into(),
            Format::WhisperCpp => "whisper.cpp".into(),
            Format::OpenAi => "OpenAI".into(),
            Format::Anthropic => "Anthropic".into(),
            Format::Gemini => "Gemini".into(),
            Format::DeepSeek => "DeepSeek".into(),
            Format::Mistral => "Mistral".into(),
            Format::Groq => "Groq".into(),
            Format::OpenRouter => "OpenRouter".into(),
            Format::XAi => "xAI".into(),
        };
        Self { format, label }
    }
}

impl SearchableListItem for FormatChoice {
    type Value = Format;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Format {
        &self.format
    }
}
