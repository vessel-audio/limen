//! Text injection into the focused window.
//!
//! Backends:
//! - `dotool`: types through a kernel-level virtual keyboard (/dev/uinput).
//!   Works on every Wayland compositor and every app, hardware-style input.
//!   Needs write access to /dev/uinput (udev rule + group).
//! - `wtype`: Wayland virtual-keyboard protocol. NOT supported by GNOME
//!   Mutter (that was learned the hard way); kept for wlroots/KDE.
//! - `fcitx5`: commits Unicode text directly to the focused Fcitx input context.
//!
//! Safety rule baked in here: we never press Enter. Text lands in the input
//! box; sending it is always the human's decision.

use std::borrow::Cow;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};

const FCITX_SERVICE: &str = "org.fcitx.Fcitx5";
const FCITX_PATH: &str = "/org/freedesktop/Fcitx5/VoiceInput";
const FCITX_INTERFACE: &str = "org.fcitx.Fcitx5.VoiceInput1";
const FCITX_COMMIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Dotool,
    Wtype,
    Fcitx5,
}

/// Stateless apart from the cross-segment spacing rule.
pub struct Injector {
    backend: Backend,
    /// dotool stays running and reads commands on stdin; respawned on error.
    dotool_stdin: Option<ChildStdin>,
    dotool_child: Option<Child>,
    fcitx_connection: Option<zbus::Connection>,
    /// Whether the previously injected segment ended in an ASCII
    /// letter/digit — Latin words need a separating space, CJK runs don't.
    last_was_latin: bool,
}

impl Injector {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            dotool_stdin: None,
            dotool_child: None,
            fcitx_connection: None,
            last_was_latin: false,
        }
    }

    pub fn reset_connections(&mut self) {
        self.dotool_stdin = None;
        self.dotool_child = None;
        self.fcitx_connection = None;
    }

    pub async fn type_segment(&mut self, text: &str) -> Result<()> {
        let text = sanitize_control_characters(text);
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let needs_space = self.last_was_latin && text.chars().next().is_some_and(is_latin);
        match self.backend {
            Backend::Wtype => {
                let payload = if needs_space {
                    format!(" {text}")
                } else {
                    text.to_string()
                };
                wtype(&payload).await?;
            }
            Backend::Dotool => {
                if !text.is_ascii() {
                    bail!("dotool only supports ASCII; configure the fcitx5 backend for Unicode");
                }
                if needs_space {
                    self.dotool(&["key space"]).await?;
                }
                self.dotool(&[&format!("type {text}")]).await?;
            }
            Backend::Fcitx5 => {
                let payload = if needs_space {
                    format!(" {text}")
                } else {
                    text.to_string()
                };
                self.fcitx_commit(&payload).await?;
            }
        }
        self.last_was_latin = text.chars().last().is_some_and(is_latin);
        Ok(())
    }

    /// Feed command lines to the persistent dotool process, respawning it if
    /// the previous one died or was never started.
    async fn dotool(&mut self, commands: &[&str]) -> Result<()> {
        if self.dotool_stdin.is_none() {
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .context(
                    "failed to spawn dotool (install it and make /dev/uinput writable for your user)",
                )?;
            self.dotool_stdin = Some(child.stdin.take().context("dotool stdin")?);
            self.dotool_child = Some(child);
        }
        let stdin = self.dotool_stdin.as_mut().unwrap();
        for cmd in commands {
            if let Err(e) = stdin.write_all(cmd.as_bytes()).await {
                // The process died (uinput perms lost, killed, ...): drop it
                // and fail this segment; the next one respawns.
                self.dotool_stdin = None;
                self.dotool_child = None;
                return Err(e).context("dotool write failed (check /dev/uinput permissions)");
            }
            stdin.write_all(b"\n").await?;
        }
        stdin.flush().await?;
        Ok(())
    }

    async fn fcitx_commit(&mut self, text: &str) -> Result<()> {
        if self.fcitx_connection.is_none() {
            self.fcitx_connection = Some(
                zbus::Connection::session()
                    .await
                    .context("failed to connect to the session D-Bus")?,
            );
        }

        let connection = self.fcitx_connection.as_ref().unwrap();
        let request = async {
            let proxy =
                zbus::Proxy::new(connection, FCITX_SERVICE, FCITX_PATH, FCITX_INTERFACE).await?;
            let committed: bool = proxy.call("CommitText", &(text,)).await?;
            Ok::<bool, zbus::Error>(committed)
        };

        match tokio::time::timeout(FCITX_COMMIT_TIMEOUT, request).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => bail!("Fcitx rejected the text: no focused input context"),
            Ok(Err(error)) => {
                self.fcitx_connection = None;
                Err(error).context("Fcitx CommitText failed")
            }
            Err(_) => {
                self.fcitx_connection = None;
                bail!("Fcitx CommitText timed out")
            }
        }
    }
}

fn sanitize_control_characters(text: &str) -> Cow<'_, str> {
    let needs_sanitizing = text
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'));
    if needs_sanitizing {
        Cow::Owned(
            text.chars()
                .map(|character| {
                    if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
                        ' '
                    } else {
                        character
                    }
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

fn is_latin(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

async fn wtype(text: &str) -> Result<()> {
    let status = Command::new("wtype")
        .arg("--")
        .arg(text)
        .status()
        .await
        .context("failed to spawn wtype (is it installed?)")?;
    if !status.success() {
        bail!("wtype exited with {status}");
    }
    Ok(())
}

/// Use Fcitx unless a legacy backend is explicitly configured.
pub fn autodetect(configured: Option<Backend>) -> Backend {
    configured.unwrap_or(Backend::Fcitx5)
}

#[cfg(test)]
mod tests {
    use super::{Backend, Injector, autodetect, sanitize_control_characters};

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(
            sanitize_control_characters("hello\nworld\t!"),
            "hello world !"
        );
        assert_eq!(sanitize_control_characters("a\u{2028}b\u{2029}c"), "a b c");
    }

    #[test]
    fn unicode_text_is_unchanged() {
        let text = "hello，测试🙂";
        assert_eq!(sanitize_control_characters(text), text);
    }

    #[test]
    fn fcitx_is_the_default_backend() {
        assert_eq!(autodetect(None), Backend::Fcitx5);
        assert_eq!(autodetect(Some(Backend::Dotool)), Backend::Dotool);
    }

    #[tokio::test]
    async fn dotool_rejects_unicode_before_starting_a_process() {
        let mut injector = Injector::new(Backend::Dotool);
        injector.last_was_latin = true;
        let error = injector.type_segment("你好").await.unwrap_err();
        assert!(error.to_string().contains("Unicode"));
        assert!(injector.dotool_stdin.is_none());
    }
}
