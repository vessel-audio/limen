//! Text injection into the focused window.
//!
//! Two backends:
//! - `dotool`: types through a kernel-level virtual keyboard (/dev/uinput).
//!   Works on every Wayland compositor and every app, hardware-style input.
//!   Needs write access to /dev/uinput (udev rule + group).
//! - `wtype`: Wayland virtual-keyboard protocol. NOT supported by GNOME
//!   Mutter (that was learned the hard way); kept for wlroots/KDE.
//!
//! Safety rule baked in here: we never press Enter. Text lands in the input
//! box; sending it is always the human's decision.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Dotool,
    Wtype,
}

/// Stateless apart from the cross-segment spacing rule.
pub struct Injector {
    backend: Backend,
    /// dotool stays running and reads commands on stdin; respawned on error.
    dotool_stdin: Option<ChildStdin>,
    dotool_child: Option<Child>,
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
            last_was_latin: false,
        }
    }

    pub async fn type_segment(&mut self, text: &str) -> Result<()> {
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
                if needs_space {
                    self.dotool(&["key space"]).await?;
                }
                self.dotool(&[&format!("type {text}")]).await?;
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
                .stderr(std::process::Stdio::null())
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

/// Pick a backend: dotool when /dev/uinput is writable, else wtype.
/// GNOME users without uinput perms get a clear error either way.
pub fn autodetect(configured: Option<Backend>) -> Backend {
    if let Some(b) = configured {
        return b;
    }
    if std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok()
    {
        Backend::Dotool
    } else {
        Backend::Wtype
    }
}
