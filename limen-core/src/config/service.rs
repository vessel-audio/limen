//! Which service transcribes and which polishes, as the config names them.

use serde::{Deserialize, Serialize};

use crate::backend::Api;

/// A model service a role can use: a server Limen is pointed at, or a
/// vendor's own API, which needs only a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Service {
    /// A server with OpenAI-compatible routes at the configured URL.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// The whisper.cpp server, or any server that takes the upload at
    /// exactly the configured URL.
    #[serde(rename = "whisper-cpp")]
    WhisperCpp,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "mistral")]
    Mistral,
    #[serde(rename = "groq")]
    Groq,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "xai")]
    XAi,
}

impl Service {
    /// The services that transcribe, in the order they are offered.
    pub const TRANSCRIBING: [Self; 6] = [
        Self::OpenAiCompatible,
        Self::WhisperCpp,
        Self::OpenAi,
        Self::Groq,
        Self::Mistral,
        Self::Gemini,
    ];

    /// The services that polish, in the order they are offered.
    pub const POLISHING: [Self; 9] = [
        Self::OpenAiCompatible,
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::DeepSeek,
        Self::Mistral,
        Self::Groq,
        Self::OpenRouter,
        Self::XAi,
    ];

    /// The name the config uses, such as `openai-compatible`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::WhisperCpp => "whisper-cpp",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::DeepSeek => "deepseek",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
            Self::OpenRouter => "openrouter",
            Self::XAi => "xai",
        }
    }

    /// Whether requests go to the configured URL; a vendor's own API
    /// ignores it.
    pub fn uses_url(self) -> bool {
        matches!(self, Self::OpenAiCompatible | Self::WhisperCpp)
    }

    /// The environment variable that holds the vendor's key when the config
    /// names none. A server Limen is pointed at may need no key.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::OpenAiCompatible | Self::WhisperCpp => None,
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::DeepSeek => Some("DEEPSEEK_API_KEY"),
            Self::Mistral => Some("MISTRAL_API_KEY"),
            Self::Groq => Some("GROQ_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::XAi => Some("XAI_API_KEY"),
        }
    }

    /// The backend API that serves this service; `None` for the upload
    /// Limen sends itself.
    pub(crate) fn api(self) -> Option<Api> {
        match self {
            Self::OpenAiCompatible => Some(Api::Compatible),
            Self::WhisperCpp => None,
            Self::OpenAi => Some(Api::OpenAi),
            Self::Anthropic => Some(Api::Anthropic),
            Self::Gemini => Some(Api::Gemini),
            Self::DeepSeek => Some(Api::DeepSeek),
            Self::Mistral => Some(Api::Mistral),
            Self::Groq => Some(Api::Groq),
            Self::OpenRouter => Some(Api::OpenRouter),
            Self::XAi => Some(Api::XAi),
        }
    }
}
