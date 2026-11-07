//! Convert a [`Shortcut`] into a global-hotkey [`HotKey`] for Carbon.
//!
//! macOS note: `Super` is Cmd (`Modifiers::META`); Ctrl and Alt stay
//! CONTROL and OPTION. The conversion accepts the same letter/digit/function/
//! named-key set the portal does, so a config moves between platforms
//! unchanged.

use global_hotkey::hotkey::{Code, HotKey};

use crate::shortcut::status::Trigger;
use crate::shortcut::{Key, Modifier, Shortcut, ShortcutError};

/// The Carbon hotkey for `shortcut`.
pub(super) fn hotkey(shortcut: Shortcut) -> Result<HotKey, ShortcutError> {
    let modifiers = shortcut
        .modifiers
        .iter()
        .map(|modifier| match modifier {
            Modifier::Shift => global_hotkey::hotkey::Modifiers::SHIFT,
            Modifier::Ctrl => global_hotkey::hotkey::Modifiers::CONTROL,
            Modifier::Alt => global_hotkey::hotkey::Modifiers::ALT,
            Modifier::Super => global_hotkey::hotkey::Modifiers::META,
        })
        .fold(global_hotkey::hotkey::Modifiers::empty(), |held, bit| {
            held | bit
        });
    Ok(HotKey::new(Some(modifiers), code(shortcut.key)?))
}

/// How the bound key reads in the status: described the same way the portal
/// describes its bindings.
pub(super) fn describe(shortcut: Shortcut) -> Trigger {
    Trigger {
        description: format!("Press {shortcut}"),
        shortcuts: vec![shortcut],
    }
}

fn code(key: Key) -> Result<Code, ShortcutError> {
    match key {
        Key::Letter(letter) => Ok(match letter {
            'a' => Code::KeyA,
            'b' => Code::KeyB,
            'c' => Code::KeyC,
            'd' => Code::KeyD,
            'e' => Code::KeyE,
            'f' => Code::KeyF,
            'g' => Code::KeyG,
            'h' => Code::KeyH,
            'i' => Code::KeyI,
            'j' => Code::KeyJ,
            'k' => Code::KeyK,
            'l' => Code::KeyL,
            'm' => Code::KeyM,
            'n' => Code::KeyN,
            'o' => Code::KeyO,
            'p' => Code::KeyP,
            'q' => Code::KeyQ,
            'r' => Code::KeyR,
            's' => Code::KeyS,
            't' => Code::KeyT,
            'u' => Code::KeyU,
            'v' => Code::KeyV,
            'w' => Code::KeyW,
            'x' => Code::KeyX,
            'y' => Code::KeyY,
            'z' => Code::KeyZ,
            _ => return Err(ShortcutError::UnsupportedKey(letter.to_string())),
        }),
        Key::Digit(digit) => Ok(match digit {
            '0' => Code::Digit0,
            '1' => Code::Digit1,
            '2' => Code::Digit2,
            '3' => Code::Digit3,
            '4' => Code::Digit4,
            '5' => Code::Digit5,
            '6' => Code::Digit6,
            '7' => Code::Digit7,
            '8' => Code::Digit8,
            '9' => Code::Digit9,
            _ => return Err(ShortcutError::UnsupportedKey(digit.to_string())),
        }),
        Key::Function(number) => Ok(match number {
            1 => Code::F1,
            2 => Code::F2,
            3 => Code::F3,
            4 => Code::F4,
            5 => Code::F5,
            6 => Code::F6,
            7 => Code::F7,
            8 => Code::F8,
            9 => Code::F9,
            10 => Code::F10,
            11 => Code::F11,
            12 => Code::F12,
            13 => Code::F13,
            14 => Code::F14,
            15 => Code::F15,
            16 => Code::F16,
            17 => Code::F17,
            18 => Code::F18,
            19 => Code::F19,
            20 => Code::F20,
            21 => Code::F21,
            22 => Code::F22,
            23 => Code::F23,
            24 => Code::F24,
            _ => return Err(ShortcutError::UnsupportedKey(format!("F{number}"))),
        }),
        Key::Named { keysym, .. } => Ok(match keysym {
            "space" => Code::Space,
            "grave" => Code::Backquote,
            "minus" => Code::Minus,
            "equal" => Code::Equal,
            "bracketleft" => Code::BracketLeft,
            "bracketright" => Code::BracketRight,
            "backslash" => Code::Backslash,
            "semicolon" => Code::Semicolon,
            "apostrophe" => Code::Quote,
            "comma" => Code::Comma,
            "period" => Code::Period,
            "slash" => Code::Slash,
            _ => return Err(ShortcutError::UnsupportedKey(keysym.to_string())),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::hotkey;
    use crate::shortcut::Shortcut;

    #[test]
    fn all_parse_formats_land_on_the_same_hotkey() {
        let shortcut = "Ctrl+Alt+Z".parse::<Shortcut>().unwrap();
        let accelerator = "<Control><Alt>z".parse::<Shortcut>().unwrap();
        let portal = "CTRL+ALT+z".parse::<Shortcut>().unwrap();
        assert_eq!(hotkey(shortcut), hotkey(accelerator));
        assert_eq!(hotkey(accelerator), hotkey(portal));
    }

    #[test]
    fn super_maps_to_cmd() {
        let hotkey = hotkey("Super+V".parse::<Shortcut>().unwrap()).unwrap();
        // global-hotkey normalizes its META alias to SUPER; both mean Cmd.
        assert!(
            hotkey
                .mods
                .contains(global_hotkey::hotkey::Modifiers::SUPER)
        );
        assert!(
            !hotkey
                .mods
                .contains(global_hotkey::hotkey::Modifiers::CONTROL)
        );
        assert_eq!(hotkey.key, global_hotkey::hotkey::Code::KeyV);
    }

    #[test]
    fn every_supported_key_converts() {
        for name in ('a'..='z').map(String::from) {
            assert!(
                hotkey(format!("Ctrl+{name}").parse().unwrap()).is_ok(),
                "{name}"
            );
        }
        for name in ('0'..='9').map(String::from) {
            assert!(
                hotkey(format!("Ctrl+{name}").parse().unwrap()).is_ok(),
                "{name}"
            );
        }
        for number in 1..=24 {
            let shortcut = format!("F{number}").parse::<Shortcut>().unwrap();
            assert!(hotkey(shortcut).is_ok(), "F{number}");
        }
        for name in [
            "Space",
            "Grave",
            "Minus",
            "Equal",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "Semicolon",
            "Apostrophe",
            "Comma",
            "Period",
            "Slash",
        ] {
            let shortcut = format!("Ctrl+{name}").parse::<Shortcut>().unwrap();
            assert!(hotkey(shortcut).is_ok(), "{name}");
        }
    }
}
