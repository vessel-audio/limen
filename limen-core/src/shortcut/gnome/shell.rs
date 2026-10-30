//! The quoting rules of GLib's `g_shell_parse_argv`, which GNOME applies to
//! the command of a custom keybinding. There is no shell: no variables, no
//! globbing, only quotes, backslashes, and comments.

/// Quote `arg` so that `g_shell_parse_argv` reads it back as one argument.
/// Plain paths stay readable; anything else is single-quoted the way
/// `g_shell_quote` does it.
pub(super) fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ':' | ',' | '@')
        });
    match plain {
        true => arg.to_string(),
        false => format!("'{}'", arg.replace('\'', r"'\''")),
    }
}

/// Split `command` into arguments the way `g_shell_parse_argv` does.
pub(super) fn parse_argv(command: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = command.chars().peekable();
    let mut previous = None;
    while let Some(character) = chars.next() {
        match character {
            ' ' | '\t' | '\n' => {
                args.extend(current.take());
            }
            // A comment starts a word: everything up to the end of the line.
            '#' if current.is_none() && matches!(previous, None | Some(' ' | '\n')) => {
                // The newline that ends the comment may start the next one.
                previous = chars.by_ref().find(|&skipped| skipped == '\n');
                continue;
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(escaped) => current.get_or_insert_default().push(escaped),
                None => return Err("text ended just after a backslash".to_string()),
            },
            '\'' => {
                let word = current.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(literal) => word.push(literal),
                        None => return Err("text ended before a closing '".to_string()),
                    }
                }
            }
            '"' => {
                let word = current.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&escaped @ ('"' | '\\' | '`' | '$' | '\n')) => {
                                word.push(escaped);
                                chars.next();
                            }
                            Some(_) | None => word.push('\\'),
                        },
                        Some(literal) => word.push(literal),
                        None => return Err("text ended before a closing \"".to_string()),
                    }
                }
            }
            other => current.get_or_insert_default().push(other),
        }
        previous = Some(character);
    }
    args.extend(current);
    match args.is_empty() {
        true => Err("text was empty".to_string()),
        false => Ok(args),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_argv, quote};

    fn argv(args: &[&str]) -> Result<Vec<String>, String> {
        Ok(args.iter().map(|arg| arg.to_string()).collect())
    }

    #[test]
    fn commands_split_like_glib() {
        let cases = [
            ("limen toggle", argv(&["limen", "toggle"])),
            ("  limen\t toggle  ", argv(&["limen", "toggle"])),
            (
                "'/my apps/limen' toggle",
                argv(&["/my apps/limen", "toggle"]),
            ),
            (
                r#""/my \"apps\"/vi" toggle"#,
                argv(&[r#"/my "apps"/vi"#, "toggle"]),
            ),
            (r#""a\b""#, argv(&[r"a\b"])),
            (r"/my\ apps/vi toggle", argv(&["/my apps/vi", "toggle"])),
            ("vi'nput' x\"y\"z", argv(&["vinput", "xyz"])),
            ("vi toggle # comment", argv(&["vi", "toggle"])),
            ("vi toggle # first\n# second", argv(&["vi", "toggle"])),
            ("vi # first\ntoggle", argv(&["vi", "toggle"])),
            ("vi to#ggle", argv(&["vi", "to#ggle"])),
        ];
        for (command, args) in cases {
            assert_eq!(parse_argv(command), args, "{command}");
        }
    }

    #[test]
    fn broken_commands_fail() {
        for command in ["", "   ", "'open", "\"open", r"trailing\"] {
            assert!(parse_argv(command).is_err(), "{command:?}");
        }
    }

    #[test]
    fn quoted_paths_parse_back_unchanged() {
        let paths = [
            "/etc/profiles/per-user/me/bin/limen",
            "/home/me/my apps/limen",
            "/home/me/it's/limen",
            "/home/me/'quoted' \"double\" \\back/limen",
            "/home/me/#hash $dollar/limen",
        ];
        for path in paths {
            assert_eq!(
                parse_argv(&format!("{} toggle", quote(path))),
                argv(&[path, "toggle"]),
                "{path}"
            );
        }
        assert_eq!(
            quote("/etc/profiles/per-user/me/bin/limen"),
            "/etc/profiles/per-user/me/bin/limen"
        );
        assert_eq!(quote("/it's"), r"'/it'\''s'");
    }
}
