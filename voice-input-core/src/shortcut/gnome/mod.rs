//! The `gnome-keybinding` backend: a GNOME custom keybinding that runs
//! `voice-input toggle`. Only the CLI writes it, through `gsettings`; GNOME
//! runs the command on each press, so the daemon plays no part.
//!
//! The list of custom keybindings is shared with other applications (WeChat
//! adds its own entries, for one), so it is only ever read, changed by our
//! own path, and written back. An entry that someone set up by hand to run
//! `voice-input toggle` is adopted instead of adding a second one, and
//! clearing only disables it.
//!
//! Everything here is pure; the I/O lives in [`gsettings`].

pub(crate) mod gsettings;
mod gvariant;
mod shell;

use std::fmt;
use std::path::{Path, PathBuf};

use super::Shortcut;

/// The dconf path of the entry we create.
pub(crate) const OUR_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/voice-input-toggle/";

/// The name of the entry we create, as GNOME Settings shows it.
const OUR_NAME: &str = "voice-input dictation";

/// A custom keybinding as dconf stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomKeybinding {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) command: String,
    /// A GTK accelerator; empty when disabled.
    pub(crate) binding: String,
}

impl CustomKeybinding {
    /// Whether the entry runs `voice-input toggle`.
    fn runs_toggle(&self) -> bool {
        match shell::parse_argv(&self.command).as_deref() {
            Ok([program, action]) => {
                action == "toggle"
                    && Path::new(program)
                        .file_name()
                        .and_then(|name| name.to_str())
                        == Some("voice-input")
            }
            Ok(_) | Err(_) => false,
        }
    }
}

/// The entry that toggles dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Managed<'a> {
    /// Our own entry, at [`OUR_PATH`].
    Own(&'a CustomKeybinding),
    /// An entry set up by hand that runs `voice-input toggle`.
    Adopted(&'a CustomKeybinding),
    Absent,
}

/// Our entry if the list has it, else the first listed entry that runs
/// `voice-input toggle`. `entries` holds the entries of `list`.
pub(crate) fn managed<'a>(list: &[String], entries: &'a [CustomKeybinding]) -> Managed<'a> {
    let listed = || entries.iter().filter(|entry| list.contains(&entry.path));
    match listed().find(|entry| entry.path == OUR_PATH) {
        Some(own) => Managed::Own(own),
        None => listed()
            .find(|entry| entry.runs_toggle())
            .map_or(Managed::Absent, Managed::Adopted),
    }
}

/// The program `command` runs, if it parses.
pub(crate) fn program_of(command: &str) -> Option<PathBuf> {
    shell::parse_argv(command)
        .ok()?
        .into_iter()
        .next()
        .map(PathBuf::from)
}

/// The command GNOME runs: `program toggle`, quoted for GNOME.
pub(crate) fn toggle_command(program: &Path) -> String {
    format!("{} toggle", shell::quote(&program.to_string_lossy()))
}

/// What `setup` and `set` write: the entry's fields first, then, only for a
/// new entry, the list with its path added, so that GNOME never sees a
/// listed entry without its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Setup {
    pub(crate) path: String,
    /// `None` keeps the name of an adopted entry.
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    pub(crate) binding: String,
    /// The new list, when the path is not in it yet.
    pub(crate) list: Option<Vec<String>>,
}

/// Bind `shortcut` to `command`, reusing the managed entry if there is one.
pub(crate) fn setup(
    list: &[String],
    entries: &[CustomKeybinding],
    shortcut: Shortcut,
    command: String,
) -> Setup {
    let binding = shortcut.gnome_accelerator();
    match managed(list, entries) {
        Managed::Own(entry) => Setup {
            path: entry.path.clone(),
            name: Some(OUR_NAME.to_string()),
            command,
            binding,
            list: None,
        },
        Managed::Adopted(entry) => Setup {
            path: entry.path.clone(),
            name: None,
            command,
            binding,
            list: None,
        },
        Managed::Absent => Setup {
            path: OUR_PATH.to_string(),
            name: Some(OUR_NAME.to_string()),
            command,
            binding,
            list: Some(list.iter().cloned().chain([OUR_PATH.to_string()]).collect()),
        },
    }
}

/// What `clear` and switching to another backend do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Clear {
    Nothing,
    /// Reset our own entry and drop it from the list.
    Remove {
        path: String,
        list: Vec<String>,
    },
    /// Disable an adopted entry by emptying its binding; it stays otherwise.
    Disable {
        path: String,
    },
}

