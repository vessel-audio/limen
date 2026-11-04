//! What is left to spend at a provider, for the services whose ordinary
//! API keys may ask: DeepSeek reports the account's balance, OpenRouter
//! what is left of the key's spending limit, or what the key has spent.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::backend::Api;
use crate::config::{Config, Paths, Service};
use crate::connection::{self, AsrRoute};
use crate::models::ModelRole;

/// How long a provider may take to report its balance.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a provider reports is left to spend.
#[derive(Debug, Clone, PartialEq)]
pub enum Balance {
    /// Money left to spend, in each currency the account holds.
    Remaining(Vec<Money>),
    /// What the key has spent, for a key with no spending limit.
    Spent(Money),
}

/// An amount as the provider wrote it, in `currency`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Money {
    pub currency: String,
    pub amount: String,
}

/// The saved provider of `role`, and what it reports is left to spend with
/// its saved key, or why it could not be asked; `None` where there is
/// nothing to ask: a config that cannot be read, which the Providers page
/// reports, a provider not set up yet, one that does not say, or one
/// reached at a URL other than its own, which may not.
pub async fn balance(paths: &Paths, role: ModelRole) -> Option<(Service, Result<Balance>)> {
    let file = paths.config_file.clone();
    let config = tokio::task::spawn_blocking(move || Config::load(&file))
        .await
        .ok()?
        .ok()?;
    let (service, connection) = match role {
        ModelRole::Asr => match connection::asr(&config).ok()? {
            AsrRoute::Api(connection) => (config.provider.service(), connection),
            AsrRoute::Upload(..) => return None,
        },
        ModelRole::Polish => (
            config.polish.service(),
            connection::polish(&config.polish, config.polish.api_key()).ok()?,
        ),
    };
    let ask = match (connection.api, connection.base_url, connection.api_key) {
        (Api::DeepSeek, None, Some(key)) => Ask::DeepSeek(key),
        (Api::OpenRouter, None, Some(key)) => Ask::OpenRouter(key),
        (
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
            | Api::Copilot
            | Api::Together
            | Api::Fireworks
            | Api::Cohere
            | Api::Kimi
            | Api::Zai
            | Api::Qwen
            | Api::MiniMax
            | Api::Nebius,
            _,
            _,
        ) => return None,
    };
    let asked = async {
        let http = connection
            .proxy
            .route(reqwest::Client::builder().timeout(TIMEOUT))?
            .build()
            .context("failed to build the balance HTTP client")?;
        match ask {
            Ask::DeepSeek(key) => deepseek(&http, &key).await,
            Ask::OpenRouter(key) => openrouter(&http, &key).await,
        }
    };
    Some((service, asked.await))
}

/// A service that reports a balance, and the key to ask with.
enum Ask {
    DeepSeek(String),
    OpenRouter(String),
}

/// The JSON `url` answers to `key`. Errors name the service, never what it
/// sent back, which can echo the key.
async fn ask<T: for<'de> Deserialize<'de>>(
    http: &reqwest::Client,
    service: &str,
    url: &str,
    key: &str,
) -> Result<T> {
    let response = http
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("{service} could not be reached for its balance"))?;
    match response.status() {
        status if status.is_success() => response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("{service} sent a balance Limen cannot read")),
        status => bail!("{service} refused to report its balance ({status})"),
    }
}

async fn deepseek(http: &reqwest::Client, key: &str) -> Result<Balance> {
    #[derive(Deserialize)]
    struct Reply {
        balance_infos: Vec<Info>,
    }
    #[derive(Deserialize)]
    struct Info {
        currency: String,
        total_balance: String,
    }
    let reply: Reply = ask(
        http,
        "DeepSeek",
        "https://api.deepseek.com/user/balance",
        key,
    )
    .await?;
    Ok(Balance::Remaining(
        reply
            .balance_infos
            .into_iter()
            .map(|info| Money {
                currency: info.currency,
                amount: info.total_balance,
            })
            .collect(),
    ))
}

async fn openrouter(http: &reqwest::Client, key: &str) -> Result<Balance> {
    #[derive(Deserialize)]
    struct Reply {
        data: Key,
    }
    #[derive(Deserialize)]
    struct Key {
        limit_remaining: Option<f64>,
        usage: f64,
    }
    let reply: Reply = ask(http, "OpenRouter", "https://openrouter.ai/api/v1/key", key).await?;
    let usd = |amount: f64| Money {
        currency: "USD".to_string(),
        amount: format!("{amount:.2}"),
    };
    Ok(match reply.data.limit_remaining {
        Some(left) => Balance::Remaining(vec![usd(left)]),
        None => Balance::Spent(usd(reply.data.usage)),
    })
}
