//! The seam between Limen and whatever serves its models.
//!
//! Limen owns the session, buffering, timeout, and polishing policy; a
//! backend only carries one request to a model and its reply back. Nothing
//! here names a model framework, so the framework behind these traits can
//! be replaced without touching the daemon.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;

/// Object-safe, cancellable future used by the provider trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One whole take, ready to transcribe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take {
    /// 16-bit mono PCM in a WAV container.
    pub wav: Vec<u8>,
    /// The spoken language as a code such as `zh`; `None` lets the model
    /// detect it.
    pub language: Option<String>,
}

/// What a transcriber answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    /// The tokens the service reported, for one that bills by token, and
    /// their cost where the backend knows it.
    pub usage: Usage,
}

/// A model that turns one take into its text.
///
/// The daemon builds one per take with the model and key already bound, and
/// calls [`transcribe`](Self::transcribe) once, after recording ends. The
/// caller bounds the call with its own timeout and drops the future when the
/// take is abandoned, so implementations must not keep working after a drop.
pub trait Transcriber: Send + Sync {
    /// The transcript of `take`, or why there is none.
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<Transcript>>;
}

/// One instruction-and-message exchange with a chat model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exchange<'a> {
    /// The system instructions.
    pub instructions: &'a str,
    /// The user message the instructions apply to.
    pub message: &'a str,
    /// The most tokens the reply may take.
    pub max_tokens: u64,
}

/// Why a chat model stopped writing its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The reply is complete.
    Stop,
    /// The reply hit the token limit.
    Length,
    /// The provider filtered the reply.
    ContentFilter,
    /// The model stopped to call a tool.
    ToolCalls,
    /// A reason outside this list. The service's own spelling is not kept:
    /// it is text the service chose, and errors and logs show this value.
    Other,
}

/// What a chat model answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The reply text; empty when the model wrote none.
    pub text: String,
    /// Why the reply ended; `None` when the provider did not say.
    pub finish: Option<Finish>,
    pub usage: Usage,
}

/// What one request used, as far as its service or the backend's price
/// list tells.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Every input token, cached ones included.
    pub input_tokens: Option<u64>,
    /// The part of `input_tokens` read from the service's cache.
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// What the request cost in US dollars: the service's own figure, or
    /// the model's list price for the tokens counted; `None` when neither
    /// is known.
    pub cost_usd: Option<f64>,
}

/// The model APIs a [`ModelBackend`] reaches, in Limen's own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// Any server with the OpenAI-compatible routes under its base URL,
    /// such as vLLM, sglang, llama.cpp, or Ollama.
    Compatible,
    OpenAi,
    Anthropic,
    Gemini,
    DeepSeek,
    Mistral,
    Groq,
    OpenRouter,
    XAi,
    /// The ChatGPT plan's backend, with the stored sign-in's credential.
    ChatGpt,
    /// GitHub Copilot, with the stored sign-in's session.
    Copilot,
    Together,
    Fireworks,
    Cohere,
    /// Moonshot AI's Kimi platform.
    Kimi,
    /// Zhipu's Z.ai platform.
    Zai,
    /// Alibaba Cloud's international DashScope, which serves Qwen.
    Qwen,
    MiniMax,
    Nebius,
}

impl Api {
    /// The chat API polishing uses unless its provider names one; `None`
    /// for an API that has only its own.
    pub(crate) fn chat_api(self) -> Option<crate::config::ChatApi> {
        use crate::config::ChatApi;
        match self {
            Self::OpenAi | Self::XAi => Some(ChatApi::Responses),
            Self::Compatible | Self::DeepSeek | Self::Mistral | Self::Groq | Self::OpenRouter => {
                Some(ChatApi::ChatCompletions)
            }
            // A subscription's backend picks its own: ChatGPT's takes only
            // Responses, Copilot's routes each model. The vendors below
            // have one chat API each.
            Self::Anthropic
            | Self::Gemini
            | Self::ChatGpt
            | Self::Copilot
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
}

/// A plan the user signs in to, in place of paying for an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscription {
    ChatGpt,
    Copilot,
}

/// What a sign-in asks the user to do while it waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStep {
    /// Finish signing in on `url`, which the browser was asked to open.
    Browser { url: String },
    /// Enter `code` on `url`, on any device.
    DeviceCode { url: String, code: String },
}

