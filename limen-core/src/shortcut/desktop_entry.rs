//! The desktop entry that names the app to the desktop. The GlobalShortcuts
//! portal registers only an app ID that matches an installed `.desktop`
//! file, and GNOME Settings lists the app's shortcuts under it, so the entry
//! must not be hidden with `NoDisplay`.
//!
//! The Nix package installs the entry itself. Elsewhere, `limen
//! shortcut setup portal` writes one to `$XDG_DATA_HOME/applications`, with
//! its icon in the user's hicolor theme, unless the data directories already
//! have an entry with this name.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// The app ID the portal knows the daemon by.
pub const APP_ID: &str = "io.github.Emin017.Limen";

/// The entry as it is kept in the repository.
const ENTRY: &str = include_str!("../../../data/io.github.Emin017.Limen.desktop");

/// The icon the entry names, installed beside an entry written here.
const ICON: &str = include_str!("../../../data/io.github.Emin017.Limen.svg");

/// Which command a non-Nix desktop entry launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Open the graphical control panel.
    Gui,
    /// Run the resident daemon.
    Daemon,
}

impl Launch {
    fn subcommand(self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Daemon => "daemon",
        }
    }
}

/// Where the entry was found or put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Install {
    Present(PathBuf),
    Installed(PathBuf),
}

/// Make sure an entry for [`APP_ID`] is installed, writing one that runs
/// `program` with the selected subcommand when none is found.
pub(crate) fn ensure_installed(program: &Path, launch: Launch) -> Result<Install> {
    let home = dirs::data_dir().context("cannot find the user's data directory")?;
    let system = std::env::var_os("XDG_DATA_DIRS")
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    install(
        &home,
        &std::env::split_paths(&system).collect::<Vec<_>>(),
        program,
        launch,
    )
}

fn install(home: &Path, system: &[PathBuf], program: &Path, launch: Launch) -> Result<Install> {
    let name = format!("{APP_ID}.desktop");
    let found = std::iter::once(home)
        .chain(system.iter().map(PathBuf::as_path))
        .map(|dir| dir.join("applications").join(&name))
        .find_map(|path| match fs::symlink_metadata(&path) {
            Ok(_) if path.is_file() => Some(Ok(Install::Present(path))),
            Ok(_) => Some(Err(anyhow!(
                "{} exists but is not a desktop file (dangling symlink or non-file); move it aside and run setup again",
                path.display()
            ))),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => Some(Err(error).with_context(|| {
                format!("failed to inspect {}", path.display())
            })),
        });
    match found {
        Some(result) => result,
        None => {
            // The icon goes in first, so the entry never names a missing one.
            create(
                &home.join("icons/hicolor/scalable/apps"),
                &format!("{APP_ID}.svg"),
                ICON,
            )?;
            create(
                &home.join("applications"),
                &name,
                &entry_for(program, launch),
            )
        }
    }
}

/// Write `contents` to `dir/name` unless a file is already there.
fn create(dir: &Path, name: &str, contents: &str) -> Result<Install> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join(name);
    // Written in full under a temporary name, then linked into place: the
    // file never shows half written, and one that another process created
    // meanwhile is never replaced.
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    // Only a dead process can have left a file under our process ID.
    let _ = fs::remove_file(&temp);
    let linked = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| match fs::hard_link(&temp, &path) {
            Ok(()) => Ok(Install::Installed(path.clone())),
            Err(error) if error.kind() == ErrorKind::AlreadyExists && path.is_file() => {
                Ok(Install::Present(path.clone()))
            }
            Err(error) => Err(error),
        });
    let _ = fs::remove_file(&temp);
    linked.with_context(|| format!("failed to write {}", path.display()))
}

/// The entry, with `Exec` running `program` with the selected subcommand.
fn entry_for(program: &Path, launch: Launch) -> String {
    let exec = format!(
        "Exec={} {}",
        exec_arg(&program.to_string_lossy()),
        launch.subcommand()
    );
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
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    use super::{APP_ID, ENTRY, ICON, Install, Launch, entry_for, exec_arg, install};

    #[test]
    fn the_entry_runs_the_daemon_and_stays_visible() {
        assert_eq!(
            entry_for(Path::new("/home/me/bin/limen"), Launch::Daemon),
            ENTRY.replace("Exec=limen daemon", "Exec=/home/me/bin/limen daemon")
        );
        assert!(!ENTRY.contains("NoDisplay"));
        assert_eq!(
            entry_for(Path::new("/home/me/bin/limen"), Launch::Gui),
            ENTRY.replace("Exec=limen daemon", "Exec=/home/me/bin/limen gui")
        );
    }

    #[test]
    fn exec_arguments_are_quoted_per_the_desktop_entry_spec() {
        assert_eq!(
            [
                "/usr/bin/limen",
                "/home/me/my apps/limen",
                "/home/me/100%/limen",
                r#"/home/me/"q" $x \b/limen"#,
            ]
            .map(exec_arg),
            [
                "/usr/bin/limen",
                r#""/home/me/my apps/limen""#,
                "/home/me/100%%/limen",
                r#""/home/me/\\"q\\" \\$x \\\\b/limen""#,
            ]
        );
    }

    #[test]
    fn an_entry_found_anywhere_is_left_alone() {
        let root = std::env::temp_dir().join(format!("limen-desktop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, system) = (root.join("home"), root.join("system"));
        let program = Path::new("/home/me/bin/limen");
        let name = format!("{APP_ID}.desktop");

        let installed = install(
            &home,
            std::slice::from_ref(&system),
            program,
            Launch::Daemon,
        )
        .unwrap();
        let again = install(
            &home,
            std::slice::from_ref(&system),
            program,
            Launch::Daemon,
        )
        .unwrap();
        assert_eq!(
            (installed, again),
            (
                Install::Installed(home.join("applications").join(&name)),
                Install::Present(home.join("applications").join(&name)),
            )
        );
        assert_eq!(
            fs::read_to_string(home.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")))
                .unwrap(),
            ICON
        );

        fs::remove_dir_all(&home).unwrap();
        fs::create_dir_all(system.join("applications")).unwrap();
        fs::write(system.join("applications").join(&name), "packaged").unwrap();
        assert_eq!(
            install(
                &home,
                &[PathBuf::from("/nonexistent"), system.clone()],
                program,
                Launch::Daemon,
            )
            .unwrap(),
            Install::Present(system.join("applications").join(&name))
        );
        assert!(!home.join("applications").join(&name).exists());
    }

    #[test]
    fn a_stale_temp_file_does_not_stop_the_install() {
        let root = std::env::temp_dir().join(format!("limen-desktop-stale-{}", std::process::id()));
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
            install(
                &root.join("home"),
                &[],
                Path::new("/bin/limen"),
                Launch::Daemon
            )
            .unwrap(),
            Install::Installed(applications.join(&name))
        );
        assert_eq!(
            fs::read_to_string(applications.join(&name)).unwrap(),
            entry_for(Path::new("/bin/limen"), Launch::Daemon)
        );
    }

    #[test]
    fn a_dangling_entry_symlink_is_an_error_not_an_installation() {
        let root =
            std::env::temp_dir().join(format!("limen-desktop-dangling-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let applications = root.join("home").join("applications");
        fs::create_dir_all(&applications).unwrap();
        let path = applications.join(format!("{APP_ID}.desktop"));
        symlink(root.join("missing.desktop"), &path).unwrap();

        let error = install(
            &root.join("home"),
            &[],
            Path::new("/bin/limen"),
            Launch::Daemon,
        )
        .unwrap_err();
        assert!(error.to_string().contains("dangling symlink"), "{error:#}");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
