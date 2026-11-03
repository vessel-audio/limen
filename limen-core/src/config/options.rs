//! A provider's settings beyond its key and model: where its requests go
//! under the base URL, what they carry, and how long and how often they are
//! tried. Each provider keeps its own, so choosing another and back
//! restores them.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The settings of one provider; every one left unset keeps the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderOptions {
    /// The route requests go to under the base URL, such as `/v2/chat`, in
    /// place of the API's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The header the key is sent in, such as `api-key`, in place of the
    /// API's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_header: Option<String>,
    /// How long one try may take, in milliseconds. For polishing,
    /// `[polish].timeout_ms` still bounds every try together.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// How many times a request that failed for now, as when the service
    /// could not be reached or was busy, is tried again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// A JSON object whose fields every request body also carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Headers every request carries. The config file is not private, so
    /// a key belongs in the key field, sent in `key_header`, not here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// The longest one attempt may be allowed, in milliseconds.
const MAX_TIMEOUT_MS: u64 = 120_000;
/// The most retries a provider may ask for.
const MAX_RETRIES: u32 = 5;

impl ProviderOptions {
    /// The fields of [`Self::body`], once it is checked to be a JSON object.
    pub fn body_fields(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        match &self.body {
            None => Ok(serde_json::Map::new()),
            Some(body) => match serde_json::from_str(body).context("the extra body is not JSON")? {
                serde_json::Value::Object(fields) => Ok(fields),
                _ => bail!("the extra body must be a JSON object, such as {{\"top_p\": 0.9}}"),
            },
        }
    }

    /// Check every setting `field` names, such as `polish.options.custom`.
    pub(crate) fn check(&self, field: &str) -> Result<()> {
        if let Some(path) = &self.path
            && !(path.starts_with('/') && !path.contains(['?', '#']))
        {
            bail!("{field}.path must start with / and carry no query");
        }
        if let Some(header) = &self.key_header {
            reqwest::header::HeaderName::try_from(header.as_str())
                .with_context(|| format!("{field}.key_header is not a header name"))?;
        }
        if let Some(timeout) = self.timeout_ms
            && !(100..=MAX_TIMEOUT_MS).contains(&timeout)
        {
            bail!("{field}.timeout_ms must be in 100..={MAX_TIMEOUT_MS}");
        }
        if let Some(retries) = self.retries
            && retries > MAX_RETRIES
        {
            bail!("{field}.retries must be at most {MAX_RETRIES}");
        }
        self.body_fields()
            .with_context(|| format!("invalid {field}.body"))?;
        for (name, value) in &self.headers {
            reqwest::header::HeaderName::try_from(name.as_str())
                .with_context(|| format!("{field}.headers: {name:?} is not a header name"))?;
            reqwest::header::HeaderValue::try_from(value.as_str())
                .with_context(|| format!("{field}.headers.{name} is not a header value"))?;
        }
        Ok(())
    }
}
