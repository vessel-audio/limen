//! Which service transcribes and which polishes, and for a custom server
//! the API it speaks, as the config names them.

use serde::{Deserialize, Serialize};

use crate::backend::{Api, Subscription};

/// A model service a role can use: a server the user names, a vendor's own
/// API, which needs only a key, or a subscription the user signs in to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    /// A server at the configured URL, speaking the configured [`Format`].
    Custom,
    /// A ChatGPT plan, signed in to rather than paid per request.
    #[serde(rename = "chatgpt")]
    ChatGpt,
    /// A GitHub Copilot plan, signed in to with GitHub.
    Copilot,
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
    Together,
    Fireworks,
    Cohere,
    Kimi,
    Zai,
    Qwen,
    MiniMax,
    Nebius,
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
    pub const POLISHING: [Self; 19] = [
        Self::Custom,
        Self::ChatGpt,
        Self::Copilot,
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::DeepSeek,
        Self::Mistral,
        Self::Groq,
        Self::OpenRouter,
        Self::XAi,
        Self::Together,
        Self::Fireworks,
        Self::Cohere,
        Self::Kimi,
        Self::Zai,
        Self::Qwen,
        Self::MiniMax,
        Self::Nebius,
    ];

    /// The name the config uses, such as `custom`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::ChatGpt => "chatgpt",
            Self::Copilot => "copilot",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::DeepSeek => "deepseek",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
            Self::OpenRouter => "openrouter",
            Self::XAi => "xai",
            Self::Together => "together",
            Self::Fireworks => "fireworks",
            Self::Cohere => "cohere",
            Self::Kimi => "kimi",
            Self::Zai => "zai",
            Self::Qwen => "qwen",
            Self::MiniMax => "minimax",
            Self::Nebius => "nebius",
        }
    }

    /// Whether requests go to the configured URL: only a custom server's
    /// do; a vendor's own API ignores it.
    pub fn uses_url(self) -> bool {
        self == Self::Custom
    }

    /// The subscription a request with this service is signed in with;
    /// `None` for a server or a vendor, which a key authorizes.
    pub fn subscription(self) -> Option<Subscription> {
        match self {
            Self::ChatGpt => Some(Subscription::ChatGpt),
            Self::Copilot => Some(Subscription::Copilot),
            Self::Custom
            | Self::OpenAi
            | Self::Anthropic
            | Self::Gemini
            | Self::DeepSeek
            | Self::Mistral
            | Self::Groq
            | Self::OpenRouter
            | Self::XAi
            | Self::Together
            | Self::Fireworks
            | Self::Cohere
            | Self::Kimi
            | Self::Zai
            | Self::Qwen
            | Self::MiniMax
            | Self::Nebius => None,
        }
    }

    /// Whether a request needs an API key: a vendor's does; a custom
    /// server may run without one, and a subscription is signed in to.
    pub fn needs_key(self) -> bool {
        self.key_env().is_some()
    }

    /// The environment variable that holds the vendor's key when the config
    /// names none. A custom server may need no key, and a subscription
    /// takes none.
    pub fn key_env(self) -> Option<&'static str> {
        match self {
            Self::Custom | Self::ChatGpt | Self::Copilot => None,
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::DeepSeek => Some("DEEPSEEK_API_KEY"),
            Self::Mistral => Some("MISTRAL_API_KEY"),
            Self::Groq => Some("GROQ_API_KEY"),
            Self::OpenRouter => Some("OPENROUTER_API_KEY"),
            Self::XAi => Some("XAI_API_KEY"),
            Self::Together => Some("TOGETHER_API_KEY"),
            Self::Fireworks => Some("FIREWORKS_API_KEY"),
            Self::Cohere => Some("COHERE_API_KEY"),
            Self::Kimi => Some("MOONSHOT_API_KEY"),
            Self::Zai => Some("ZAI_API_KEY"),
            Self::Qwen => Some("DASHSCOPE_API_KEY"),
            Self::MiniMax => Some("MINIMAX_API_KEY"),
            Self::Nebius => Some("NEBIUS_API_KEY"),
        }
    }

    /// The base URL a vendor's API is reached at unless its settings name
    /// another; `None` for a custom server, which has only its own, and a
    /// subscription, which its sign-in names.
    pub fn default_url(self) -> Option<&'static str> {
        match self {
            Self::Custom | Self::ChatGpt | Self::Copilot => None,
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::Gemini => Some("https://generativelanguage.googleapis.com"),
            Self::DeepSeek => Some("https://api.deepseek.com"),
            Self::Mistral => Some("https://api.mistral.ai"),
            Self::Groq => Some("https://api.groq.com/openai/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::XAi => Some("https://api.x.ai"),
            Self::Together => Some("https://api.together.xyz/v1"),
            Self::Fireworks => Some("https://api.fireworks.ai/inference/v1"),
            Self::Cohere => Some("https://api.cohere.com/v1"),
            Self::Kimi => Some("https://api.moonshot.ai/v1"),
            Self::Zai => Some("https://api.z.ai/api/paas/v4"),
            Self::Qwen => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
            Self::MiniMax => Some("https://api.minimax.io/anthropic/v1"),
            Self::Nebius => Some("https://api.studio.nebius.ai/v1"),
        }
    }

    /// The chat API polishing with this service uses unless its settings
    /// name one, for a custom server the one `format` offers; `None` where
    /// the API has only its own.
    pub fn chat_api(self, format: Format) -> Option<super::ChatApi> {
        match self {
            Self::Custom => format.api(),
            vendor => vendor.vendor_api(),
        }
        .and_then(Api::chat_api)
    }

    /// The vendor's own API; `None` for a custom server, whose
    /// [`Format`] decides.
    pub(crate) fn vendor_api(self) -> Option<Api> {
        match self {
            Self::Custom => None,
            Self::ChatGpt => Some(Api::ChatGpt),
            Self::Copilot => Some(Api::Copilot),
            Self::OpenAi => Some(Api::OpenAi),
            Self::Anthropic => Some(Api::Anthropic),
            Self::Gemini => Some(Api::Gemini),
            Self::DeepSeek => Some(Api::DeepSeek),
            Self::Mistral => Some(Api::Mistral),
            Self::Groq => Some(Api::Groq),
            Self::OpenRouter => Some(Api::OpenRouter),
            Self::XAi => Some(Api::XAi),
            Self::Together => Some(Api::Together),
            Self::Fireworks => Some(Api::Fireworks),
            Self::Cohere => Some(Api::Cohere),
            Self::Kimi => Some(Api::Kimi),
            Self::Zai => Some(Api::Zai),
            Self::Qwen => Some(Api::Qwen),
            Self::MiniMax => Some(Api::MiniMax),
            Self::Nebius => Some(Api::Nebius),
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
