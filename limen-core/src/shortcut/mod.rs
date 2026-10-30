//! Global shortcuts: the key combinations bound to dictation actions, and the
//! backends that bind them.
//!
//! [`Shortcut`] is the one representation shared by the config file, the CLI,
//! GNOME's keybinding settings, and the GlobalShortcuts portal. It reads and
//! writes three formats:
//!
//! | Format         | Example                       | Used for                           |
//! |----------------|-------------------------------|------------------------------------|
//! | human-readable | `Super+Z`, `Ctrl+Alt+W`       | config file, CLI input, display    |
//! | GNOME          | `<Super>z`, `<Control><Alt>w` | dconf keybindings, conflict checks |
//! | portal         | `LOGO+z`, `CTRL+ALT+w`        | the portal's `preferred_trigger`   |
//!
//! Parsing accepts any of the three, ignoring case, along with the modifier
//! aliases of each (`Meta`, `Logo`, and `Win` for Super; GTK's `<Primary>` for
//! Ctrl). Rendering always lists the modifiers in GTK's order: Shift, Ctrl,
//! Alt, Super.

pub(crate) mod desktop_entry;
pub(crate) mod gnome;
pub(crate) mod portal;
pub(crate) mod program;
pub(crate) mod setup;
pub(crate) mod status;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Press once to start dictation and again to stop.
    Toggle,
    /// Hold to talk; releasing the key stops.
    Hold,
}

impl Action {
    /// The ID the portal knows the action's shortcut by.
    pub fn id(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Hold => "hold",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "toggle" => Some(Self::Toggle),
            "hold" => Some(Self::Hold),
            _ => None,
        }
    }
}

/// The mechanism that turns system-wide key presses into actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// The daemon binds both actions through the GlobalShortcuts portal.
    Portal,
    /// A GNOME custom keybinding runs `limen toggle`; toggle only.
    GnomeKeybinding,
    /// The user binds `limen toggle` in the desktop settings.
    #[default]
    None,
}

/// The name used in the config file and on the command line.
impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Portal => "portal",
            Self::GnomeKeybinding => "gnome-keybinding",
            Self::None => "none",
        })
    }
}

/// Whether the hold-to-talk key went down or came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum HoldEdge {
    Press,
    Release,
}

/// A global shortcut fired: what the dictation loop should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activation {
    Toggle,
    Hold(HoldEdge),
}

/// An activation and when it reached the daemon's mailbox. Keeping this
/// timestamp next to the event prevents later mailbox scheduling from
/// changing a hold's effective duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivationReceipt {
    pub(crate) activation: Activation,
    pub(crate) received_at: tokio::time::Instant,
}

/// Why a shortcut does not parse or is not allowed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShortcutError {
    #[error("empty shortcut")]
    Empty,
    #[error("expected modifiers and a key, e.g. Super+Z or <Super>z")]
    Malformed,
    #[error("unsupported modifier {0:?}; use Super, Ctrl, Alt, or Shift")]
    UnsupportedModifier(String),
    #[error("{0} is given twice")]
    DuplicateModifier(&'static str),
    #[error("no key after the modifiers")]
    MissingKey,
    #[error(
        "unsupported key {0:?}; supported keys: A-Z, 0-9, F1-F24, Space, Grave, Minus, Equal, \
         BracketLeft, BracketRight, Backslash, Semicolon, Apostrophe, Comma, Period, Slash"
    )]
    UnsupportedKey(String),
    #[error("{0} needs Super, Ctrl, or Alt; only F1-F24 work without them")]
    NeedsModifier(Shortcut),
    #[error("{0} would get in the way of typing; add Super, Ctrl, or Alt")]
    ShiftOnly(Shortcut),
}

/// A key combination: modifiers held together with one non-modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Shortcut {
    modifiers: Modifiers,
    key: Key,
}

impl Shortcut {
    /// `Super+Z`, the default toggle shortcut.
    pub const DEFAULT_TOGGLE: Self = Self::with_super(Key::Letter('z'));
    /// `Super+X`, the default hold-to-talk shortcut.
    pub const DEFAULT_HOLD: Self = Self::with_super(Key::Letter('x'));

    const fn with_super(key: Key) -> Self {
        Self {
            modifiers: Modifiers(Modifier::Super.bit()),
            key,
        }
    }

