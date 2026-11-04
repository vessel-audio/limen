//! Configuration loading, following the XDG Base Directory spec.
//!
//! | Purpose    | Path                                 |
//! |------------|--------------------------------------|
//! | config     | `$XDG_CONFIG_HOME/limen/config.toml` |
//! | IPC socket | `$XDG_RUNTIME_DIR/limen.sock`        |
//! | logs       | `$XDG_STATE_HOME/limen/daemon.log`   |
//! | cache      | `$XDG_CACHE_HOME/limen/`             |

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::shortcut::{Backend, Shortcut};

mod endpoint;
mod keys;
mod network;
mod options;
mod service;
mod transaction;
mod validate;
mod value;
mod write;

pub(crate) use endpoint::{
    check_key_url, check_polish_root, check_polish_url, check_url_parameters, display_url,
};
pub use keys::KeyChange;
pub(crate) use keys::{Secret, saved_keys};
pub use network::{NetworkConfig, Proxy, ProxyLogin, ProxyMode};
pub use options::{ChatApi, ProviderOptions};
pub use service::{Format, Service};
pub(crate) use transaction::recover as recover_provider_save;
pub(crate) use value::{Key, Value, update};
pub(crate) use write::{check_writable, lock_dir, replace, snippet, update_shortcut};

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
#   "custom": your server at `url`, speaking `format`
#   "openai", "groq", "mistral", "gemini": the vendor's own API; `url` and
#            `format` are not used
service = "custom"
# What a custom server speaks:
#   "openai-compatible": OpenAI's routes under `url`, such as vLLM or
#                        sglang-omni at "http://127.0.0.1:8000/v1"
#   "whisper-cpp":       the take uploaded to exactly `url`, such as
#                        "http://127.0.0.1:8080/inference"
#   "openai", "gemini", "mistral", "groq": that API, with `url` as its
#                        base URL
format = "openai-compatible"
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
#   "custom": your server at `url`, speaking `format`
#   "openai", "anthropic", "gemini", "deepseek", "mistral", "groq",
#   "openrouter", "xai": the vendor's own API; `url` and `format` are not
#   used
#   "chatgpt", "copilot": your ChatGPT or GitHub Copilot plan, signed in to on
#   the Polish page; no key, `url`, or `format`
service = "custom"
# What a custom server speaks: "openai-compatible", such as llama.cpp,
# Ollama, or vLLM at "http://127.0.0.1:8080/v1", or any of the vendors'
# APIs above, with `url` as its base URL
format = "openai-compatible"
url = ""
model = ""
# Read the API key from this environment variable; a vendor's own variable,
# such as OPENAI_API_KEY, is read when unset
# api_key_env = "MY_POLISH_KEY"
timeout_ms = 5000
# Settings of one provider, here OpenAI; [provider] takes the same but
# `chat_api`, and a custom server's URL is `url` above
# [polish.options.openai]
# url = "https://gateway.example/v1"  # in place of the vendor's own
# chat_api = "chat-completions"  # or "responses", OpenAI's default
# key_header = "api-key"         # the header the key is sent in
# timeout_ms = 4000              # one try; timeout_ms above bounds them all
# retries = 1                    # tries again after a failure
# body = '{"top_p": 0.9}'        # fields every request body also carries
# headers = { X-Org = "limen" }  # never a key: this file is not private

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

