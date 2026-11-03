//! Checking a provider's endpoint before a key is sent to it, and showing
//! one without what it may carry.

use anyhow::{Context, Result, bail};

/// Check that `url` may carry the polishing API key: HTTPS, or HTTP on a
/// loopback address, without credentials of its own.
pub(crate) fn check_polish_url(url: &str) -> Result<()> {
    let polish_url = reqwest::Url::parse(url).context("invalid polish.url")?;
    let Some(polish_host) = polish_url.host_str() else {
        bail!("polish.url must include a host");
    };
    match polish_url.scheme() {
        "https" => {}
        "http" => {
            let is_loopback = polish_host.eq_ignore_ascii_case("localhost")
                || polish_host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback());
            if !is_loopback {
                bail!("polish.url must use HTTPS, or HTTP on a loopback address");
            }
        }
        _ => bail!("polish.url must use HTTPS, or HTTP on a loopback address"),
    }
    check_url_parameters(&polish_url, "polish.url")?;
    Ok(())
}

/// Check that `url` is the base URL of a polishing server speaking
/// `format`: one without a query, which no route under it can carry. Only
/// saves check this, so a config from before services, which may name a
/// route with a query, still loads; its polisher then keeps the original
/// transcripts.
pub(crate) fn check_polish_root(url: &str, format: super::Format) -> Result<()> {
    match crate::connection::custom_root(url, format, crate::connection::CHAT_COMPLETIONS) {
        Some(_) => Ok(()),
        None => bail!("polish.url must be the server's base URL, without a query"),
    }
}

/// Allow only endpoint query parameters that are routing or API metadata,
/// never credentials. An allowlist is deliberate: a denylist cannot cover
/// every name a provider might give a secret.
pub(crate) fn check_url_parameters(url: &reqwest::Url, field: &str) -> Result<()> {
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{field} must not contain credentials; use an API key instead");
    }
    let unsupported = url.query_pairs().any(|(name, _)| {
        let name = name.to_ascii_lowercase().replace(['-', '_'], "");
        !matches!(
            name.as_str(),
            "apiversion"
                | "version"
                | "deployment"
                | "deploymentid"
                | "model"
                | "project"
                | "projectid"
                | "organization"
                | "organizationid"
                | "region"
                | "location"
                | "tenant"
                | "tenantid"
                | "language"
                | "format"
        )
    });
    if unsupported {
        bail!(
            "{field} allows only non-secret query parameters such as api-version; use an API key instead"
        );
    }
    Ok(())
}

/// An endpoint safe to show in snapshots, logs, and errors: no userinfo,
/// query, or fragment. An invalid URL is never echoed back.
pub(crate) fn display_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.into()
        }
        Err(_) => "<invalid URL>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn displayed_endpoints_drop_credentials_queries_and_fragments() {
        assert_eq!(
            [
                super::display_url(
                    "https://user:pw@ai.example/v1/chat/completions?api_key=secret#token"
                ),
                super::display_url("https://user:pw@"),
            ],
            [
                "https://ai.example/v1/chat/completions".to_string(),
                "<invalid URL>".to_string()
            ]
        );
    }
}