    /// Parse the GNOME format only, e.g. `<Control><Alt>w` or `F9`.
    pub fn parse_accelerator(text: &str) -> Result<Self, ShortcutError> {
        let mut modifiers = Modifiers::default();
        let mut rest = text;
        while let Some(tail) = rest.strip_prefix('<') {
            let (name, after) = tail.split_once('>').ok_or(ShortcutError::Malformed)?;
            modifiers = modifiers.insert(Modifier::parse_gtk(name)?)?;
            rest = after;
        }
        Ok(Self {
            modifiers,
            key: Key::parse(rest)?,
        })
    }

    /// Parse the human-readable or portal format, e.g. `Ctrl+Alt+W` or `CTRL+ALT+w`.
    fn parse_joined(text: &str) -> Result<Self, ShortcutError> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let key = parts.pop().unwrap_or_default();
        let modifiers = parts
            .into_iter()
            .try_fold(Modifiers::default(), |held, name| {
                held.insert(Modifier::parse_joined(name)?)
            })?;
        Ok(Self {
            modifiers,
            key: Key::parse(key)?,
        })
    }

    /// Check the rules for a global shortcut: it needs Super, Ctrl, or Alt so
    /// that ordinary typing never triggers it; only F1-F24 may stand alone.
    pub fn validate(self) -> Result<Self, ShortcutError> {
        let has_command_modifier = [Modifier::Ctrl, Modifier::Alt, Modifier::Super]
            .into_iter()
            .any(|modifier| self.modifiers.contains(modifier));
        match (has_command_modifier, self.key) {
            (true, _) | (false, Key::Function(_)) => Ok(self),
            (false, Key::Letter(_) | Key::Digit(_)) if self.modifiers.contains(Modifier::Shift) => {
                Err(ShortcutError::ShiftOnly(self))
            }
            (false, Key::Letter(_) | Key::Digit(_) | Key::Named { .. }) => {
                Err(ShortcutError::NeedsModifier(self))
            }
        }
    }

    /// The GNOME format, e.g. `<Control><Alt>w`.
    pub(crate) fn gnome_accelerator(self) -> String {
        self.modifiers
            .iter()
            .map(|modifier| format!("<{}>", modifier.gnome_name()))
            .chain([self.key.keysym()])
            .collect()
    }

    /// The portal format, e.g. `CTRL+ALT+w`.
    pub(crate) fn portal_trigger(self) -> String {
        self.modifiers
            .iter()
            .map(|modifier| modifier.portal_name().to_string())
            .chain([self.key.keysym()])
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl FromStr for Shortcut {
    type Err = ShortcutError;

    /// Parse any of the three formats.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        match text.chars().next() {
            None => Err(ShortcutError::Empty),
            Some('<') => Self::parse_accelerator(text),
            Some(_) => Self::parse_joined(text),
        }
    }
}

/// The human-readable format, e.g. `Ctrl+Alt+W`.
impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.modifiers
            .iter()
            .try_for_each(|modifier| write!(f, "{}+", modifier.label()))?;
        f.write_str(&self.key.label())
    }
}

impl TryFrom<String> for Shortcut {
    type Error = ShortcutError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Shortcut> for String {
    fn from(shortcut: Shortcut) -> Self {
        shortcut.to_string()
    }
}

/// The shortcuts named by a portal `trigger_description` such as
/// `Press <Super>z`. GNOME words it as "Press %s" or "Press %s or %s" around
/// GTK accelerators, and translations may put quotes right next to them
/// (`Натиснете „<Super>z“`), so the text is split at every character that
/// cannot be part of an accelerator and each piece is tried as one.
///
/// These are bindings the user chose in GNOME, so the rules of [`validate`]
/// do not apply. A lone letter or digit is skipped, though: it cannot be told
/// apart from a translated word such as "o". Neither can a translated suffix
/// glued to the key, so such descriptions yield nothing rather than a wrong
/// shortcut.
///
/// [`validate`]: Shortcut::validate
pub(crate) fn accelerators_in(description: &str) -> Vec<Shortcut> {
    description
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '<' | '>')))
        .filter_map(|piece| Shortcut::parse_accelerator(piece).ok())
        .filter(
            |shortcut| match (shortcut.modifiers.iter().next(), shortcut.key) {
                (Some(_), _) | (None, Key::Function(_) | Key::Named { .. }) => true,
                (None, Key::Letter(_) | Key::Digit(_)) => false,
            },
        )
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Super,
}

