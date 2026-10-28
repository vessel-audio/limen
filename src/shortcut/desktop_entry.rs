//! The desktop entry that names the app to the desktop. The GlobalShortcuts
//! portal registers only an app ID that matches an installed `.desktop`
//! file, and GNOME Settings lists the app's shortcuts under it, so the entry
//! must not be hidden with `NoDisplay`.
//!
//! The Nix package installs the entry itself. Elsewhere, `voice-input
//! shortcut setup portal` writes one to `$XDG_DATA_HOME/applications`, unless
//! the data directories already have an entry with this name.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The app ID the portal knows the daemon by.
pub(crate) const APP_ID: &str = "io.github.Emin017.VoiceInput";

/// The entry as it is kept in the repository.
const ENTRY: &str = include_str!("../../data/io.github.Emin017.VoiceInput.desktop");

/// Where the entry was found or put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Install {
    Present(PathBuf),
    Installed(PathBuf),
}

/// Make sure an entry for [`APP_ID`] is installed, writing one that runs
/// `program daemon` when none is found.
pub(crate) fn ensure_installed(program: &Path) -> Result<Install> {
    let home = dirs::data_dir().context("cannot find the user's data directory")?;
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    install(
        &home,
        &std::env::split_paths(&system).collect::<Vec<_>>(),
        program,
    )
}

fn install(home: &Path, system: &[PathBuf], program: &Path) -> Result<Install> {
    let name = format!("{APP_ID}.desktop");
    let found = std::iter::once(home)
        .chain(system.iter().map(PathBuf::as_path))
        .map(|dir| dir.join("applications").join(&name))
        .find(|path| path.exists());
    match found {
        Some(path) => Ok(Install::Present(path)),
        None => {
            let dir = home.join("applications");
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
            let path = dir.join(&name);
            // Written in full under a temporary name, then linked into place: the
            // entry never shows half written, and one that another process
            // created meanwhile is never replaced.
            let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
            // Only a dead process can have left a file under our process ID.
            let _ = fs::remove_file(&temp);
            let linked = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .and_then(|mut file| {
                    file.write_all(entry_for(program).as_bytes())?;
                    file.sync_all()
                })
                .and_then(|()| match fs::hard_link(&temp, &path) {
                    Ok(()) => Ok(Install::Installed(path.clone())),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        Ok(Install::Present(path.clone()))
                    }
                    Err(error) => Err(error),
                });
            let _ = fs::remove_file(&temp);
            linked.with_context(|| format!("failed to write {}", path.display()))
        }
    }
}

/// The entry, with `Exec` running `program daemon`.
fn entry_for(program: &Path) -> String {
    let exec = format!("Exec={} daemon", exec_arg(&program.to_string_lossy()));
    ENTRY
        .lines()
        .map(|line| match line.starts_with("Exec=") {
            true => exec.as_str(),
            false => line,
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// `arg` as an argument of the `Exec` key. The Desktop Entry spec quotes an
/// argument that holds reserved characters in double quotes, escaping `"`,
/// `` ` ``, `$`, and `\` inside them, and writes `%` as `%%`. The key's value
/// is a string, whose own escaping doubles every backslash once more.
fn exec_arg(arg: &str) -> String {
    let reserved = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '\\'
                    | '>'
                    | '<'
                    | '~'
                    | '|'
                    | '&'
                    | ';'
                    | '$'
                    | '*'
                    | '?'
                    | '#'
                    | '('
                    | ')'
                    | '`'
            )
    };
    let quoted = match arg.chars().any(reserved) {
        true => {
            let escaped: String = arg
                .chars()
                .flat_map(|c| match c {
                    '"' | '`' | '$' | '\\' => vec!['\\', c],
                    other => vec![other],
                })
                .collect();
            format!("\"{escaped}\"")
        }
        false => arg.to_string(),
    };
    quoted.replace('%', "%%").replace('\\', "\\\\")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{APP_ID, ENTRY, Install, entry_for, exec_arg, install};

    #[test]
    fn the_entry_runs_the_daemon_and_stays_visible() {
        assert_eq!(
            entry_for(Path::new("/home/me/bin/voice-input")),
            ENTRY.replace(
                "Exec=voice-input daemon",
                "Exec=/home/me/bin/voice-input daemon"
            )
        );
        assert!(!ENTRY.contains("NoDisplay"));
    }

    #[test]
    fn exec_arguments_are_quoted_per_the_desktop_entry_spec() {
        assert_eq!(
            [
                "/usr/bin/voice-input",
                "/home/me/my apps/voice-input",
                "/home/me/100%/voice-input",
                r#"/home/me/"q" $x \b/voice-input"#,
            ]
            .map(exec_arg),
            [
                "/usr/bin/voice-input",
                r#""/home/me/my apps/voice-input""#,
                "/home/me/100%%/voice-input",
                r#""/home/me/\\"q\\" \\$x \\\\b/voice-input""#,
            ]
        );
    }

    #[test]
    fn an_entry_found_anywhere_is_left_alone() {
        let root = std::env::temp_dir().join(format!("voice-input-desktop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, system) = (root.join("home"), root.join("system"));
        let program = Path::new("/home/me/bin/voice-input");
        let name = format!("{APP_ID}.desktop");

        let installed = install(&home, std::slice::from_ref(&system), program).unwrap();
        let again = install(&home, std::slice::from_ref(&system), program).unwrap();
        assert_eq!(
            (installed, again),
            (
                Install::Installed(home.join("applications").join(&name)),
                Install::Present(home.join("applications").join(&name)),
            )
        );

        fs::remove_dir_all(&home).unwrap();
        fs::create_dir_all(system.join("applications")).unwrap();
        fs::write(system.join("applications").join(&name), "packaged").unwrap();
        assert_eq!(
            install(
                &home,
                &[PathBuf::from("/nonexistent"), system.clone()],
                program
            )
            .unwrap(),
            Install::Present(system.join("applications").join(&name))
        );
        assert!(!home.join("applications").join(&name).exists());
    }

    #[test]
    fn a_stale_temp_file_does_not_stop_the_install() {
        let root =
            std::env::temp_dir().join(format!("voice-input-desktop-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let applications = root.join("home").join("applications");
        fs::create_dir_all(&applications).unwrap();
        let name = format!("{APP_ID}.desktop");
        fs::write(
            applications.join(format!(".{name}.{}.tmp", std::process::id())),
            "stale",
        )
        .unwrap();

        assert_eq!(
            install(&root.join("home"), &[], Path::new("/bin/voice-input")).unwrap(),
            Install::Installed(applications.join(&name))
        );
        assert_eq!(
            fs::read_to_string(applications.join(&name)).unwrap(),
            entry_for(Path::new("/bin/voice-input"))
        );
    }
}
