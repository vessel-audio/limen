//! Limen's model backend on [rig](https://github.com/0xPlaygrounds/rig).
//!
//! This crate is the only one that names rig: it translates
//! [`limen_core::Connection`]s into rig clients and their replies back into
//! Limen's types. Replacing the framework means replacing this crate; the
//! core and the GUI only see [`limen_core::ModelBackend`].

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use limen_core::{
    Api, BoxFuture, ChatApi, ChatModel, Connection, Exchange, Finish, ModelBackend, Reply,
    SignInPrompt, Subscription, Take, Transcriber, Transcript, Transient,
};
use rig_core::DynModel;
use rig_core::completion::{CompletionRequest, FinishReason, OnUnsupported, Reasoning};
use rig_core::error::ProviderError;
use rig_core::operation::{Completion, Transcription};
use rig_core::providers::anthropic::wire::Auth as AnthropicAuth;
use rig_core::providers::anthropic::{Anthropic, AnthropicConfig};
use rig_core::providers::gemini::{Gemini, GeminiConfig};
use rig_core::providers::openai::wire::{
    Auth, DEEPSEEK, Dialect, GROQ, MISTRAL, OPENAI, OPENROUTER, OpenAIConfig,
};
use rig_core::providers::openai::{OpenAI, Route};
use rig_core::transcription::TranscriptionRequestBuilder;
use rig_reqwest::ReqwestClient;

/// Fields a request body also carries.
type JsonMap = serde_json::Map<String, serde_json::Value>;

/// A server Limen knows only by its OpenAI-compatible routes. Its default
/// base URL is never used: Limen always names the server.
const COMPATIBLE: Dialect = Dialect::gateway("openai-compatible", "http://127.0.0.1/v1", "");

/// The [`ModelBackend`] that reaches every [`Api`] through rig.
///
/// Each model client gets its own HTTP client, which never follows
/// redirects so a key is only ever sent to the host it was configured for.
/// It is built with the model client rather than with the backend: a
/// system without CA certificates cannot build one, and that must fail the
/// request, not stop the daemon or the GUI from starting.
///
/// Subscriptions' sign-ins are kept in files under its sign-in directory.
pub struct Rig {
    sign_ins: PathBuf,
}

/// An HTTP client that never follows redirects and sends `headers` with
/// every request.
fn http(headers: reqwest::header::HeaderMap) -> Result<ReqwestClient> {
    Ok(ReqwestClient::from(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()
            .context("failed to build the model HTTP client")?,
    ))
}

impl Rig {
    /// The backend keeping subscriptions' sign-ins under `sign_ins`.
    pub fn new(sign_ins: PathBuf) -> Self {
        Self { sign_ins }
    }