impl Modifier {
    /// Every modifier, in rendering order.
    const ALL: [Self; 4] = [Self::Shift, Self::Ctrl, Self::Alt, Self::Super];

    /// A modifier in the human-readable or portal format, ignoring case.
    fn parse_joined(name: &str) -> Result<Self, ShortcutError> {
        match name.to_ascii_lowercase().as_str() {
            "shift" => Ok(Self::Shift),
            "ctrl" | "control" => Ok(Self::Ctrl),
            "alt" => Ok(Self::Alt),
            "super" | "meta" | "logo" | "win" => Ok(Self::Super),
            "" => Err(ShortcutError::Malformed),
            _ => Err(ShortcutError::UnsupportedModifier(name.to_string())),
        }
    }

    /// A modifier as GTK spells it, ignoring case. GTK's Meta and Hyper are
    /// modifiers of their own rather than names for Super, so they are not
    /// supported.
    fn parse_gtk(name: &str) -> Result<Self, ShortcutError> {
        match name.to_ascii_lowercase().as_str() {
            "shift" | "shft" => Ok(Self::Shift),
            "control" | "ctrl" | "ctl" | "primary" => Ok(Self::Ctrl),
            "alt" => Ok(Self::Alt),
            "super" => Ok(Self::Super),
            "" => Err(ShortcutError::Malformed),
            _ => Err(ShortcutError::UnsupportedModifier(name.to_string())),
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }

    fn label(self) -> &'static str {
        match self {
            Self::Shift => "Shift",
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Super => "Super",
        }
    }

    fn gnome_name(self) -> &'static str {
        match self {
            Self::Shift => "Shift",
            Self::Ctrl => "Control",
            Self::Alt => "Alt",
            Self::Super => "Super",
        }
    }

    fn portal_name(self) -> &'static str {
        match self {
            Self::Shift => "SHIFT",
            Self::Ctrl => "CTRL",
            Self::Alt => "ALT",
            Self::Super => "LOGO",
        }
    }
}

/// The set of modifiers held with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Modifiers(u8);

impl Modifiers {
    fn contains(self, modifier: Modifier) -> bool {
        self.0 & modifier.bit() != 0
    }

    /// Held modifiers in rendering order.
    fn iter(self) -> impl Iterator<Item = Modifier> {
        Modifier::ALL
            .into_iter()
            .filter(move |modifier| self.contains(*modifier))
    }

    fn insert(self, modifier: Modifier) -> Result<Self, ShortcutError> {
        match self.contains(modifier) {
            true => Err(ShortcutError::DuplicateModifier(modifier.label())),
            false => Ok(Self(self.0 | modifier.bit())),
        }
    }
}

/// Supported keys besides letters, digits, and function keys: the X keysym
/// name used by GNOME and the portal, and the human-readable label.
const NAMED_KEYS: [(&str, &str); 12] = [
    ("space", "Space"),
    ("grave", "Grave"),
    ("minus", "Minus"),
    ("equal", "Equal"),
    ("bracketleft", "BracketLeft"),
    ("bracketright", "BracketRight"),
    ("backslash", "Backslash"),
    ("semicolon", "Semicolon"),
    ("apostrophe", "Apostrophe"),
    ("comma", "Comma"),
    ("period", "Period"),
    ("slash", "Slash"),
];

/// A non-modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// `a`-`z`, stored in lowercase.
    Letter(char),
    /// `0`-`9`.
    Digit(char),
    /// `F1`-`F24`.
    Function(u8),
    /// One of [`NAMED_KEYS`].
    Named {
        keysym: &'static str,
        label: &'static str,
    },
}

impl Key {
    fn parse(name: &str) -> Result<Self, ShortcutError> {
        let lower = name.to_ascii_lowercase();
        let function = lower
            .strip_prefix('f')
            .filter(|digits| !digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u8>().ok())
            .filter(|number| (1..=24).contains(number));
        let named = NAMED_KEYS.into_iter().find(|(keysym, _)| *keysym == lower);
        let mut chars = lower.chars();
        match (chars.next(), chars.next(), function, named) {
            (None, _, _, _) => Err(ShortcutError::MissingKey),
            (Some(letter @ 'a'..='z'), None, _, _) => Ok(Self::Letter(letter)),
            (Some(digit @ '0'..='9'), None, _, _) => Ok(Self::Digit(digit)),
            (_, _, Some(number), _) => Ok(Self::Function(number)),
            (_, _, None, Some((keysym, label))) => Ok(Self::Named { keysym, label }),
            (Some(_), _, None, None) => Err(ShortcutError::UnsupportedKey(name.to_string())),
        }
    }

