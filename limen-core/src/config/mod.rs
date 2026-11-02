//! Configuration loading, following the XDG Base Directory spec.
//!
//! | Purpose    | Path                                 |
//! |------------|--------------------------------------|
//! | config     | `$XDG_CONFIG_HOME/limen/config.toml` |
//! | IPC socket | `$XDG_RUNTIME_DIR/limen.sock`        |
//! | logs       | `$XDG_STATE_HOME/limen/daemon.log`   |
//! | cache      | `$XDG_CACHE_HOME/limen/`             |

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::shortcut::{Backend, Shortcut};

mod keys;
mod service;
mod transaction;
mod value;
mod write;

pub use keys::KeyChange;
pub(crate) use keys::saved_keys;
pub use service::Service;
pub(crate) use transaction::recover as recover_provider_save;
pub(crate) use value::{Key, Value, update_provider};
pub(crate) use write::{check_writable, lock_dir, snippet, update_shortcut};

/// An API key saved from the GUI. Its `Debug` output never shows the key,
/// so configs and commands that hold one stay safe to log.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: String) -> Self {
        Self(key)
    }

    /// The key itself, for the request that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey(…)")
    }
}

/// Written on first run; annotated so nothing relies on hidden defaults.
const DEFAULT_CONFIG: &str = r#"# Limen configuration
# ASR backend: one buffered take per HTTP transcription request.

[provider]
# Who transcribes each take:
#   "openai-compatible": an OpenAI-compatible server at `url`, such as
#                        vLLM or sglang-omni
#   "whisper-cpp":       the whisper.cpp server, or any server that takes
#                        the upload at exactly `url`
#   "openai", "groq", "mistral", "gemini": the vendor's own API; `url` is
#                        not used
service = "openai-compatible"
# e.g. "http://127.0.0.1:8000/v1/audio/transcriptions"
url = ""
# One of the models the server lists; the ASR page offers them
model = ""
language = "auto"
# Read the API key from this environment variable; not needed for a local
# server. A vendor's own variable, such as OPENAI_API_KEY, is read when unset
# api_key_env = "MY_ASR_KEY"

[audio]
# pw-record source name; defaults to the system default input
# device = "default"
# Length of one audio push (milliseconds)
chunk_ms = 100

[input_method]
# Input method backend:
#   "fcitx5": direct Unicode commit through the Limen Fcitx5 addon
#   "dotool": kernel virtual keyboard via /dev/uinput — works on GNOME,
#             needs your user to have write access to /dev/uinput
#   "wtype":  Wayland virtual-keyboard protocol — wlroots/KDE only,
#             GNOME Mutter does NOT support it
# backend = "fcitx5"   # default: direct Unicode commit through Fcitx5

[polish]
# Conservative correction of the final transcript of each take by a chat
# model of your choice; audio is never sent to it.
enabled = false
# Who corrects it:
#   "openai-compatible": a chat-completions server at `url`, such as
#                        llama.cpp, Ollama, or vLLM
#   "openai", "anthropic", "gemini", "deepseek", "mistral", "groq",
#   "openrouter", "xai": the vendor's own API; `url` is not used
service = "openai-compatible"
# e.g. "http://127.0.0.1:8080/v1/chat/completions"
url = ""
model = ""
# Read the API key from this environment variable; a vendor's own variable,
# such as OPENAI_API_KEY, is read when unset
# api_key_env = "MY_POLISH_KEY"
timeout_ms = 5000

[shortcut]
# How global shortcuts are bound; set up with `limen shortcut setup <backend>`:
#   "portal":           recommended; supports hold-to-talk. The running daemon
#                       asks the desktop for global shortcuts
#   "gnome-keybinding": toggle only; a GNOME custom shortcut runs `limen toggle`
#   "none":             bind `limen toggle` in your desktop settings yourself
backend = "none"
# portal: prefilled when GNOME first asks; GNOME Settings decides afterwards
# gnome-keybinding: the key that `setup` / `set` write to GNOME
toggle = "Super+Z"
# Hold to talk; portal only
hold = "Super+X"
"#;