    /// The rig client for `connection`, and the fields every request body
    /// also carries.
    fn client(&self, connection: Connection) -> Result<(Client, JsonMap)> {
        let Connection {
            api,
            base_url,
            api_key,
            shape,
        } = connection;
        let key = api_key.unwrap_or_default();
        // A key in a header of the user's naming goes with the other headers,
        // and rig, given none, sends none of its own.
        let (key, key_header) = match shape.key_header {
            Some(header) if !key.is_empty() => (String::new(), Some((header, key))),
            Some(_) | None => (key, None),
        };
        if let (Api::Gemini, Some(_)) = (api, &key_header) {
            bail!("Gemini sends its key in the URL, not in a header");
        }
        let headers = shape
            .headers
            .iter()
            .chain(key_header.iter())
            .map(|(name, value)| {
                Ok((
                    reqwest::header::HeaderName::try_from(name.as_str())
                        .with_context(|| format!("{name:?} is not a header name"))?,
                    reqwest::header::HeaderValue::try_from(value.as_str())
                        .with_context(|| format!("the {name} header's value is not valid"))?,
                ))
            })
            .collect::<Result<reqwest::header::HeaderMap>>()?;
        let http = http(headers)?;
        let openai = |dialect: &Dialect| {
            let mut config = OpenAIConfig::with_key(dialect, key.clone());
            if let Some(base_url) = &base_url {
                config = config.with_base_url(base_url);
                // A custom server speaking a vendor's API may run without a
                // key, and then gets no `Authorization` header.
                config.auth = Auth::OptionalBearer;
            }
            // A key moved to its own header leaves no `Authorization` either.
            if key.is_empty() {
                config.auth = Auth::OptionalBearer;
            }
            let config = match shape.chat_api {
                Some(ChatApi::ChatCompletions) => config.with_route(Route::Chat),
                Some(ChatApi::Responses) => config.with_route(Route::Responses),
                None => config,
            };
            Client::OpenAi(Box::new(config.connect(http.clone())))
        };
        let one_chat_api = |name: &str| match shape.chat_api {
            Some(_) => bail!("{name} has one chat API; only OpenAI's APIs offer a choice"),
            None => Ok(()),
        };
        let client = match api {
            Api::Compatible => match &base_url {
                Some(_) => openai(&COMPATIBLE),
                None => bail!("an OpenAI-compatible server needs its URL"),
            },
            Api::OpenAi => openai(&OPENAI),
            Api::DeepSeek => openai(&DEEPSEEK),
            Api::Mistral => openai(&MISTRAL),
            Api::Groq => openai(&GROQ),
            Api::OpenRouter => openai(&OPENROUTER),
            Api::XAi => openai(&rig_core::providers::xai::DIALECT),
            Api::Anthropic => {
                one_chat_api("Anthropic")?;
                let mut config = AnthropicConfig::new(key.clone());
                // A keyless server, or a key in its own header, gets no
                // credential header from rig, rather than an empty one.
                if key.is_empty() {
                    config.dialect.quirks.auth = AnthropicAuth::OptionalBearer;
                }
                let config = match &base_url {
                    Some(base_url) => config.with_base_url(base_url),
                    None => config,
                };
                Client::Anthropic(config.connect(http.clone()))
            }
            Api::Gemini => {
                one_chat_api("Gemini")?;
                // Gemini always sends its key in the URL; without one it
                // would ask with an empty key.
                if key.is_empty() {
                    bail!("a Gemini server needs an API key");
                }
                let config = GeminiConfig::new(key.clone());
                let config = match &base_url {
                    Some(base_url) => config.with_base_url(base_url),
                    None => config,
                };
                Client::Gemini(config.connect(http.clone()))
            }
            // A subscription's sign-in, not a key, authorizes its requests.
            Api::ChatGpt => Client::SignedIn(signed_in::Account::new(
                Subscription::ChatGpt,
                &self.sign_ins,
                http,
                None,
            )),
            Api::Copilot => Client::SignedIn(signed_in::Account::new(
                Subscription::Copilot,
                &self.sign_ins,
                http,
                None,
            )),
            Api::Together
            | Api::Fireworks
            | Api::Cohere
            | Api::Kimi
            | Api::Zai
            | Api::Qwen
            | Api::MiniMax
            | Api::Nebius => bail!("{api:?} is not reached through rig"),
        };
        Ok((client, shape.body))
    }
}

/// A rig client of one of its client families, or a subscription's sign-in,
/// which makes one for each request.
enum Client {
    OpenAi(Box<OpenAI>),
    Anthropic(Anthropic),
    Gemini(Gemini),
    SignedIn(signed_in::Account),
}

impl ModelBackend for Rig {
    fn transcriber(&self, connection: Connection, model: String) -> Result<Box<dyn Transcriber>> {
        let api = connection.api;
        let (client, body) = self.client(connection)?;
        let transcription = match (api, client) {
            (Api::DeepSeek | Api::Anthropic, _) => bail!("{api:?} does not transcribe audio"),
            (_, Client::OpenAi(client)) => client.transcription(model.clone()).erase(),
            (_, Client::Gemini(client)) => client.transcription(model.clone()).erase(),
            (_, Client::Anthropic(_) | Client::SignedIn(_)) => {
                bail!("{api:?} does not transcribe audio")
            }
        };
        Ok(Box::new(RigTranscriber {
            model: transcription,
            body,
            api,
            name: model,
        }))
    }

    fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>> {
        let api = connection.api;
        let (client, body) = self.client(connection)?;
        let chat = match client {
            Client::OpenAi(client) => Chat::Ready(client.completion(model.clone()).erase()),
            Client::Anthropic(client) => Chat::Ready(client.completion(model.clone()).erase()),
            Client::Gemini(client) => Chat::Ready(client.completion(model.clone()).erase()),
            Client::SignedIn(account) => Chat::SignedIn(account, model.clone()),
        };
        Ok(Box::new(RigChat {
            chat,
            body,
            api,
            name: model,
        }))
    }

    fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
        let client = self.client(connection).map(|(client, _)| client);
        Box::pin(async move {
            let listed = match client? {
                Client::OpenAi(client) => client.list_models().await,
                Client::Anthropic(client) => client.list_models().await,
                Client::Gemini(client) => client.list_models().await,
                // A subscription serves what its plan offers, as rig knows it.
                Client::SignedIn(account) => return Ok(account.models()),
            }
            .map_err(sanitized)?;
            Ok(listed.data.into_iter().map(|model| model.id).collect())
        })
    }

    fn sign_in(
        &self,
        subscription: Subscription,
        prompt: SignInPrompt,
    ) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let http = http(Default::default())?;
            signed_in::sign_in(&self.sign_ins, subscription, http, Arc::from(prompt)).await
        })
    }

    fn sign_out(&self, subscription: Subscription) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let http = http(Default::default())?;
            signed_in::sign_out(&self.sign_ins, subscription, http).await
        })
    }

    fn signed_in(&self, subscription: Subscription) -> bool {
        signed_in::kept(&self.sign_ins, subscription)
    }
}