[network]
# How requests to model services reach them:
#   "system": through the proxy HTTPS_PROXY, HTTP_PROXY, or ALL_PROXY names,
#             except to the hosts NO_PROXY lists
#   "none":   directly, whatever the environment names
#   "manual": through `proxy_url`, except to the hosts `no_proxy` lists
proxy = "system"
# An http://, https://, socks5://, or socks5h:// URL, such as
# "http://127.0.0.1:7890"; never with a login in it
proxy_url = ""
# Who signs in to the proxy; its password is saved from the GUI
# proxy_username = "me"
no_proxy = "localhost, 127.0.0.1, ::1"
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
    /// The directory a backend keeps subscription sign-ins in, beside the
    /// config file and as private as its keys.
    pub fn sign_ins(&self) -> PathBuf {
        self.config_file.with_file_name("sign-ins")
    }

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
    #[serde(default)]
    pub network: NetworkConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Left out by configs from before services; see [`Self::service`].
    pub service: Option<Service>,
    /// What a custom server speaks; see [`Self::format`].
    pub format: Option<Format>,
    /// A custom server's base URL, or its upload URL; unused by a vendor's
    /// own API.
    #[serde(default)]
    pub url: String,
    /// Empty until one is chosen; a take then fails and says so.
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_language")]
    pub language: String,
    pub api_key_env: Option<String>,
    /// Each provider's settings beyond its key and model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<Service, ProviderOptions>,
    /// The key saved from the GUI, which comes before `api_key_env`.
    #[serde(skip)]
    pub saved_key: Option<ApiKey>,
    /// How its requests go, as `[network]` and the proxy's saved password
    /// say.
    #[serde(skip)]
    pub proxy: Proxy,
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
    /// Left out by configs from before services; see [`Self::service`].
    pub service: Option<Service>,
    /// What a custom server speaks; see [`Self::format`].
    pub format: Option<Format>,
    /// A custom server's base URL; unused by a vendor's own API.
    pub url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    /// Each provider's settings beyond its key and model.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<Service, ProviderOptions>,
    /// The key saved from the GUI, which comes before `api_key_env`.
    #[serde(skip)]
    pub saved_key: Option<ApiKey>,
    /// How its requests go, as `[network]` and the proxy's saved password
    /// say.
    #[serde(skip)]
    pub proxy: Proxy,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service: None,
            format: None,
            url: String::new(),
            model: String::new(),
            api_key_env: None,
            timeout_ms: 5000,
            options: BTreeMap::new(),
            saved_key: None,
            proxy: Proxy::System,
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

    /// Read, parse, and validate the config file, with the API keys and
    /// proxy password saved beside it; never creates either. The directory
    /// lock keeps the config and keys on opposite sides of a save from being
    /// read together.
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
        config.route(keys.proxy);
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

    /// Send both roles' requests as `[network]` says, signing in to a
    /// manual proxy with `password`, the one saved from the GUI.
    pub(crate) fn route(&mut self, password: Option<String>) {
        let proxy = self.network.proxy(password.map(ApiKey::new));
        self.provider.proxy = proxy.clone();
        self.polish.proxy = proxy;
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
    /// The configured service; a config from before services named a
    /// custom server by its URL alone.
    pub fn service(&self) -> Service {
        self.service.unwrap_or(Service::Custom)
    }

    /// What a custom server speaks. A config from before formats named the
    /// server by its URL alone: an OpenAI-compatible server when it ends in
    /// `/audio/transcriptions`, and an upload to exactly that URL otherwise.
    pub fn format(&self) -> Format {
        match (
            self.format,
            crate::connection::base_url(&self.url, crate::connection::TRANSCRIPTIONS),
        ) {
            (Some(format), _) => format,
            (None, Some(_)) => Format::OpenAiCompatible,
            (None, None) => Format::WhisperCpp,
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
    /// The configured service. Configs from before services named a custom
    /// server by its URL; DeepSeek's own, the old
    /// default, is the DeepSeek service, whose requests turn its thinking
    /// off.
    pub fn service(&self) -> Service {
        let deepseek = reqwest::Url::parse(&self.url)
            .is_ok_and(|url| url.host_str() == Some("api.deepseek.com"));
        match (self.service, deepseek) {
            (Some(service), _) => service,
            (None, true) => Service::DeepSeek,
            (None, false) => Service::Custom,
        }
    }

    /// What a custom server speaks; configs from before formats all named
    /// an OpenAI-compatible one.
    pub fn format(&self) -> Format {
        self.format.unwrap_or(Format::OpenAiCompatible)
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

#[cfg(test)]
mod tests {
    use super::{Config, Format, Service, ShortcutConfig};
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

        config.polish.service = Some(Service::Custom);
        config.polish.format = Some(Format::WhisperCpp);
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "polish.format \"whisper-cpp\" does not polish text"
        );
    }

    #[test]
    fn only_a_whisper_cpp_upload_url_may_carry_a_query() {
        let mut config = base_config();
        config.provider.service = Some(Service::Custom);
        config.provider.format = Some(Format::OpenAiCompatible);
        config.provider.url = "http://127.0.0.1:8080/inference?language=zh".to_string();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("provider.url must be the server's base URL, without a query")
        );
        config.provider.format = Some(Format::WhisperCpp);
        config.validate().unwrap();
    }

    #[test]
    fn each_provider_keeps_its_own_options() {
        let config: Config = toml::from_str(
            r#"
            [provider]
            url = ""

            [polish.options.custom]
            chat_api = "responses"
            key_header = "api-key"
            body = '{"top_p": 0.9}'

            [polish.options.custom.headers]
            X-Org = "limen"

            [polish.options.anthropic]
            url = "https://gateway.example"
            timeout_ms = 8000
            "#,
        )
        .unwrap();
        config.validate().unwrap();

        assert_eq!(
            config.polish.options,
            [
                (
                    Service::Custom,
                    super::ProviderOptions {
                        chat_api: Some(super::ChatApi::Responses),
                        key_header: Some("api-key".to_string()),
                        body: Some(r#"{"top_p": 0.9}"#.to_string()),
                        headers: [("X-Org".to_string(), "limen".to_string())].into(),
                        ..Default::default()
                    }
                ),
                (
                    Service::Anthropic,
                    super::ProviderOptions {
                        url: Some("https://gateway.example".to_string()),
                        timeout_ms: Some(8000),
                        ..Default::default()
                    }
                ),
            ]
            .into()
        );
    }

    #[test]
    fn provider_options_are_checked() {
        let refused = |options: &str| {
            let config: Config = toml::from_str(&format!(
                "[provider]\nurl = \"\"\n\n[polish.options.custom]\n{options}\n"
            ))
            .unwrap();
            config.validate().unwrap_err().to_string()
        };
        assert_eq!(
            [
                "url = \"http://192.0.2.1/v1\"",
                "key_header = \"bad header\"",
                "timeout_ms = 50",
                "retries = 9",
                "body = '[1, 2]'",
                "headers = { \"X-Org\" = \"a\\nb\" }",
            ]
            .map(refused),
            [
                "polish.options.custom.url must use HTTPS, or HTTP on a loopback address",
                "polish.options.custom.key_header is not a header name",
                "polish.options.custom.timeout_ms must be in 100..=120000",
                "polish.options.custom.retries must be at most 5",
                "invalid polish.options.custom.body",
                "polish.options.custom.headers.X-Org is not a header value",
            ]
        );
    }

    #[test]
    fn a_setting_its_provider_cannot_use_is_refused() {
        let refused = |options: &str| {
            let config: Config =
                toml::from_str(&format!("[provider]\nurl = \"\"\n\n{options}\n")).unwrap();
            config.validate().unwrap_err().to_string()
        };
        assert_eq!(
            [
                "[polish.options.custom]\nurl = \"https://gateway.example/v1\"",
                "[provider.options.openai]\nchat_api = \"responses\"",
                "[polish.options.anthropic]\nchat_api = \"chat-completions\"",
                "[polish.options.chatgpt]\nurl = \"https://gateway.example/v1\"",
                "[polish.options.copilot]\nkey_header = \"api-key\"",
            ]
            .map(refused),
            [
                "polish.options.custom.url is not used: a custom server's URL is polish.url",
                "provider.options.openai.chat_api is not used: only polishing chats",
                "polish.options.anthropic.chat_api is not used: this API has only its own",
                "polish.options.chatgpt.url is not used: the sign-in names the server",
                "polish.options.copilot.key_header is not used: a subscription takes no key",
            ]
        );
    }

    #[test]
    fn a_custom_polishing_url_is_a_base_url_without_a_query() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.service = Some(Service::Custom);
        config.polish.model = "corrector".to_string();
        config.polish.url = "https://ai.example/v1?api-version=2026-01-01".to_string();
        let explicit_service = config.validate().map_err(|error| error.to_string());
        config.polish.service = None;
        config.polish.format = Some(Format::OpenAiCompatible);
        let explicit_format = config.validate().map_err(|error| error.to_string());
        let refused = Err("polish.url must be the server's base URL, without a query".to_string());
        assert_eq!(
            [explicit_service, explicit_format],
            [refused.clone(), refused]
        );
    }

    #[test]
    fn a_polish_route_the_backend_cannot_reach_still_loads() {
        let mut config = base_config();
        config.polish.enabled = true;
        config.polish.model = "corrector".to_string();
        config.polish.url =
            "https://ai.example/chat/completions?api-version=2026-01-01".to_string();
        config.validate().unwrap();
        assert_eq!(
            [
                "https://ai.example/chat/completions?api-version=2026-01-01",
                "https://ai.example/v1",
                "https://ai.example/v1/chat/completions",
            ]
            .map(|url| {
                super::check_polish_root(url, Format::OpenAiCompatible)
                    .map_err(|error| error.to_string())
            }),
            [
                Err("polish.url must be the server's base URL, without a query".to_string()),
                Ok(()),
                Ok(()),
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
