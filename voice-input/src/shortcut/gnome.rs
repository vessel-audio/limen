//! `voice-input shortcut status` for gnome-keybinding and none.

use anyhow::Result;

use super::SetupError;
use voice_input_core::{GnomeBinding, Program, Shortcut, gnome_status};

const NO_HOLD: &str =
    "not supported by this backend; for hold-to-talk run `voice-input shortcut setup portal`";

pub(super) fn status_none() {
    println!("backend:  none");
    println!("toggle:   bind `voice-input toggle` to a key in your desktop settings");
    println!("hold:     {NO_HOLD}");
}

pub(super) async fn status() -> Result<(), SetupError> {
    let status = gnome_status().await?;
    println!("backend:  gnome-keybinding");
    match status.toggle {
        Some(entry) => println!("toggle:   {}", describe(&entry)),
        None => {
            println!("toggle:   not bound; run `voice-input shortcut setup gnome-keybinding`")
        }
    }
    println!("hold:     {NO_HOLD}");
    Ok(())
}

fn describe(entry: &GnomeBinding) -> String {
    let key = match entry.binding.as_str() {
        "" => "disabled".to_string(),
        binding => Shortcut::parse_accelerator(binding)
            .map_or_else(|_| binding.to_string(), |shortcut| shortcut.to_string()),
    };
    format!(
        "{key} → {} ({})",
        entry.command,
        stability(entry.program.as_ref())
    )
}

pub(super) fn stability(program: Option<&Program>) -> &'static str {
    match program {
        Some(Program::Stable(_)) => "stable path",
        Some(Program::Unstable(_)) => "Nix store path: breaks after an upgrade; run setup again",
        None => "unrecognized command",
    }
}
