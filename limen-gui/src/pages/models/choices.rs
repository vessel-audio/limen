//! The models a model select offers, and a typed name it takes; the names
//! and logos of the services, and the names of the formats and chat APIs,
//! a provider page offers.

use gpui_kit::assets::IconName;
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::component::{ActiveTheme, Icon, IndexPath};
use gpui_kit::*;
use limen_core::{ChatApi, Format, ModelRole, Service};

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

/// The logo `service`'s card and dialog show: a server icon for a custom
/// server, else its vendor's, in its colors where it has its own and in the
/// text color otherwise.
pub fn logo(service: Service, cx: &App) -> AnyElement {
    let size = px(18.);
    let tinted = |path: &'static str| {
        svg()
            .path(path)
            .flex_shrink_0()
            .size(size)
            .text_color(cx.theme().foreground)
            .into_any_element()
    };
    let colored = |path: &'static str| img(path).flex_shrink_0().size(size).into_any_element();
    match service {
        Service::Custom => Icon::new(IconName::Server)
            .flex_shrink_0()
            .size(size)
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        Service::ChatGpt | Service::OpenAi => tinted("limen/logos/openai.svg"),
        Service::Copilot => tinted("limen/logos/githubcopilot.svg"),
        Service::Anthropic => tinted("limen/logos/anthropic.svg"),
        Service::Gemini => colored("limen/logos/gemini-color.svg"),
        Service::DeepSeek => colored("limen/logos/deepseek-color.svg"),
        Service::Mistral => colored("limen/logos/mistral-color.svg"),
        Service::Groq => tinted("limen/logos/groq.svg"),
        Service::OpenRouter => tinted("limen/logos/openrouter.svg"),
        Service::XAi => tinted("limen/logos/xai.svg"),
    }
}

/// The name the user knows `service` by, as its card shows it.
pub fn service_label(service: Service) -> SharedString {
    match service {
        Service::Custom => rust_i18n::t!("models.custom").into(),
        Service::ChatGpt => "ChatGPT".into(),
        Service::Copilot => "Copilot".into(),
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
        // Mistral's and xAI's routes start with `/v1` themselves.
        (ModelRole::Asr | ModelRole::Polish, Format::Mistral) => "https://api.mistral.ai",
        (ModelRole::Asr | ModelRole::Polish, Format::Groq) => "https://api.groq.com/openai/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::OpenRouter) => "https://openrouter.ai/api/v1",
        (ModelRole::Asr | ModelRole::Polish, Format::XAi) => "https://api.x.ai",
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

/// One of OpenAI's chat APIs, under the name OpenAI gives it.
#[derive(Debug, Clone)]
pub struct ChatApiChoice(ChatApi);

impl ChatApiChoice {
    pub fn new(api: ChatApi) -> Self {
        Self(api)
    }
}

impl SearchableListItem for ChatApiChoice {
    type Value = ChatApi;

    fn title(&self) -> SharedString {
        match self.0 {
            ChatApi::ChatCompletions => "Chat Completions".into(),
            ChatApi::Responses => "Responses".into(),
        }
    }

    fn value(&self) -> &ChatApi {
        &self.0
    }
}