/// All resolved XDG paths.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub socket: PathBuf,
    // Used once the daemon gets file logging / a cache; part of the XDG
    // contract already, so resolve them eagerly.
    #[allow(dead_code)]
    pub log_file: PathBuf,
    #[allow(dead_code)]
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Resolve socket, log, cache, and config paths from the user's XDG directories.
    pub fn discover() -> Result<Self> {
        let proj = ProjectDirs::from("dev", "limen", "limen")
            .context("cannot resolve user directories (HOME unset?)")?;
        let socket = dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("limen.sock");
        Ok(Self {
            config_file: proj.config_dir().join("config.toml"),
            socket,
            log_file: proj
                .state_dir()
                .map(|d| d.join("daemon.log"))
                .unwrap_or_else(|| proj.cache_dir().join("daemon.log")),
            cache_dir: proj.cache_dir().to_path_buf(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Validated daemon configuration loaded from the user's TOML config file.
pub struct Config {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub input_method: InputMethodConfig,
    #[serde(default)]
    pub polish: PolishConfig,
    #[serde(default)]
    pub shortcut: ShortcutConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Left out by configs from before services; see [`Self::service`].
    pub service: Option<Service>,
    /// The server's URL; unused by a vendor's own API.
    #[serde(default)]
    pub url: String,
    /// Empty until one is chosen; a take then fails and says so.
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_language")]
    pub language: String,
    pub api_key_env: Option<String>,
    /// The key saved from the GUI, which comes before `api_key_env`.
    #[serde(skip)]
    pub saved_key: Option<ApiKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub device: Option<String>,
    #[serde(default = "default_chunk_ms")]
    pub chunk_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: None,
            chunk_ms: default_chunk_ms(),
        }
    }
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_chunk_ms() -> u32 {
    100
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputMethodConfig {
    pub backend: Option<crate::input_method::Backend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolishConfig {
    pub enabled: bool,
    /// Left out by configs from before services, which all used an
    /// OpenAI-compatible server.
    pub service: Option<Service>,
    /// The chat-completions URL; unused by a vendor's own API.
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    /// The key saved from the GUI, which comes before `api_key_env`.
    #[serde(skip)]
    pub saved_key: Option<ApiKey>,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service: None,
            url: String::new(),
            model: String::new(),
            api_key_env: None,
            timeout_ms: 5000,
            saved_key: None,
        }
    }
}

/// How global shortcuts are bound; see [`crate::shortcut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutConfig {
    pub backend: Backend,
    /// portal: the key GNOME prefills when it first asks; gnome-keybinding:
    /// the key written to GNOME.
    pub toggle: Shortcut,
    /// Hold to talk; used by the portal backend only.
    pub hold: Shortcut,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            backend: Backend::None,
            toggle: Shortcut::DEFAULT_TOGGLE,
            hold: Shortcut::DEFAULT_HOLD,
        }
    }
}