pub(crate) fn clear(list: &[String], entries: &[CustomKeybinding]) -> Clear {
    match managed(list, entries) {
        Managed::Own(entry) => Clear::Remove {
            path: entry.path.clone(),
            list: list
                .iter()
                .filter(|path| **path != entry.path)
                .cloned()
                .collect(),
        },
        Managed::Adopted(entry) if entry.binding.is_empty() => Clear::Nothing,
        Managed::Adopted(entry) => Clear::Disable {
            path: entry.path.clone(),
        },
        Managed::Absent => Clear::Nothing,
    }
}

/// Something that already uses a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conflict {
    /// A key of a GNOME keybinding schema.
    Setting { schema: String, key: String },
    /// Another custom keybinding.
    Custom { name: String },
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setting { schema, key } => write!(f, "{key} ({schema})"),
            Self::Custom { name } => write!(f, "the custom shortcut \"{name}\""),
        }
    }
}

/// What already uses `shortcut`: keys in `listing`, the output of
/// `gsettings list-recursively` for keybinding schemas, and `others`, the
/// custom keybindings besides the managed one. Values that are no
/// accelerator are skipped.
pub(crate) fn conflicts(
    shortcut: Shortcut,
    listing: &str,
    others: &[CustomKeybinding],
) -> Vec<Conflict> {
    let uses = |accelerator: &str| Shortcut::parse_accelerator(accelerator) == Ok(shortcut);
    let settings = listing.lines().filter_map(|line| {
        let mut fields = line.splitn(3, ' ');
        let (schema, key, value) = (fields.next()?, fields.next()?, fields.next()?);
        gvariant::parse_strings(value)?
            .iter()
            .any(|accelerator| uses(accelerator))
            .then(|| Conflict::Setting {
                schema: schema.to_string(),
                key: key.to_string(),
            })
    });
    let custom = others
        .iter()
        .filter(|entry| uses(&entry.binding))
        .map(|entry| Conflict::Custom {
            name: entry.name.clone(),
        });
    settings.chain(custom).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        Clear, Conflict, CustomKeybinding, Managed, OUR_PATH, Setup, clear, conflicts, gvariant,
        managed, setup, toggle_command,
    };

    pub(super) const PREFIX: &str =
        "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/";

    /// The two entries WeChat wrote on the development machine, as
    /// `gsettings get` prints their command.
    pub(super) fn wechat() -> Vec<CustomKeybinding> {
        let command = |action: &str| {
            gvariant::parse_string(&format!(
                r#""/bin/sh -c ''\\''/usr/bin/gdbus'\\'' call --session --dest '\\''com.tencent.wechat.wayland_global_shortcut_d3c81e34e639ba5c_5329_0'\\'' --method '\\''com.tencent.wechat.GlobalShortcuts.Activate'\\'' \"$1\" \"$XDG_ACTIVATION_TOKEN\"' fallback '{action}'""#
            ))
            .unwrap()
        };
        vec![
            CustomKeybinding {
                path: format!("{PREFIX}wayland_global_shortcut_95d769a40359c970/"),
                name: "显示/隐藏窗口".to_string(),
                command: command("main_panel_visibility"),
                binding: "<Control><Alt>w".to_string(),
            },
            CustomKeybinding {
                path: format!("{PREFIX}wayland_global_shortcut_60cf3e2567c6329/"),
                name: "截图".to_string(),
                command: command("screenshot"),
                binding: "<Alt>a".to_string(),
            },
        ]
    }

    pub(super) fn paths(entries: &[CustomKeybinding]) -> Vec<String> {
        entries.iter().map(|entry| entry.path.clone()).collect()
    }

    fn manual(command: &str) -> CustomKeybinding {
        CustomKeybinding {
            path: format!("{PREFIX}custom0/"),
            name: "dictation".to_string(),
            command: command.to_string(),
            binding: "<Super>d".to_string(),
        }
    }

    fn own(binding: &str) -> CustomKeybinding {
        CustomKeybinding {
            path: OUR_PATH.to_string(),
            name: "voice-input dictation".to_string(),
            command: "/bin/voice-input toggle".to_string(),
            binding: binding.to_string(),
        }
    }

    fn super_z() -> crate::shortcut::Shortcut {
        "Super+Z".parse().unwrap()
    }

    #[test]
    fn setup_adds_our_entry_and_keeps_every_other() {
        let entries = wechat();
        let list = paths(&entries);
        let first = setup(
            &list,
            &entries,
            super_z(),
            "/bin/voice-input toggle".to_string(),
        );
        assert_eq!(
            first,
            Setup {
                path: OUR_PATH.to_string(),
                name: Some("voice-input dictation".to_string()),
                command: "/bin/voice-input toggle".to_string(),
                binding: "<Super>z".to_string(),
                list: Some([list.clone(), vec![OUR_PATH.to_string()]].concat()),
            }
        );

        // Run again once written: same entry, list untouched.
        let written = [entries.clone(), vec![own("<Super>z")]].concat();
        assert_eq!(
            setup(
                &first.list.clone().unwrap(),
                &written,
                super_z(),
                "/bin/voice-input toggle".to_string()
            ),
            Setup {
                list: None,
                ..first
            }
        );
    }

    #[test]
    fn a_manual_toggle_entry_is_adopted() {
        for command in [
            "voice-input toggle",
            "'/home/me/my apps/voice-input' toggle",
        ] {
            let entries = [wechat(), vec![manual(command)]].concat();
            assert_eq!(
                setup(
                    &paths(&entries),
                    &entries,
                    super_z(),
                    "/bin/voice-input toggle".to_string()
                ),
                Setup {
                    path: format!("{PREFIX}custom0/"),
                    name: None,
                    command: "/bin/voice-input toggle".to_string(),
                    binding: "<Super>z".to_string(),
                    list: None,
                },
                "{command}"
            );
        }
    }

    #[test]
    fn other_commands_are_never_adopted() {
        for command in [
            "voice-input status",
            "voice-input toggle --now",
            "voice-input-other toggle",
            "notify-send toggle",
        ] {
            let entries = vec![manual(command)];
            assert_eq!(
                managed(&paths(&entries), &entries),
                Managed::Absent,
                "{command}"
            );
        }
    }

    #[test]
    fn our_entry_wins_and_unlisted_entries_do_not_count() {
        let entries = vec![manual("voice-input toggle"), own("<Super>z")];
        assert_eq!(
            managed(&paths(&entries), &entries),
            Managed::Own(&entries[1])
        );
        // A leftover of a setup that never wrote the list is overwritten.
        assert_eq!(managed(&[], &entries), Managed::Absent);
    }

    #[test]
    fn clear_removes_our_entry_and_disables_an_adopted_one() {
        let ours = [wechat(), vec![own("<Super>z")]].concat();
        assert_eq!(
            clear(&paths(&ours), &ours),
            Clear::Remove {
                path: OUR_PATH.to_string(),
                list: paths(&wechat()),
            }
        );

        let adopted = [wechat(), vec![manual("voice-input toggle")]].concat();
        assert_eq!(
            clear(&paths(&adopted), &adopted),
            Clear::Disable {
                path: format!("{PREFIX}custom0/"),
            }
        );
        let disabled = vec![CustomKeybinding {
            binding: String::new(),
            ..manual("voice-input toggle")
        }];
        assert_eq!(clear(&paths(&disabled), &disabled), Clear::Nothing);
        assert_eq!(clear(&paths(&wechat()), &wechat()), Clear::Nothing);
    }

    #[test]
    fn conflicts_name_what_uses_the_shortcut() {
        let listing = "org.gnome.shell.keybindings focus-active-notification ['<Super>n']\n\
                       org.gnome.shell.keybindings toggle-message-tray ['<Super>v', '<Super>m']\n\
                       org.gnome.shell.keybindings toggle-overview @as []\n\
                       org.gnome.settings-daemon.plugins.media-keys calculator ['']\n";
        assert_eq!(
            conflicts("Super+V".parse().unwrap(), listing, &wechat()),
            [Conflict::Setting {
                schema: "org.gnome.shell.keybindings".to_string(),
                key: "toggle-message-tray".to_string(),
            }]
        );
        assert_eq!(
            conflicts("Ctrl+Alt+W".parse().unwrap(), listing, &wechat()),
            [Conflict::Custom {
                name: "显示/隐藏窗口".to_string(),
            }]
        );
        assert_eq!(conflicts(super_z(), listing, &wechat()), []);
        assert_eq!(
            Conflict::Setting {
                schema: "org.gnome.shell.keybindings".to_string(),
                key: "toggle-message-tray".to_string(),
            }
            .to_string(),
            "toggle-message-tray (org.gnome.shell.keybindings)"
        );
    }

    #[test]
    fn toggle_commands_are_quoted_for_gnome() {
        assert_eq!(
            [
                toggle_command(Path::new("/etc/profiles/per-user/me/bin/voice-input")),
                toggle_command(Path::new("/home/me/my apps/voice-input")),
            ],
            [
                "/etc/profiles/per-user/me/bin/voice-input toggle",
                "'/home/me/my apps/voice-input' toggle",
            ]
        );
    }
}