/// Told each [`SignInStep`] as a sign-in reaches it.
pub type SignInPrompt = Box<dyn Fn(SignInStep) + Send + Sync>;

/// A failure worth sending the same request again for: the service could
/// not be reached, or said it is busy or failing for now. A backend returns
/// it, inside its `anyhow::Error`, only for such failures; the core tries
/// nothing else again, so a rejected key or a refused request is never
/// repeated.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Transient(pub String);

/// Where a backend sends one role's requests, and with which key.
#[derive(Clone, PartialEq)]
pub struct Connection {
    pub api: Api,
    /// The API root that routes such as `/chat/completions` hang off, for
    /// example `http://127.0.0.1:8000/v1`. Required for
    /// [`Api::Compatible`]; for a vendor, `None` is its public endpoint.
    pub base_url: Option<String>,
    /// `None` sends no credential, which a local server may not need.
    pub api_key: Option<String>,
    pub shape: Shape,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("api", &self.api)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .field("shape", &self.shape)
            .finish()
    }
}

/// How a connection's requests differ from its API's own, as the user set
/// the provider up. A backend that cannot send a request so shaped, such as
/// a route on an API whose routes it cannot move, refuses the connection.
#[derive(Clone, Default, PartialEq)]
pub struct Shape {
    /// Which of OpenAI's chat APIs a chat request uses; unset for an API
    /// that has only its own, and for transcription.
    pub chat_api: Option<crate::config::ChatApi>,
    /// The header the key is sent in, alone, in place of the API's own.
    pub key_header: Option<String>,
    /// Headers every request carries.
    pub headers: Vec<(String, String)>,
    /// Fields every request body also carries.
    pub body: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for Shape {
    /// Header values can be private, so only their names show.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Shape")
            .field("chat_api", &self.chat_api)
            .field("key_header", &self.key_header)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body", &self.body.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Builds the model clients for a [`Connection`].
///
/// The program hands one backend to the daemon and the GUI when it starts;
/// the core never builds one itself, so it does not depend on the framework
/// an implementation wraps. Implementations only translate: timeouts,
/// retries of the user's take, and validation of replies stay in the core.
/// Their errors reach logs and the GUI, so they must never quote what a
/// service sent back, which can echo the request or a rejected key.
/// A connection to an API that cannot serve the request, such as
/// transcription on a chat-only vendor, is an error.
pub trait ModelBackend: Send + Sync {
    /// The transcriber for `model` on `connection`.
    fn transcriber(&self, connection: Connection, model: String) -> Result<Box<dyn Transcriber>>;

    /// The chat model `model` on `connection`.
    fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>>;

    /// The models `connection` offers, in its order. The caller bounds the
    /// call with its own timeout.
    fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>>;

    /// Sign in to `subscription` again, telling `prompt` what the user must
    /// do, and keep the credential where later requests read it. Dropping
    /// the future abandons the sign-in.
    fn sign_in(
        &self,
        subscription: Subscription,
        prompt: SignInPrompt,
    ) -> BoxFuture<'_, Result<()>>;

    /// Forget the sign-in to `subscription`, revoking it where its
    /// provider can.
    fn sign_out(&self, subscription: Subscription) -> BoxFuture<'_, Result<()>>;

    /// Whether a sign-in to `subscription` is kept. It may still have
    /// expired; a request then says so.
    fn signed_in(&self, subscription: Subscription) -> bool;
}

/// The list prices of chat models, for a backend whose services report
/// tokens but not what they cost.
///
/// The program builds one and hands it to such a backend, which asks it once
/// per reply and puts the answer in [`Usage::cost_usd`]. Implementations
/// price at the standard tier's list price, so the figure is an estimate.
pub trait PriceList: Send + Sync {
    /// What `usage`'s tokens cost on `model` of `api`, in US dollars;
    /// `None` when the list has no price for the model, or `usage` counts
    /// no tokens.
    fn price(&self, api: Api, model: &str, usage: &Usage) -> Option<f64>;
}

/// A chat model that answers one exchange at a time.
///
/// Polishing asks for one deterministic, short correction, so
/// implementations should send it at temperature 0 and with the least
/// reasoning the model allows. They report the reply as it came: deciding
/// whether the text is usable is the caller's job. The caller bounds each
/// call with its own timeout and drops the future to cancel it.
pub trait ChatModel: Send + Sync {
    /// The model's reply to `exchange`, or why the request failed.
    fn reply<'a>(&'a self, exchange: Exchange<'a>) -> BoxFuture<'a, Result<Reply>>;
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, bail};

    use super::{
        BoxFuture, ChatModel, Connection, Exchange, ModelBackend, Reply, SignInPrompt,
        Subscription, Take, Transcriber, Transcript, Usage,
    };

    /// A backend that refuses every request.
    pub(crate) struct Refusing;

    impl ModelBackend for Refusing {
        fn transcriber(&self, _: Connection, _: String) -> Result<Box<dyn Transcriber>> {
            bail!("no transcriber in this test")
        }

        fn chat(&self, _: Connection, _: String) -> Result<Box<dyn ChatModel>> {
            bail!("no chat model in this test")
        }

        fn models(&self, _: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
            Box::pin(async { bail!("no models in this test") })
        }

        fn sign_in(&self, _: Subscription, _: SignInPrompt) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { bail!("no sign-in in this test") })
        }