    /// The X keysym name, as written by GNOME and the portal.
    fn keysym(self) -> String {
        match self {
            Self::Letter(character) | Self::Digit(character) => character.to_string(),
            Self::Function(number) => format!("F{number}"),
            Self::Named { keysym, .. } => keysym.to_string(),
        }
    }

    fn label(self) -> String {
        match self {
            Self::Letter(letter) => letter.to_ascii_uppercase().to_string(),
            Self::Digit(digit) => digit.to_string(),
            Self::Function(number) => format!("F{number}"),
            Self::Named { label, .. } => label.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NAMED_KEYS, Shortcut, ShortcutError, accelerators_in};

    fn parse(text: &str) -> Shortcut {
        text.parse().unwrap()
    }

    /// The name of every supported key, as accepted on input.
    fn every_key() -> Vec<String> {
        ('a'..='z')
            .chain('0'..='9')
            .map(String::from)
            .chain((1..=24).map(|number| format!("F{number}")))
            .chain(NAMED_KEYS.into_iter().map(|(keysym, _)| keysym.to_string()))
            .collect()
    }

    #[test]
    fn every_format_parses_to_the_same_shortcut() {
        let shortcuts = ["Super+Z", "super+z", "<Super>z", "LOGO+z"].map(parse);
        assert_eq!(shortcuts, [Shortcut::DEFAULT_TOGGLE; 4]);
        let shortcut = Shortcut::DEFAULT_TOGGLE;
        assert_eq!(
            [
                shortcut.to_string(),
                shortcut.gnome_accelerator(),
                shortcut.portal_trigger()
            ],
            ["Super+Z", "<Super>z", "LOGO+z"]
        );
    }

    #[test]
    fn every_key_round_trips_through_every_format() {
        for key in every_key() {
            let shortcut = parse(&format!("Ctrl+{key}"));
            let rendered = [
                shortcut.to_string(),
                shortcut.gnome_accelerator(),
                shortcut.portal_trigger(),
            ];
            assert_eq!(rendered.map(|text| parse(&text)), [shortcut; 3], "{key}");
        }
    }

    #[test]
    fn modifier_aliases_round_trip_through_every_format() {
        let aliases = [
            ("Meta+F5", "Super+F5"),
            ("Logo+F5", "Super+F5"),
            ("Win+F5", "Super+F5"),
            ("Control+F5", "Ctrl+F5"),
            ("<Control>F5", "Ctrl+F5"),
            ("<Primary>F5", "Ctrl+F5"),
            ("<Ctrl>F5", "Ctrl+F5"),
            ("<Ctl>F5", "Ctrl+F5"),
            ("<Shft>F5", "Shift+F5"),
            ("CTRL+ALT+SHIFT+LOGO+F5", "Shift+Ctrl+Alt+Super+F5"),
        ];
        for (alias, canonical) in aliases {
            let shortcut = parse(alias);
            assert_eq!(shortcut, parse(canonical), "{alias}");
            assert_eq!(
                [
                    parse(&shortcut.to_string()),
                    parse(&shortcut.gnome_accelerator()),
                    parse(&shortcut.portal_trigger()),
                ],
                [shortcut; 3],
                "{alias}"
            );
        }
    }

    #[test]
    fn modifiers_are_unordered_on_input_and_ordered_on_output() {
        assert_eq!(parse("Alt+Ctrl+W"), parse("Ctrl+Alt+W"));
        let shortcut = parse("Super+Shift+Alt+Ctrl+W");
        assert_eq!(
            [
                shortcut.to_string(),
                shortcut.gnome_accelerator(),
                shortcut.portal_trigger()
            ],
            [
                "Shift+Ctrl+Alt+Super+W",
                "<Shift><Control><Alt><Super>w",
                "SHIFT+CTRL+ALT+LOGO+w",
            ]
        );
    }

    #[test]
    fn gnome_accelerators_ignore_case() {
        assert_eq!(
            ["<super>z", "<SUPER>Z", "<Super>Z"].map(parse),
            [Shortcut::DEFAULT_TOGGLE; 3]
        );
        assert_eq!(parse("<Super>Z").gnome_accelerator(), "<Super>z");
    }

    #[test]
    fn trigger_descriptions_yield_their_accelerators() {
        let super_z = Shortcut::DEFAULT_TOGGLE;
        let ctrl_f9 = parse("Ctrl+F9");
        // GNOME 50 portal wordings, including translations.
        let cases = [
            ("Press <Super>z", vec![super_z]),
            ("Press <Super>z or <Control>F9", vec![super_z, ctrl_f9]),
            ("按下 <Super>z", vec![super_z]),
            ("<Super>z を押す", vec![super_z]),
            (
                "Натиснете „<Super>z“ или „<Control>F9“",
                vec![super_z, ctrl_f9],
            ),
            ("Pulse <Super>z o F9", vec![super_z, parse("F9")]),
            (
                "Nyomja meg a(z) <Super>z billentyűkombinációt",
                vec![super_z],
            ),
            (
                "<Super>z yoki <Control>F9 tugmasini bosing",
                vec![super_z, ctrl_f9],
            ),
            // Bindings chosen in GNOME need not follow the config rules.
            ("Press <Shift>a", vec![parse("Shift+A")]),
            ("Press space", vec![parse("Space")]),
        ];
        for (description, shortcuts) in cases {
            assert_eq!(accelerators_in(description), shortcuts, "{description}");
        }
    }

    #[test]
    fn descriptions_without_accelerators_yield_nothing() {
        let descriptions = [
            "",
            "Press nothing",
            "Super+Z",
            "<Super",
            "<Super>zni bosing",
            "Press a",
            "Press <Meta>F5",
        ];
        for description in descriptions {
            assert_eq!(accelerators_in(description), [], "{description:?}");
        }
    }

    #[test]
    fn usable_shortcuts_pass_validation() {
        for text in ["F9", "Ctrl+Alt+W", "Shift+Super+Z"] {
            assert_eq!(parse(text).validate(), Ok(parse(text)), "{text}");
        }
    }

    #[test]
    fn shortcuts_that_interfere_with_typing_fail_validation() {
        assert_eq!(
            ["A", "Shift+A", "Shift+1", "Shift+Space"].map(|text| parse(text).validate()),
            [
                Err(ShortcutError::NeedsModifier(parse("A"))),
                Err(ShortcutError::ShiftOnly(parse("Shift+A"))),
                Err(ShortcutError::ShiftOnly(parse("Shift+1"))),
                Err(ShortcutError::NeedsModifier(parse("Shift+Space"))),
            ]
        );
    }

    #[test]
    fn unsupported_keys_name_the_supported_range() {
        let error = "Super+Escape".parse::<Shortcut>().unwrap_err();
        assert_eq!(error, ShortcutError::UnsupportedKey("Escape".to_string()));
        assert!(error.to_string().contains("A-Z, 0-9, F1-F24, Space"));
    }

    #[test]
    fn malformed_shortcuts_fail_to_parse() {
        let cases = [
            ("", ShortcutError::Empty),
            ("Super+", ShortcutError::MissingKey),
            ("+Z", ShortcutError::Malformed),
            (
                "Hyper+Z",
                ShortcutError::UnsupportedModifier("Hyper".to_string()),
            ),
            ("Super+Super+Z", ShortcutError::DuplicateModifier("Super")),
            ("<Super", ShortcutError::Malformed),
            ("<Super>", ShortcutError::MissingKey),
            (
                "<Foo>z",
                ShortcutError::UnsupportedModifier("Foo".to_string()),
            ),
            (
                "<Meta>F5",
                ShortcutError::UnsupportedModifier("Meta".to_string()),
            ),
            (
                "<Logo>F5",
                ShortcutError::UnsupportedModifier("Logo".to_string()),
            ),
            (
                "Primary+F5",
                ShortcutError::UnsupportedModifier("Primary".to_string()),
            ),
            (
                "Super+F25",
                ShortcutError::UnsupportedKey("F25".to_string()),
            ),
            (
                "Super+F09",
                ShortcutError::UnsupportedKey("F09".to_string()),
            ),
        ];
        for (text, error) in cases {
            assert_eq!(text.parse::<Shortcut>(), Err(error), "{text:?}");
        }
    }
}
