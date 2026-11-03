//! Limen's model backend on [rig](https://github.com/0xPlaygrounds/rig).
//!
//! This crate is the only one that names rig: it translates
//! [`limen_core::Connection`]s into rig clients and their replies back into
//! Limen's types. Replacing the framework means replacing this crate; the
//! core and the GUI only see [`limen_core::ModelBackend`].

use anyhow::{Context, Result, bail};
use limen_core::{
    Api, BoxFuture, ChatModel, Connection, Exchange, Finish, ModelBackend, Reply, Take, Transcriber,
};
use rig_core::DynModel;
use rig_core::completion::{CompletionRequest, FinishReason, OnUnsupported, Reasoning};
use rig_core::error::ProviderError;
use rig_core::operation::{Completion, Transcription};
use rig_core::providers::anthropic::{Anthropic, AnthropicConfig};
use rig_core::providers::gemini::{Gemini, GeminiConfig};
use rig_core::providers::openai::OpenAI;
use rig_core::providers::openai::wire::{
    Auth, DEEPSEEK, Dialect, GROQ, MISTRAL, OPENAI, OPENROUTER, OpenAIConfig,
};
use rig_core::transcription::TranscriptionRequestBuilder;
use rig_reqwest::ReqwestClient;

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
pub struct Rig;

impl Rig {
    fn client(&self, connection: Connection) -> Result<Client> {
        let http = ReqwestClient::from(
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("failed to build the model HTTP client")?,
        );
        let Connection {
            api,
            base_url,
            api_key,
        } = connection;
        let key = api_key.unwrap_or_default();
        let openai = |dialect: &Dialect| {
            let config = OpenAIConfig::with_key(dialect, key.clone());
            let config = match &base_url {
                // A custom server speaking a vendor's API may run without a
                // key, and then gets no `Authorization` header.
                Some(base_url) => OpenAIConfig {
                    auth: Auth::OptionalBearer,
                    ..config.with_base_url(base_url)
                },
                None => config,
            };
            Client::OpenAi(Box::new(config.connect(http.clone())))
        };
        Ok(match api {
            Api::Compatible => {
                let base_url = base_url.context("an OpenAI-compatible server needs its URL")?;
                let mut config = OpenAIConfig::with_key(&COMPATIBLE, key).with_base_url(base_url);
                // A local server started without a key rejects any
                // `Authorization` header, so send one only with a key.
                config.auth = Auth::OptionalBearer;
                Client::OpenAi(Box::new(config.connect(http.clone())))
            }
            Api::OpenAi => openai(&OPENAI),
            Api::DeepSeek => openai(&DEEPSEEK),
            Api::Mistral => openai(&MISTRAL),
            Api::Groq => openai(&GROQ),
            Api::OpenRouter => openai(&OPENROUTER),
            Api::XAi => openai(&rig_core::providers::xai::DIALECT),
            Api::Anthropic => {
                let config = AnthropicConfig::new(key);
                let config = match &base_url {
                    Some(base_url) => config.with_base_url(base_url),
                    None => config,
                };
                Client::Anthropic(config.connect(http.clone()))
            }
            Api::Gemini => {
                let config = GeminiConfig::new(key);
                let config = match &base_url {
                    Some(base_url) => config.with_base_url(base_url),
                    None => config,
                };
                Client::Gemini(config.connect(http.clone()))
            }
        })
    }
}

/// A rig client of one of its client families.
enum Client {
    OpenAi(Box<OpenAI>),
    Anthropic(Anthropic),
    Gemini(Gemini),
}

impl ModelBackend for Rig {
    fn transcriber(&self, connection: Connection, model: String) -> Result<Box<dyn Transcriber>> {
        let api = connection.api;
        let model = match (api, self.client(connection)?) {
            (Api::DeepSeek | Api::Anthropic, _) => bail!("{api:?} does not transcribe audio"),
            (_, Client::OpenAi(client)) => client.transcription(model).erase(),
            (_, Client::Gemini(client)) => client.transcription(model).erase(),
            (_, Client::Anthropic(_)) => bail!("{api:?} does not transcribe audio"),
        };
        Ok(Box::new(RigTranscriber(model)))
    }

    fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>> {
        let model = match self.client(connection)? {
            Client::OpenAi(client) => client.completion(model).erase(),
            Client::Anthropic(client) => client.completion(model).erase(),
            Client::Gemini(client) => client.completion(model).erase(),
        };
        Ok(Box::new(RigChat(model)))
    }

    fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
        let client = self.client(connection);
        Box::pin(async move {
            let listed = match client? {
                Client::OpenAi(client) => client.list_models().await,
                Client::Anthropic(client) => client.list_models().await,
                Client::Gemini(client) => client.list_models().await,
            }
            .map_err(sanitized)?;
            Ok(listed.data.into_iter().map(|model| model.id).collect())
        })
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
                Some(status) => anyhow::anyhow!("the service returned HTTP {status}"),
                None => anyhow::anyhow!("the service returned an error"),
            }
        }
        ProviderError::Http(error) => anyhow::anyhow!("the request did not complete: {error}"),
        ProviderError::Json(_) | ProviderError::Response(_) => {
            anyhow::anyhow!("the service sent a reply that could not be read")
        }
        ProviderError::Url(_) | ProviderError::Request(_) => {
            anyhow::anyhow!("the request could not be built")
        }
        ProviderError::Truncated => anyhow::anyhow!("the reply ended early"),
        ProviderError::Stalled(after) => anyhow::anyhow!("the reply stalled for {after:?}"),
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

struct RigTranscriber(DynModel<Transcription>);

impl Transcriber for RigTranscriber {
    fn transcribe(&self, take: Take) -> BoxFuture<'_, Result<String>> {
        let request = TranscriptionRequestBuilder::new(take.wav).filename("audio.wav".to_string());
        let request = match take.language {
            Some(language) => request.language(language),
            None => request,
        };
        let reply = self.0.call(request.build());
        Box::pin(async move { Ok(reply.await.map_err(sanitized)?.text) })
    }
}

struct RigChat(DynModel<Completion>);

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
        let reply = self.0.call(request);
        Box::pin(async move {
            let response = reply.await.map_err(sanitized)?;
            Ok(Reply {
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

#[cfg(test)]
mod tests;
