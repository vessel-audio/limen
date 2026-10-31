//! Desktop notifications via `notify-send`. Fire-and-forget: a missing
//! notification daemon must never break dictation.

use std::process::Stdio;

use tokio::process::Command;

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
