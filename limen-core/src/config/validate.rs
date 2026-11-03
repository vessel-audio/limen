//! Checking a parsed config before anything uses it: the services and
//! formats each role can serve, the URLs a key may go to, and the bounds
//! of the numbers.

use anyhow::{Context, Result, bail};

use super::{Config, Format, Service, check_polish_root, check_polish_url, check_url_parameters};

impl Config {
    pub(super) fn validate(&self) -> Result<()> {
        let (service, format) = (self.provider.service(), self.provider.format());
        if !Service::TRANSCRIBING.contains(&service) {
            bail!(
                "provider.service \"{}\" does not transcribe audio",
                service.as_str()
            );
        }
        if !Format::TRANSCRIBING.contains(&format) {
            bail!(
                "provider.format \"{}\" does not transcribe audio",
                format.as_str()
            );
        }
        // An empty URL is a server not chosen yet, as in the default config;
        // a take then fails and says so.
        if service.uses_url() && !self.provider.url.is_empty() {
            let url = &self.provider.url;
            let provider_url = reqwest::Url::parse(url).context("invalid provider.url")?;
            match (provider_url.scheme(), provider_url.host_str()) {
                ("http" | "https", Some(_)) => {}
                _ => bail!("provider.url must be an http(s):// URL with a host"),
            }
            check_url_parameters(&provider_url, "provider.url")?;
            // Only the whisper.cpp upload goes to exactly the URL, query and
            // all; an API's routes hang off a base URL without one.
            match (
                format,
                crate::connection::custom_root(url, format, crate::connection::TRANSCRIPTIONS),
            ) {
                (Format::WhisperCpp, _) | (_, Some(_)) => {}
                (_, None) => bail!(
                    "provider.url must be the server's base URL, without a query; use format = \"whisper-cpp\" to upload to exactly this URL"
                ),
            }
        }
        for (role, options) in [
            ("provider", &self.provider.options),
            ("polish", &self.polish.options),
        ] {
            for (service, options) in options {
                options.check(&format!("{role}.options.{}", service.as_str()))?;
            }
        }
        // 0 would panic `chunks(0)` in the send path; keep the upper bound sane.
        if self.audio.chunk_ms == 0 || self.audio.chunk_ms > 1000 {
            bail!("audio.chunk_ms must be in 1..=1000");
        }
        if self.polish.enabled {
            let (service, format) = (self.polish.service(), self.polish.format());
            if !Service::POLISHING.contains(&service) {
                bail!(
                    "polish.service \"{}\" does not polish text",
                    service.as_str()
                );
            }
            if !Format::POLISHING.contains(&format) {
                bail!("polish.format \"{}\" does not polish text", format.as_str());
            }
            match (service.uses_url(), self.polish.url.trim().is_empty()) {
                (true, true) => bail!(
                    "polish.url must name the server when polishing is enabled; choose a service and model on the Polish page"
                ),
                (true, false) => {
                    check_polish_url(&self.polish.url)?;
                    // A config from before services and formats may name a
                    // route with a query; it loads, and its polisher keeps the
                    // originals.
                    if self.polish.service.is_some() || self.polish.format.is_some() {
                        check_polish_root(&self.polish.url, format)?;
                    }
                }
                (false, _) => {}
            }
            if self.polish.model.trim().is_empty() {
                bail!("polish.model must not be empty when polishing is enabled");
            }
            if !(100..=30_000).contains(&self.polish.timeout_ms) {
                bail!("polish.timeout_ms must be in 100..=30000");
            }
        }
        for (name, shortcut) in [
            ("toggle", self.shortcut.toggle),
            ("hold", self.shortcut.hold),
        ] {
            if let Err(error) = shortcut.validate() {
                bail!("shortcut.{name}: {error}");
            }
        }
        let toggle = self.shortcut.toggle;
        if toggle == self.shortcut.hold {
            bail!("shortcut.toggle and shortcut.hold must differ; both are {toggle}");
        }
        Ok(())
    }
}
