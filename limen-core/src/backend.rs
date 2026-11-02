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
