//! `voice-input shortcut status` for gnome-keybinding and none.

use anyhow::Result;

use super::SetupError;
use crate::shortcut::Shortcut;
use crate::shortcut::gnome::gsettings::{self, Gsettings};
use crate::shortcut::gnome::{self as keybinding, CustomKeybinding, Managed};
use crate::shortcut::program::Program;

const NO_HOLD: &str =
    "not supported by this backend; for hold-to-talk run `voice-input shortcut setup portal`";

pub(super) fn status_none() {
    println!("backend:  none");
    println!("toggle:   bind `voice-input toggle` to a key in your desktop settings");
    println!("hold:     {NO_HOLD}");
}

pub(super) async fn status() -> Result<(), SetupError> {
    let bindings = gsettings::read(&Gsettings).await?;
    println!("backend:  gnome-keybinding");
    match keybinding::managed(&bindings.list, &bindings.entries) {
        Managed::Own(entry) | Managed::Adopted(entry) => println!("toggle:   {}", describe(entry)),
        Managed::Absent => {
            println!("toggle:   not bound; run `voice-input shortcut setup gnome-keybinding`")
        }
    }
    println!("hold:     {NO_HOLD}");
    Ok(())
}

fn describe(entry: &CustomKeybinding) -> String {
    let key = match entry.binding.as_str() {
        "" => "disabled".to_string(),
        binding => Shortcut::parse_accelerator(binding)
            .map_or_else(|_| binding.to_string(), |shortcut| shortcut.to_string()),
    };
    let program = keybinding::program_of(&entry.command).map(Program::of);
    format!(
        "{key} → {} ({})",
        entry.command,
        stability(program.as_ref())
    )
}

pub(super) fn stability(program: Option<&Program>) -> &'static str {
    match program {
        Some(Program::Stable(_)) => "stable path",
        Some(Program::Unstable(_)) => "Nix store path: breaks after an upgrade; run setup again",
        None => "unrecognized command",
    }
}
