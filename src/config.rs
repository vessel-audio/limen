//! Configuration loading, following the XDG Base Directory spec.
//!
//! | Purpose    | Path                                        |
//! |------------|---------------------------------------------|
//! | config     | `$XDG_CONFIG_HOME/voice-input/config.toml`  |
//! | IPC socket | `$XDG_RUNTIME_DIR/voice-input.sock`         |
//! | logs       | `$XDG_STATE_HOME/voice-input/daemon.log`    |
//! | cache      | `$XDG_CACHE_HOME/voice-input/`              |

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Written on first run; annotated so nothing relies on hidden defaults.
const DEFAULT_CONFIG: &str = r#"# voice-input configuration
# ASR backend (provider) section. Switching backends only touches this block.

[provider]
# "openai-realtime": WebSocket OpenAI Realtime protocol (sglang-omni /v1/realtime, OpenAI)
# "openai-batch":    POST /v1/audio/transcriptions; buffers a whole take, universal fallback
type = "openai-realtime"
url = "ws://127.0.0.1:8000/v1/realtime"
model = "Qwen/Qwen3-ASR-0.6B"
language = "auto"
# Read the API key from this environment variable; not needed for a local server
# api_key_env = "OPENAI_API_KEY"

[audio]
# pw-record source name; defaults to the system default input
# device = "default"
# Length of one audio push (milliseconds)
chunk_ms = 100
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
    pub fn discover() -> Result<Self> {
        let proj = ProjectDirs::from("dev", "voice-input", "voice-input")
            .context("cannot resolve user directories (HOME unset?)")?;
        let socket = dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("voice-input.sock");
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
pub struct Config {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub audio: AudioConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    pub url: String,
    pub model: String,
    #[serde(default = "default_language")]
    pub language: String,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenaiRealtime,
    OpenaiBatch,
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

impl Config {
    /// Load the config; write the annotated default first if none exists.
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            write_default(&paths.config_file)?;
        }
        let text = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("failed to read config: {}", paths.config_file.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("failed to parse config: {}", paths.config_file.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.provider.url.is_empty() {
            bail!("provider.url must not be empty");
        }
        // 0 would panic `chunks(0)` in the send path; keep the upper bound sane.
        if self.audio.chunk_ms == 0 || self.audio.chunk_ms > 1000 {
            bail!("audio.chunk_ms must be in 1..=1000");
        }
        match self.provider.kind {
            ProviderKind::OpenaiRealtime if !self.provider.url.starts_with("ws") => {
                bail!("openai-realtime url must start with ws:// or wss://")
            }
            ProviderKind::OpenaiBatch if !self.provider.url.starts_with("http") => {
                bail!("openai-batch url must start with http(s)://")
            }
            _ => Ok(()),
        }
    }

    pub fn api_key(&self) -> Option<String> {
        let var = self.provider.api_key_env.as_ref()?;
        std::env::var(var).ok().filter(|v| !v.is_empty())
    }
}

/// Atomic write: temp file + rename, so a crash never leaves a truncated config.
fn write_default(path: &Path) -> Result<()> {
    let dir = path.parent().context("config path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, DEFAULT_CONFIG)?;
    fs::rename(&tmp, path)?;
    eprintln!("wrote default config: {}", path.display());
    Ok(())
}
