//! Limen's model backend on [genai](https://github.com/jeremychone/rust-genai),
//! for the chat vendors rig does not reach.
//!
//! This crate is the only one that names genai: it translates
//! [`limen_core::Connection`]s into genai service targets and their replies
//! back into Limen's types. genai reports tokens but not what they cost, so
//! each reply is priced from the [`PriceList`] the program hands it.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponse, StopReason};
use genai::resolver::{AuthData, Endpoint, ProviderConfig};
use genai::{Client, ModelIden, ServiceTarget};
use limen_core::{
    Api, BoxFuture, ChatModel, Connection, Exchange, Finish, ModelBackend, PriceList, Proxy, Reply,
    SignInPrompt, Subscription, Transcriber, Transient, Usage,
};

/// The [`ModelBackend`] for the chat vendors only genai reaches. None of
/// them transcribes, and none is a subscription.
///
/// As with rig, each model gets its own HTTP client, which never follows
/// redirects so a key only goes to the host it was configured for, and is
/// built with the model so a system without CA certificates fails the
/// request rather than the program.
pub struct Genai {
    prices: Arc<dyn PriceList>,
}

impl Genai {
    /// The backend whose replies are priced from `prices`.
    pub fn new(prices: Arc<dyn PriceList>) -> Self {
        Self { prices }
    }

    /// Whether `api` is one this backend reaches.
    pub fn serves(api: Api) -> bool {
        adapter(api).is_some()
    }
}

/// genai's adapter for `api`, and the endpoint it is reached at unless the
/// connection names another; `None` for an API rig reaches.
fn adapter(api: Api) -> Option<(AdapterKind, &'static str)> {
    match api {
        Api::Together => Some((AdapterKind::Together, "https://api.together.xyz/v1/")),
        Api::Fireworks => Some((
            AdapterKind::Fireworks,
            "https://api.fireworks.ai/inference/v1/",
        )),
        Api::Cohere => Some((AdapterKind::Cohere, "https://api.cohere.com/v1/")),
        Api::Kimi => Some((AdapterKind::Kimi, "https://api.moonshot.ai/v1/")),
        Api::Zai => Some((AdapterKind::Zai, "https://api.z.ai/api/paas/v4/")),
        Api::Qwen => Some((
            AdapterKind::QwenCloud,
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/",
        )),
        Api::MiniMax => Some((AdapterKind::MiniMax, "https://api.minimax.io/anthropic/v1/")),
        Api::Nebius => Some((AdapterKind::Nebius, "https://api.studio.nebius.ai/v1/")),
        Api::Compatible
        | Api::OpenAi
        | Api::Anthropic
        | Api::Gemini
        | Api::DeepSeek
        | Api::Mistral
        | Api::Groq
        | Api::OpenRouter
        | Api::XAi
        | Api::ChatGpt
        | Api::Copilot => None,
    }
}

/// What a request to `connection` is sent with: genai's client, the
/// adapter and endpoint, the key, and the options every request carries.
struct Target {
    client: Client,
    kind: AdapterKind,
    endpoint: Endpoint,
    auth: AuthData,
    options: ChatOptions,
}

fn target(connection: Connection) -> Result<Target> {
    let Connection {
        api,
        base_url,
        api_key,
        shape,
        proxy,
    } = connection;
    let Some((kind, endpoint)) = adapter(api) else {
        bail!("{api:?} is not reached through genai");
    };
    if shape.key_header.is_some() {
        bail!("{api:?} takes its key only in its own header");
    }
    if shape.chat_api.is_some() {
        bail!("{api:?} has one chat API; only OpenAI's APIs offer a choice");
    }
    let Some(key) = api_key.filter(|key| !key.is_empty()) else {
        bail!("{api:?} needs an API key");
    };
    // genai joins routes onto the endpoint, which must end in a slash.
    let endpoint = match base_url {
        Some(url) => Endpoint::from_owned(format!("{}/", url.trim_end_matches('/'))),
        None => Endpoint::from_static(endpoint),
    };
    let http = proxy
        .route(reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()))?
        .build()
        .context("failed to build the chat HTTP client")?;
    let options = ChatOptions::default()
        .with_temperature(0.)
        .with_capture_usage(true);
    let options = match shape.headers.is_empty() {
        true => options,
        false => options.with_extra_headers(shape.headers),
    };
    let options = match shape.body.is_empty() {
        true => options,
        false => options.with_extra_body(serde_json::Value::Object(shape.body)),
    };
    Ok(Target {
        client: Client::builder()
            .with_reqwest(http)
            .build()
            .map_err(|_| anyhow::anyhow!("failed to build the chat client"))?,
        kind,
        endpoint,
        auth: AuthData::from_single(key),
        options,
    })
}

