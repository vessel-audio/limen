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

/// A model that turns one take into its text.
///
/// The daemon builds one per take with the model and key already bound, and
/// calls [`transcribe`](Self::transcribe) once, after recording ends. The
/// caller bounds the call with its own timeout and drops the future when the
/// take is abandoned, so implementations must not keep working after a drop.
pub trait Transcriber: Send + Sync {
    /// The transcript of `take`, or why there is none.
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<String>>;
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
    /// A reason outside this list, in the provider's own spelling.
    Other(String),
}

/// What a chat model answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The reply text; empty when the model wrote none.
    pub text: String,
    /// Why the reply ended; `None` when the provider did not say.
    pub finish: Option<Finish>,
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
}

/// Where a backend sends one role's requests, and with which key.
#[derive(Clone, PartialEq, Eq)]
pub struct Connection {
    pub api: Api,
    /// The API root that routes such as `/chat/completions` hang off, for
    /// example `http://127.0.0.1:8000/v1`. Required for
    /// [`Api::Compatible`]; for a vendor, `None` is its public endpoint.
    pub base_url: Option<String>,
    /// `None` sends no credential, which a local server may not need.
    pub api_key: Option<String>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("api", &self.api)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Builds the model clients for a [`Connection`].
///
/// The program hands one backend to the daemon and the GUI when it starts;
/// the core never builds one itself, so it does not depend on the framework
/// an implementation wraps. Implementations only translate: timeouts,
/// retries of the user's take, and validation of replies stay in the core.
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
        BoxFuture, ChatModel, Connection, Exchange, ModelBackend, Reply, Take, Transcriber,
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
    }

    /// One request a [`Recording`] backend was asked to build.
    #[derive(Debug, Clone, PartialEq, Eq)]
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
        fn transcribe(&self, _: Take) -> BoxFuture<'_, Result<String>> {
            Box::pin(async { bail!("this test transcribes nothing") })
        }
    }

    impl ChatModel for Failing {
        fn reply<'a>(&'a self, _: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
            Box::pin(async { bail!("this test answers nothing") })
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
    }
}