impl Config {
    /// Load the config; write the annotated default first if none exists.
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        let path = &paths.config_file;
        {
            let _lock = write::lock_dir(path)?;
            transaction::recover(path)?;
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => write::create_default(path)?,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read config: {}", path.display()));
            }
            // Replacing a dangling link with the default config would cut it off from
            // whatever manages it.
            Ok(metadata) if metadata.file_type().is_symlink() && !path.exists() => {
                bail!("config {} is a symlink to a missing file", path.display());
            }
            Ok(_) => {}
        }
        Self::load(path)
    }

    /// Read, parse, and validate the config file, with the API keys saved
    /// beside it; never creates either. The directory lock keeps the config
    /// and keys on opposite sides of a save from being read together.
    pub fn load(path: &Path) -> Result<Self> {
        let _lock = write::lock_dir(path)?;
        transaction::recover(path)?;
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let mut config =
            Self::parse(&text).with_context(|| format!("invalid config: {}", path.display()))?;
        let keys = saved_keys(path)?;
        config.provider.saved_key = keys.asr.map(ApiKey::new);
        config.polish.saved_key = keys.polish.map(ApiKey::new);
        Ok(config)
    }

    /// Read and parse the config file without validating it or reading its
    /// keys, so a config the GUI is about to repair can still be shown. A
    /// missing file reads as the annotated default.
    pub(crate) fn read_unchecked(path: &Path) -> Result<Self> {
        let (text, _) = write::read_text(path)?;
        toml::from_str(&text).with_context(|| format!("failed to parse config: {}", path.display()))
    }

    /// Parse and validate the text of a config file.
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let service = self.provider.service();
        if !Service::TRANSCRIBING.contains(&service) {
            bail!(
                "provider.service \"{}\" does not transcribe audio",
                service.as_str()
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
            let route = crate::connection::TRANSCRIPTIONS;
            if service == Service::OpenAiCompatible
                && crate::connection::base_url(url, route).is_none()
            {
                bail!(
                    "provider.url must end in {route}, without a query, for an OpenAI-compatible server; use service = \"whisper-cpp\" to upload to exactly this URL"
                );
            }
        }
        // 0 would panic `chunks(0)` in the send path; keep the upper bound sane.
        if self.audio.chunk_ms == 0 || self.audio.chunk_ms > 1000 {
            bail!("audio.chunk_ms must be in 1..=1000");
        }
        if self.polish.enabled {
            let service = self.polish.service();
            if !Service::POLISHING.contains(&service) {
                bail!(
                    "polish.service \"{}\" does not polish text",
                    service.as_str()
                );
            }
            match (service.uses_url(), self.polish.url.trim().is_empty()) {
                (true, true) => bail!(
                    "polish.url must name the server when polishing is enabled; choose a service and model on the Polish page"
                ),
                (true, false) => check_polish_url(&self.polish.url)?,
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

    /// The ASR key: the one saved from the GUI, else the one in
    /// [`ProviderConfig::key_env`].
    pub fn api_key(&self) -> Option<String> {
        key(&self.provider.saved_key, self.provider.key_env())
    }
}

/// `saved`, else the non-empty value of the variable `env`.
fn key(saved: &Option<ApiKey>, env: Option<&str>) -> Option<String> {
    match saved {
        Some(key) => Some(key.expose().to_string()),
        None => std::env::var(env?).ok().filter(|key| !key.is_empty()),
    }
}

impl ProviderConfig {
    /// The configured service; a config from before services named the
    /// server by its URL alone, which is an OpenAI-compatible server when
    /// it ends in `/audio/transcriptions` and an upload to exactly that URL
    /// otherwise.
    pub fn service(&self) -> Service {
        match (
            self.service,
            crate::connection::base_url(&self.url, crate::connection::TRANSCRIPTIONS),
        ) {
            (Some(service), _) => service,
            (None, Some(_)) => Service::OpenAiCompatible,
            (None, None) => Service::WhisperCpp,
        }
    }

    /// The variable the key is read from: `api_key_env`, else the
    /// service's own.
    pub fn key_env(&self) -> Option<&str> {
        self.api_key_env
            .as_deref()
            .or_else(|| self.service().key_env())
    }
}

impl PolishConfig {
    /// The configured service. Configs from before services named an
    /// OpenAI-compatible server by its URL; DeepSeek's own, the old
    /// default, is the DeepSeek service, whose requests turn its thinking
    /// off.
    pub fn service(&self) -> Service {
        let deepseek = reqwest::Url::parse(&self.url)
            .is_ok_and(|url| url.host_str() == Some("api.deepseek.com"));
        match (self.service, deepseek) {
            (Some(service), _) => service,
            (None, true) => Service::DeepSeek,
            (None, false) => Service::OpenAiCompatible,
        }
    }

    /// The variable the key is read from: `api_key_env`, else the
    /// service's own. A config from before services read
    /// `DEEPSEEK_API_KEY`, the old default, whatever its server.
    pub fn key_env(&self) -> Option<&str> {
        let legacy = self.service.is_none().then_some("DEEPSEEK_API_KEY");
        self.api_key_env
            .as_deref()
            .or_else(|| self.service().key_env())
            .or(legacy)
    }

    /// The polishing key: the one saved from the GUI, else the one in
    /// [`Self::key_env`].
    pub fn api_key(&self) -> Option<String> {
        key(&self.saved_key, self.key_env())
    }
}

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

/// Check that `url` is a chat-completions route the backend can reach: one
/// ending in `/chat/completions`, without a query. Only saves check this,
/// so a config from before services, which may name another route, still
/// loads; its polisher then keeps the original transcripts.
pub(crate) fn check_polish_route(url: &str) -> Result<()> {
    match crate::connection::base_url(url, crate::connection::CHAT_COMPLETIONS) {
        Some(_) => Ok(()),
        None => bail!(
            "polish.url must end in {}, without a query",
            crate::connection::CHAT_COMPLETIONS
        ),
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
    use super::{Config, Service, ShortcutConfig};
    use crate::shortcut::Backend;

    fn base_config() -> Config {
        toml::from_str(
            r#"
            [provider]
            url = "http://127.0.0.1:8000/v1/audio/transcriptions"
            model = "test-model"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn generated_default_config_is_valid() {
        let config: Config = toml::from_str(super::DEFAULT_CONFIG).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn missing_polish_section_is_disabled_and_backward_compatible() {
        let config = base_config();
        assert!(!config.polish.enabled);
        config.validate().unwrap();
    }

    #[test]
    fn enabled_polish_requires_secure_transport_without_credentials() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.url = "http://192.0.2.1:8000/chat/completions".to_string();
        assert!(config.validate().is_err());
        config.polish.url = "https://user:pw@api.deepseek.com/chat/completions".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn endpoint_queries_allow_versions_but_not_credentials() {
        let mut config = base_config();
        config.polish.model = "corrector".to_string();
        for parameter in [
            "api_key",
            "token",
            "X-API-Key",
            "clientsecret",
            "auth_token",
            "access_key",
            "api-token",
            "unrecognised",
        ] {
            config.provider.url =
                format!("https://asr.example/v1/audio/transcriptions?{parameter}=do-not-echo");
            assert!(config.validate().is_err(), "{parameter}");
            config.polish.enabled = true;
            config.polish.url =
                format!("https://ai.example/chat/completions?{parameter}=do-not-echo");
            assert!(config.validate().is_err(), "{parameter}");
        }
        config.provider.url =
            "https://asr.example/v1/audio/transcriptions?api-version=2026-01-01".to_string();
        config.polish.url =
            "https://ai.example/chat/completions?api-version=2026-01-01".to_string();
        config.validate().unwrap();
    }

    #[test]
    fn enabled_polishing_without_a_server_says_so() {
        let mut config = base_config();
        config.polish.enabled = true;
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "polish.url must name the server when polishing is enabled; choose a service and model on the Polish page"
        );
    }

    #[test]
    fn a_vendor_needs_no_url_but_must_serve_its_role() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.service = Some(Service::Anthropic);
        config.polish.model = "corrector".to_string();
        config.validate().unwrap();

        config.provider.service = Some(Service::Anthropic);
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "provider.service \"anthropic\" does not transcribe audio"
        );
        config.provider.service = Some(Service::Groq);
        config.provider.url = String::new();
        config.validate().unwrap();

        config.polish.service = Some(Service::WhisperCpp);
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "polish.service \"whisper-cpp\" does not polish text"
        );
    }

    #[test]
    fn a_compatible_asr_server_needs_its_transcriptions_route() {
        let mut config = base_config();
        config.provider.service = Some(Service::OpenAiCompatible);
        config.provider.url = "http://127.0.0.1:8080/inference".to_string();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("provider.url must end in /audio/transcriptions")
        );
        config.provider.service = Some(Service::WhisperCpp);
        config.validate().unwrap();
    }

    #[test]
    fn a_polish_route_the_backend_cannot_reach_still_loads() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.model = "corrector".to_string();
        config.polish.url = "https://ai.example/v1/completions".to_string();
        config.validate().unwrap();
        assert_eq!(
            [
                "https://ai.example/v1/completions",
                "https://ai.example/chat/completions?api-version=2026-01-01",
                "https://ai.example/v1/chat/completions",
            ]
            .map(|url| super::check_polish_route(url).map_err(|error| error.to_string())),
            [
                Err("polish.url must end in /chat/completions, without a query".to_string()),
                Err("polish.url must end in /chat/completions, without a query".to_string()),
                Ok(()),
            ]
        );
    }

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

    #[test]
    fn loopback_http_is_allowed_for_local_polishing() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.model = "corrector".to_string();
        for url in [
            "http://127.0.0.1:3425/v1/chat/completions",
            "http://localhost:3425/v1/chat/completions",
            "http://[::1]:3425/v1/chat/completions",
        ] {
            config.polish.url = url.to_string();
            config.validate().unwrap();
        }
    }

    #[test]
    fn enabled_polish_requires_a_bounded_timeout() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.service = Some(Service::OpenAi);
        config.polish.model = "corrector".to_string();
        config.polish.timeout_ms = 0;
        assert!(config.validate().is_err());
        config.polish.timeout_ms = 30_001;
        assert!(config.validate().is_err());
        config.polish.timeout_ms = 5000;
        config.validate().unwrap();
    }

    #[test]
    fn missing_shortcut_section_leaves_shortcuts_to_the_user() {
        assert_eq!(
            base_config().shortcut,
            ShortcutConfig {
                backend: Backend::None,
                toggle: "Super+Z".parse().unwrap(),
                hold: "Super+X".parse().unwrap(),
            }
        );
    }

    #[test]
    fn invalid_shortcuts_fail_to_load_and_name_their_key() {
        let config: Config = toml::from_str(
            r#"
            [provider]
            url = "http://127.0.0.1:8000/v1/audio/transcriptions"
            model = "test-model"

            [shortcut]
            backend = "portal"
            toggle = "Shift+A"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "shortcut.toggle: Shift+A would get in the way of typing; add Super, Ctrl, or Alt"
        );
    }

    #[test]
    fn toggle_and_hold_must_differ() {
        let mut config = base_config();
        config.shortcut.hold = config.shortcut.toggle;
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "shortcut.toggle and shortcut.hold must differ; both are Super+Z"
        );
    }
}
