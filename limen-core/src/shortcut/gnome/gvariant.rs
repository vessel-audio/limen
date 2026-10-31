//! The GVariant text format of the strings and string arrays that
//! `gsettings` prints and parses, e.g. `'<Super>z'`, `['/a/', '/b/']`, and
//! `@as []`.

/// Parse a string or a string array; other values yield `None`.
pub(super) fn parse_strings(text: &str) -> Option<Vec<String>> {
    let text = text.trim();
    let text = text.strip_prefix("@as").map_or(text, str::trim_start);
    match text.strip_prefix('[') {
        Some(items) => {
            let mut rest = items.trim_start();
            let mut strings = Vec::new();
            loop {
                match rest.strip_prefix(']') {
                    Some(after) => return after.trim().is_empty().then_some(strings),
                    None => {
                        let (string, after) = parse_literal(rest)?;
                        strings.push(string);
                        let after = after.trim_start();
                        rest = after.strip_prefix(',').map_or(after, str::trim_start);
                    }
                }
            }
        }
        None => match parse_literal(text)? {
            (string, "") => Some(vec![string]),
            (_, _) => None,
        },
    }
}

/// Parse a single string.
pub(super) fn parse_string(text: &str) -> Option<String> {
    match parse_literal(text.trim())? {
        (string, "") => Some(string),
        (_, _) => None,
    }
}

/// A quoted string at the start of `text`, and the text after it.
fn parse_literal(text: &str) -> Option<(String, &str)> {
    let mut chars = text.char_indices();
    let quote = match chars.next()? {
        (_, quote @ ('\'' | '"')) => quote,
        (_, _) => return None,
    };
    let mut string = String::new();
    while let Some((index, character)) = chars.next() {
        match character {
            '\\' => {
                let (_, escaped) = chars.next()?;
                let unescaped = match escaped {
                    'a' => '\x07',
                    'b' => '\x08',
                    'f' => '\x0c',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'v' => '\x0b',
                    'u' | 'U' => {
                        let digits = match escaped {
                            'u' => 4,
                            _ => 8,
                        };
                        let hex: String = chars.by_ref().take(digits).map(|(_, c)| c).collect();
                        char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
                    }
                    other => other,
                };
                string.push(unescaped);
            }
            closing if closing == quote => {
                return Some((string, &text[index + closing.len_utf8()..]));
            }
            other => string.push(other),
        }
    }
    None
}

/// `value` as a GVariant string.
pub(super) fn string(value: &str) -> String {
    let mut text = String::from("'");
    for character in value.chars() {
        match character {
            '\'' | '\\' => {
                text.push('\\');
                text.push(character);
            }
            '\n' => text.push_str("\\n"),
            '\t' => text.push_str("\\t"),
            control if control.is_control() => {
                text.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => text.push(other),
        }
    }
    text.push('\'');
    text
}

/// `values` as a GVariant string array.
pub(super) fn strings(values: &[String]) -> String {
    match values {
        [] => "@as []".to_string(),
        values => format!(
            "[{}]",
            values
                .iter()
                .map(|value| string(value))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_string, parse_strings, string, strings};

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn string_arrays_parse() {
        let cases = [
            ("@as []", Some(owned(&[]))),
            ("[]", Some(owned(&[]))),
            ("['<Super>z']", Some(owned(&["<Super>z"]))),
            (
                "['/org/a/', \"/it's/\" ,'/b/']",
                Some(owned(&["/org/a/", "/it's/", "/b/"])),
            ),
            ("'<Super>z'", Some(owned(&["<Super>z"]))),
            ("true", None),
            ("uint32 5", None),
            ("['unterminated]", None),
            ("['a'] trailing", None),
        ];
        for (text, parsed) in cases {
            assert_eq!(parse_strings(text), parsed, "{text}");
        }
    }

    #[test]
    fn strings_unescape() {
        assert_eq!(
            [
                r"'it\'s'",
                r#""it's""#,
                r"'back\\slash'",
                r"'tab\there'",
                r"'é'",
                r"'a' 'b'",
            ]
            .map(parse_string),
            [
                Some("it's".to_string()),
                Some("it's".to_string()),
                Some(r"back\slash".to_string()),
                Some("tab\there".to_string()),
                Some("é".to_string()),
                None,
            ]
        );
    }

    #[test]
    fn written_values_parse_back() {
        let values = owned(&[
            "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/",
            "it's a \\ path\n",
            "",
            "微信",
        ]);
        for value in &values {
            assert_eq!(
                parse_string(&string(value)).as_ref(),
                Some(value),
                "{value}"
            );
        }
        assert_eq!(parse_strings(&strings(&values)), Some(values.clone()));
        assert_eq!(strings(&[]), "@as []");
        assert_eq!(strings(&owned(&["/a/", "/b/"])), "['/a/', '/b/']");
    }
}
