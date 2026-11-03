//! Which service transcribes and which polishes, and for a custom server
//! the API it speaks, as the config names them.

use serde::{Deserialize, Serialize};

use crate::backend::Api;

/// A model service a role can use: a server the user names, or a vendor's
/// own API, which needs only a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    /// A server at the configured URL, speaking the configured [`Format`].
    Custom,
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    #[serde(rename = "deepseek")]
    DeepSeek,
    Mistral,
    Groq,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "xai")]
    XAi,
}

impl Service {
    /// The services that transcribe, in the order they are offered.
    pub const TRANSCRIBING: [Self; 5] = [
        Self::Custom,
        Self::OpenAi,
        Self::Groq,
        Self::Mistral,
        Self::Gemini,
    ];

    /// The services that polish, in the order they are offered.
    pub const POLISHING: [Self; 9] = [
        Self::Custom,
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::DeepSeek,
        Self::Mistral,
        Self::Groq,
        Self::OpenRouter,
        Self::XAi,
    ];

    /// The name the config uses, such as `custom`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
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

    /// Whether requests go to the configured URL: only a custom server's
    /// do; a vendor's own API ignores it.
    pub fn uses_url(self) -> bool {
        self == Self::Custom
    }

    /// The environment variable that holds the vendor's key when the config
    /// names none. A custom server may need no key.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::Custom => None,
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

    /// The vendor's own API; `None` for a custom server, whose
    /// [`Format`] decides.
    pub(crate) fn vendor_api(self) -> Option<Api> {
        match self {
            Self::Custom => None,
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

/// The API a custom server speaks: the requests it takes and the replies
/// it sends back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// OpenAI's chat-completions and audio-transcriptions routes, as vLLM,
    /// sglang, llama.cpp, and Ollama serve them.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// The whole take uploaded to exactly the configured URL, as the
    /// whisper.cpp server's `/inference` takes it.
    #[serde(rename = "whisper-cpp")]
    WhisperCpp,
    /// OpenAI's own API, Responses route included.
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    #[serde(rename = "deepseek")]
    DeepSeek,
    Mistral,
    Groq,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "xai")]
    XAi,
}

impl Format {
    /// The formats a custom ASR server may speak, in the order offered: the
    /// APIs of the vendors that transcribe, as [`Service::TRANSCRIBING`]
    /// offers them, and the two kinds of server.
    pub const TRANSCRIBING: [Self; 6] = [
        Self::OpenAiCompatible,
        Self::WhisperCpp,
        Self::OpenAi,
        Self::Gemini,
        Self::Mistral,
        Self::Groq,
    ];

    /// The formats a custom polishing server may speak, in the order
    /// offered.
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

    /// The backend API that serves this format; `None` for the upload
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
