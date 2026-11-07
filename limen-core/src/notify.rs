//! Desktop notifications. Fire-and-forget: a missing notification daemon must
//! never break dictation.

use std::process::Stdio;

use tokio::process::Command;

#[cfg(target_os = "linux")]
pub fn notify(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .arg("--app-name=Limen")
        .arg(summary)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Native-looking notifications come from the terminal-notifier helper app
/// (installable through Homebrew); plain CLI processes cannot post to
/// UNUserNotificationCenter, so without it we fall back to osascript, which
/// shows up as "Script Editor" in Notification Center.
#[cfg(target_os = "macos")]
pub fn notify(summary: &str, body: &str) {
    let mut terminal_notifier = Command::new("terminal-notifier");
    terminal_notifier
        .arg("-title")
        .arg(summary)
        .arg("-message")
        .arg(body);
    let mut fallback = Command::new("osascript");
    let script = format!(
        "display notification {} with title {}",
        apple_script_literal(body),
        apple_script_literal(summary)
    );
    fallback.arg("-e").arg(script);
    let mut command = match terminal_notifier_available() {
        true => terminal_notifier,
        false => fallback,
    };
    let _ = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn terminal_notifier_available() -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("terminal-notifier"))
        .any(|candidate| candidate.is_file())
}

/// Quote a string for embedding in AppleScript source.
#[cfg(target_os = "macos")]
fn apple_script_literal(text: &str) -> String {
    let escaped = text
        .chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            character if character.is_control() => vec![' '],
            character => vec![character],
        })
        .collect::<String>();
    format!("\"{escaped}\"")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::apple_script_literal;

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(
            apple_script_literal("say \"hi\"\\now"),
            "\"say \\\"hi\\\"\\\\now\""
        );
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(apple_script_literal("a\nb\t"), "\"a b \"");
    }
}