impl ModelBackend for Genai {
    fn transcriber(&self, connection: Connection, _: String) -> Result<Box<dyn Transcriber>> {
        bail!("{:?} does not transcribe audio", connection.api)
    }

    fn chat(&self, connection: Connection, model: String) -> Result<Box<dyn ChatModel>> {
        let api = connection.api;
        Ok(Box::new(GenaiChat {
            target: target(connection)?,
            api,
            model,
            prices: self.prices.clone(),
        }))
    }

    fn models(&self, connection: Connection) -> BoxFuture<'_, Result<Vec<String>>> {
        let target = target(connection);
        Box::pin(async move {
            let Target {
                client,
                kind,
                endpoint,
                auth,
                ..
            } = target?;
            client
                .all_model_names(
                    kind,
                    ProviderConfig::from_endpoint(endpoint).with_auth(auth),
                )
                .await
                .map_err(sanitized)
        })
    }

    fn sign_in(
        &self,
        subscription: Subscription,
        _: Proxy,
        _: SignInPrompt,
    ) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { bail!("{subscription:?} is not signed in to through genai") })
    }

    fn sign_out(&self, subscription: Subscription, _: Proxy) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { bail!("{subscription:?} is not signed in to through genai") })
    }

    fn signed_in(&self, _: Subscription) -> bool {
        false
    }
}

/// A chat model on one vendor, priced from the program's price list.
struct GenaiChat {
    target: Target,
    api: Api,
    model: String,
    prices: Arc<dyn PriceList>,
}

impl ChatModel for GenaiChat {
    fn reply<'a>(&'a self, exchange: Exchange<'a>) -> BoxFuture<'a, Result<Reply>> {
        let request = ChatRequest::new(vec![ChatMessage::user(exchange.message)])
            .with_system(exchange.instructions);
        let options = self
            .target
            .options
            .clone()
            .with_max_tokens(u32::try_from(exchange.max_tokens).unwrap_or(u32::MAX));
        let service = ServiceTarget {
            endpoint: self.target.endpoint.clone(),
            auth: self.target.auth.clone(),
            model: ModelIden::new(self.target.kind, self.model.as_str()),
        };
        Box::pin(async move {
            let response = self
                .target
                .client
                .exec_chat(service, request, Some(&options))
                .await
                .map_err(sanitized)?;
            Ok(reply(self.api, &self.model, response, &*self.prices))
        })
    }
}

/// Limen's reply for genai's `response` from `model` on `api`, its tokens
/// priced from `prices`.
fn reply(api: Api, model: &str, response: ChatResponse, prices: &dyn PriceList) -> Reply {
    let counted = |tokens: Option<i32>| tokens.and_then(|tokens| u64::try_from(tokens).ok());
    let usage = Usage {
        input_tokens: counted(response.usage.prompt_tokens),
        cached_input_tokens: counted(
            response
                .usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
        ),
        output_tokens: counted(response.usage.completion_tokens),
        cost_usd: None,
    };
    Reply {
        finish: response.stop_reason.as_ref().map(|reason| match reason {
            StopReason::Completed(_) => Finish::Stop,
            StopReason::MaxTokens(_) => Finish::Length,
            StopReason::ToolCall(_) => Finish::ToolCalls,
            StopReason::ContentFilter(_) => Finish::ContentFilter,
            // Limen sets no stop sequences; a reply stopped at one is not
            // the whole correction.
            StopReason::StopSequence(_) | StopReason::Other(_) => Finish::Other,
        }),
        text: response.into_first_text().unwrap_or_default(),
        usage: Usage {
            cost_usd: prices.price(api, model, &usage),
            ..usage
        },
    }
}

/// Limen's error for genai's. genai's messages quote the service's reply
/// and the request, which can echo the key, so none of them is kept: only
/// the HTTP status, and whether the failure may pass if tried again.
fn sanitized(error: genai::Error) -> anyhow::Error {
    match error.status().map(|status| status.as_u16()) {
        Some(status @ (408 | 429 | 500..=599)) => {
            Transient(format!("the service returned HTTP {status}")).into()
        }
        Some(status) => anyhow::anyhow!("the service returned HTTP {status}"),
        None => match error {
            genai::Error::WebModelCall { .. } | genai::Error::WebAdapterCall { .. } => {
                Transient("the service could not be reached".to_string()).into()
            }
            _ => anyhow::anyhow!("the service failed"),
        },
    }
}

#[cfg(test)]
mod tests;