/// `error` without anything the service sent back: a reply body can quote
/// the request, and some services echo a rejected key in it.
fn sanitized(error: ProviderError) -> anyhow::Error {
    match error {
        ProviderError::InvalidAuthentication(response)
        | ProviderError::ProviderResponse(response)
        | ProviderError::CacheExpired { response, .. } => {
            match response.status.map(|status| status.as_u16()) {
                Some(status @ (401 | 403)) => {
                    anyhow::anyhow!("the service rejected the API key with HTTP {status}")
                }
                // Busy, or failing for now: worth trying again.
                Some(status @ (408 | 429 | 500..=599)) => {
                    Transient(format!("the service returned HTTP {status}")).into()
                }
                Some(status) => anyhow::anyhow!("the service returned HTTP {status}"),
                None => anyhow::anyhow!("the service returned an error"),
            }
        }
        // The transport's error quotes the URL, where Gemini puts the key.
        ProviderError::Http(_) => Transient("the service could not be reached".to_string()).into(),
        ProviderError::Json(_) | ProviderError::Response(_) => {
            anyhow::anyhow!("the service sent a reply that could not be read")
        }
        ProviderError::Url(_) | ProviderError::Request(_) => {
            anyhow::anyhow!("the request could not be built")
        }
        ProviderError::Truncated => Transient("the reply ended early".to_string()).into(),
        ProviderError::Stalled(after) => {
            Transient(format!("the reply stalled for {after:?}")).into()
        }
        // rig's own diagnostics and refusals of an option name no reply.
        error @ (ProviderError::Provider(_) | ProviderError::UnsupportedOption(_)) => {
            anyhow::anyhow!("{error}")
        }
        ProviderError::MismatchedDimensions { .. } | ProviderError::Relayed(_) => {
            anyhow::anyhow!("the service failed")
        }
        // rig may add failures; say no more about them than that.
        _ => anyhow::anyhow!("the service failed"),
    }
}

/// A transcription model, the fields every request also carries, and the
/// API and model it is priced as.
struct RigTranscriber {
    model: DynModel<Transcription>,
    body: JsonMap,
    api: Api,
    name: String,
}

impl Transcriber for RigTranscriber {
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<Transcript>> {
        let request = TranscriptionRequestBuilder::new(take.wav).filename("audio.wav".to_string());
        let request = match take.language {
            Some(language) => request.language(language),
            None => request,
        };
        let request = match self.body.is_empty() {
            true => request,
            false => request.additional_params(serde_json::Value::Object(self.body.clone())),
        };
        let reply = self.model.call(request.build());
        Box::pin(async move {
            let reply = reply.await.map_err(sanitized)?;
            Ok(Transcript {
                usage: prices::usage(self.api, &self.name, &reply.usage),
                text: reply.text,
            })
        })
    }
}

/// A chat model, or the sign-in that connects one for each request.
enum Chat {
    Ready(DynModel<Completion>),
    SignedIn(signed_in::Account, String),
}

/// A chat model, the fields every request also carries, and the API and
/// model it is priced as.
struct RigChat {
    chat: Chat,
    body: JsonMap,
    api: Api,
    name: String,
}

impl ChatModel for RigChat {
    fn reply<'a>(&'a self, exchange: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
        let request = CompletionRequest::new(exchange.message)
            .preamble(exchange.instructions)
            .temperature(0.0)
            .max_tokens(exchange.max_tokens)
            .reasoning(Reasoning::Off)
            // A model that cannot turn reasoning off, or takes no
            // temperature, still polishes with its own default.
            .on_unsupported(OnUnsupported::Ignore);
        let request = match self.body.is_empty() {
            true => request,
            false => request.additional_params(serde_json::Value::Object(self.body.clone())),
        };
        Box::pin(async move {
            let model = match &self.chat {
                Chat::Ready(model) => model.clone(),
                Chat::SignedIn(account, model) => account.completion(model).await?,
            };
            let response = model.call(request).await.map_err(sanitized)?;
            Ok(Reply {
                usage: prices::usage(self.api, &self.name, &response.usage),
                text: response.text(),
                finish: response.finish_reason().map(|reason| match reason {
                    FinishReason::Stop => Finish::Stop,
                    FinishReason::Length => Finish::Length,
                    FinishReason::ContentFilter => Finish::ContentFilter,
                    FinishReason::ToolCalls => Finish::ToolCalls,
                    // rig may add reasons; none of them is a complete reply.
                    _ => Finish::Other,
                }),
            })
        })
    }
}

mod prices;
mod signed_in;

pub use prices::Catalog;

#[cfg(test)]
mod tests;