        fn sign_out(&self, _: Subscription) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn signed_in(&self, _: Subscription) -> bool {
            false
        }
    }

    /// One request a [`Recording`] backend was asked to build.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Built {
        Transcriber(Connection, String),
        Chat(Connection, String),
        Models(Connection),
    }

    /// A backend that records what it builds, lists `models`, and builds
    /// models that fail every request.
    #[derive(Default)]
    pub(crate) struct Recording {
        pub(crate) models: Vec<String>,
        pub(crate) built: Arc<Mutex<Vec<Built>>>,
    }

    struct Failing;

    impl Transcriber for Failing {
        fn transcribe(&self, _: Take) -> BoxFuture<'_, Result<Transcript>> {
            Box::pin(async { bail!("this test transcribes nothing") })
        }
    }

    impl ChatModel for Failing {
        fn reply<'a>(&'a self, _: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
            Box::pin(async { bail!("this test answers nothing") })
        }
    }

    /// A backend whose models answer: the transcriber with `transcript`,
    /// the chat model with `reply`.
    pub(crate) struct Answering {
        pub(crate) transcript: String,
        pub(crate) reply: Reply,
    }

    struct Answer(String, Reply);

    impl Transcriber for Answer {
        fn transcribe(&self, _: Take) -> BoxFuture<'_, Result<Transcript>> {
            let transcript = self.0.clone();
            Box::pin(async move {
                Ok(Transcript {
                    text: transcript,
                    usage: Usage::default(),
                })
            })
        }
    }

    impl ChatModel for Answer {
        fn reply<'a>(&'a self, _: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
            let reply = self.1.clone();
            Box::pin(async move { Ok(reply) })
        }
    }

    impl ModelBackend for Answering {
        fn transcriber(&self, _: Connection, _: String) -> Result<Box<dyn Transcriber>> {
            Ok(Box::new(Answer(
                self.transcript.clone(),
                self.reply.clone(),
            )))
        }

        fn chat(&self, _: Connection, _: String) -> Result<Box<dyn ChatModel>> {
            Ok(Box::new(Answer(
                self.transcript.clone(),
                self.reply.clone(),
            )))
        }

        fn models(&self, _: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn sign_in(&self, _: Subscription, _: SignInPrompt) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { bail!("no sign-in in this test") })
        }

        fn sign_out(&self, _: Subscription) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn signed_in(&self, _: Subscription) -> bool {
            false
        }
    }

    impl ModelBackend for Recording {
        fn transcriber(
            &self,
            connection: Connection,
            model: String,
        ) -> Result<Box<dyn Transcriber>> {
            self.built
                .lock()
                .unwrap()
                .push(Built::Transcriber(connection, model));
            Ok(Box::new(Failing))
        }

        fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>> {
            self.built
                .lock()
                .unwrap()
                .push(Built::Chat(connection, model));
            Ok(Box::new(Failing))
        }

        fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
            self.built.lock().unwrap().push(Built::Models(connection));
            let models = self.models.clone();
            Box::pin(async move { Ok(models) })
        }

        fn sign_in(&self, _: Subscription, _: SignInPrompt) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { bail!("no sign-in in this test") })
        }

        fn sign_out(&self, _: Subscription) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }

        fn signed_in(&self, _: Subscription) -> bool {
            false
        }
    }
}
