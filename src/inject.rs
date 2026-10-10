//! Text injection into the focused window via `wtype`
//! (Wayland virtual-keyboard protocol, native Unicode).
//!
//! Safety rule baked in here: we never press Enter. Text lands in the input
//! box; sending it is always the human's decision.

use anyhow::{Context, Result};
use tokio::process::Command;

/// Stateless apart from the cross-segment spacing rule.
pub struct Injector {
    /// Whether the previously injected segment ended in an ASCII
    /// letter/digit — Latin words need a separating space, CJK runs don't.
    last_was_latin: bool,
}

impl Injector {
    pub fn new() -> Self {
        Self {
            last_was_latin: false,
        }
    }

    pub async fn type_segment(&mut self, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let needs_space = self.last_was_latin && text.chars().next().is_some_and(is_latin);
        let payload = if needs_space {
            format!(" {text}")
        } else {
            text.to_string()
        };
        type_text(&payload).await?;
        self.last_was_latin = text.chars().last().is_some_and(is_latin);
        Ok(())
    }
}

fn is_latin(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

async fn type_text(text: &str) -> Result<()> {
    let status = Command::new("wtype")
        .arg("--")
        .arg(text)
        .status()
        .await
        .context("failed to spawn wtype (is it installed? `nix shell nixpkgs#wtype`)")?;
    if !status.success() {
        anyhow::bail!("wtype exited with {status}");
    }
    Ok(())
}
