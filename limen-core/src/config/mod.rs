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
mod value;
mod write;

pub use keys::KeyChange;
pub(crate) use keys::saved_keys;
pub(crate) use value::{Key, Value, update_provider};
pub(crate) use write::{check_writable, snippet, update_shortcut};

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
url = "http://127.0.0.1:8000/v1/audio/transcriptions"
model = "Qwen/Qwen3-ASR-0.6B"
language = "auto"
# Read the API key from this environment variable; not needed for a local server
# api_key_env = "OPENAI_API_KEY"

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
# Conservative cloud correction for the final transcript of each take;
# audio is never uploaded.
enabled = false
url = "https://api.deepseek.com/chat/completions"
model = "deepseek-flash"
api_key_env = "DEEPSEEK_API_KEY"
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
    pub url: String,
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
    pub url: String,
    pub model: String,
    pub api_key_env: String,
    pub timeout_ms: u64,
    /// The key saved from the GUI, which comes before `api_key_env`.
    #[serde(skip)]
    pub saved_key: Option<ApiKey>,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "https://api.deepseek.com/chat/completions".to_string(),
            model: "deepseek-flash".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
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
    /// beside it; never creates either.
    pub fn load(path: &Path) -> Result<Self> {
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
        if self.provider.url.is_empty() {
            bail!("provider.url must not be empty");
        }
        let provider_url =
            reqwest::Url::parse(&self.provider.url).context("invalid provider.url")?;
        match (provider_url.scheme(), provider_url.host_str()) {
            ("http" | "https", Some(_)) => {}
            _ => bail!("provider.url must be an http(s):// URL with a host"),
        }
        check_url_parameters(&provider_url, "provider.url")?;
        // 0 would panic `chunks(0)` in the send path; keep the upper bound sane.
        if self.audio.chunk_ms == 0 || self.audio.chunk_ms > 1000 {
            bail!("audio.chunk_ms must be in 1..=1000");
        }
        if self.polish.enabled {
            check_polish_url(&self.polish.url)?;
            if self.polish.model.trim().is_empty() {
                bail!("polish.model must not be empty when polishing is enabled");
            }
            if self.polish.api_key_env.trim().is_empty() {
                bail!("polish.api_key_env must not be empty when polishing is enabled");
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
    /// `provider.api_key_env`.
    pub fn api_key(&self) -> Option<String> {
        match &self.provider.saved_key {
            Some(key) => Some(key.expose().to_string()),
            None => {
                let var = self.provider.api_key_env.as_ref()?;
                std::env::var(var).ok().filter(|v| !v.is_empty())
            }
        }
    }
}

impl PolishConfig {
    /// The polishing key: the one saved from the GUI, else the one in
    /// `api_key_env`.
    pub fn api_key(&self) -> Option<String> {
        match &self.saved_key {
            Some(key) => Some(key.expose().to_string()),
            None => std::env::var(&self.api_key_env)
                .ok()
                .filter(|key| !key.is_empty()),
        }
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
    use super::{Config, ShortcutConfig};
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
