//! Writing the config file back from `limen shortcut` commands.
//!
//! Only keys of the `[shortcut]` section change; every other byte of the
//! file, comments included, stays as it was. The file is replaced atomically
//! and durably, keeping its mode, so a crash leaves either the old or the new
//! file. A symlinked config (e.g. managed by home-manager) or a read-only one
//! is never replaced: the user gets the section to add themselves.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use toml_edit::{ImDocument, Item, Value};

use super::{Config, DEFAULT_CONFIG, ShortcutConfig};

/// Why the config file must not be replaced.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Refusal {
    #[error("{} is a symlink, probably managed by a declarative setup such as home-manager", .0.display())]
    Symlink(PathBuf),
    #[error("{} is not writable", .0.display())]
    ReadOnly(PathBuf),
    #[error("cannot create a temporary file in {} to replace the config", .0.display())]
    UnwritableDirectory(PathBuf),
}

/// Check that the config file at `path` may be replaced. A missing file may
/// be created. Replacement needs both a writable file and a directory in
/// which its temporary file can be created.
pub(crate) fn check_writable(path: &Path) -> Result<(), Refusal> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Refusal::Symlink(path.to_path_buf()));
        }
        Ok(_) => match OpenOptions::new().write(true).open(path) {
            Ok(_) => {}
            Err(_) => return Err(Refusal::ReadOnly(path.to_path_buf())),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err(Refusal::ReadOnly(path.to_path_buf())),
    }
    let Some(dir) = path.parent() else {
        return Err(Refusal::ReadOnly(path.to_path_buf()));
    };
    fs::create_dir_all(dir).map_err(|_| Refusal::UnwritableDirectory(dir.to_path_buf()))?;
    let (temp_path, temp) =
        create_temp(path).map_err(|_| Refusal::UnwritableDirectory(dir.to_path_buf()))?;
    drop(temp);
    fs::remove_file(temp_path).map_err(|_| Refusal::UnwritableDirectory(dir.to_path_buf()))
}

/// The `[shortcut]` section as TOML, for users who must add it themselves.
pub(crate) fn snippet(shortcut: &ShortcutConfig) -> String {
    #[derive(Serialize)]
    struct Section<'a> {
        shortcut: &'a ShortcutConfig,
    }
    toml::to_string(&Section { shortcut }).expect("the shortcut section holds only strings")
}

/// Apply `change` to the `[shortcut]` section of the config file at `path`
/// and return the section it leaves. The change applies to the file as it is
/// now, under a lock on its directory, so concurrent commands never lose
/// each other's updates. A missing file is created from the annotated
/// default. Nothing is written when an existing section stays the same, or
/// when the edited file would no longer be a valid config.
pub(crate) fn update_shortcut(
    path: &Path,
    change: impl FnOnce(ShortcutConfig) -> ShortcutConfig,
) -> Result<ShortcutConfig> {
    let _lock = lock_dir(path)?;
    let (text, mode, current) = read_current(path)?;
    let shortcut = change(current.shortcut);
    check_writable(path)?;
    // A missing config is written even unchanged: setup leaves one behind.
    if shortcut == current.shortcut && mode.is_some() {
        return Ok(shortcut);
    }
    let edited = with_shortcut(&text, &current.shortcut, &shortcut)?;
    let config: Config = toml::from_str(&edited).context("the edited config does not parse")?;
    config.validate()?;
    if config.shortcut != shortcut {
        bail!(
            "the [shortcut] section of {} could not be edited",
            path.display()
        );
    }
    replace(path, &edited, mode)?;
    Ok(shortcut)
}

/// The config file at `path` as text, its mode, and the valid config it
/// holds. A missing file reads as the annotated default, without a mode.
/// Callers hold the directory lock.
pub(super) fn read_current(path: &Path) -> Result<(String, Option<u32>, Config)> {
    let (text, mode) = match fs::read_to_string(path) {
        Ok(text) => (text, Some(fs::metadata(path)?.permissions().mode())),
        Err(error) if error.kind() == ErrorKind::NotFound => (DEFAULT_CONFIG.to_string(), None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let current: Config = toml::from_str(&text)
        .with_context(|| format!("failed to parse config: {}", path.display()))?;
    current.validate()?;
    Ok((text, mode, current))
}

/// Write the annotated default config to `path`, unless a config appeared
/// there meanwhile, e.g. written by a concurrent `limen shortcut`.
pub(super) fn create_default(path: &Path) -> Result<()> {
    let _lock = lock_dir(path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            replace(path, DEFAULT_CONFIG, None)?;
            eprintln!("wrote default config: {}", path.display());
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        Ok(_) => Ok(()),
    }
}

/// Lock the directory of the config file at `path`, creating it, against
/// other writers of the config. Held until the returned file is dropped.
pub(super) fn lock_dir(path: &Path) -> Result<File> {
    let dir = path
        .parent()
        .context("config path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let lock = File::open(dir).with_context(|| format!("failed to open {}", dir.display()))?;
    lock.lock()
        .with_context(|| format!("failed to lock {}", dir.display()))?;
    Ok(lock)
}

/// `text`, whose `[shortcut]` section reads as `current`, with that section
/// set to `wanted`. Only values that change are replaced, and missing keys
/// are added after the section's last value, so every other byte of the
/// file stays as written, line endings included.
fn with_shortcut(text: &str, current: &ShortcutConfig, wanted: &ShortcutConfig) -> Result<String> {
    let fields = |config: &ShortcutConfig| {
        [
            ("backend", config.backend.to_string()),
            ("toggle", config.toggle.to_string()),
            ("hold", config.hold.to_string()),
        ]
    };
    let line = |key: &str, value: &str, newline: &str| {
        format!(
            "{key} = {}{newline}",
            toml::Value::String(value.to_string())
        )
    };
    let document = ImDocument::parse(text).context("the config is not valid TOML")?;
    let splices = match document.get("shortcut") {
        None => {
            let (_, newline) = line_end(text, text.len());
            let lines: String = fields(wanted)
                .iter()
                .map(|(key, value)| line(key, value, newline))
                .collect();
            let separator = match text.is_empty() || text.ends_with('\n') {
                true => "",
                false => newline,
            };
            vec![(
                text.len()..text.len(),
                format!("{separator}{newline}[shortcut]{newline}{lines}"),
            )]
        }
        Some(Item::Table(section)) if !section.is_dotted() => {
            let mut splices = Vec::new();
            let mut missing = String::new();
            let last = section
                .iter()
                .filter_map(|(_, item)| item.as_value().and_then(Value::span))
                .chain(section.span())
                .map(|span| span.end)
                .max()
                .context("the [shortcut] section has no position")?;
            let (after_last, newline) = line_end(text, last);
            for ((key, value), (_, old)) in fields(wanted).into_iter().zip(fields(current)) {
                match section.get(key).and_then(Item::span) {
                    Some(_) if value == old => {}
                    Some(span) => splices.push((span, toml::Value::String(value).to_string())),
                    None => missing.push_str(&line(key, &value, newline)),
                }
            }
            let separator = match after_last == text.len() && !text.ends_with('\n') {
                true => newline,
                false => "",
            };
            splices.push((after_last..after_last, format!("{separator}{missing}")));
            splices
        }
        Some(_) => bail!("`shortcut` in the config must be a [shortcut] section"),
    };
    // From the end of the file backwards, so that earlier offsets stay valid.
    let mut splices = splices;
    splices.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut edited = text.to_string();
    for (span, replacement) in splices {
        edited.replace_range(span, &replacement);
    }
    Ok(edited)
}

/// Where the line holding byte `index` ends, just past its newline, and that
/// newline. Past the last line, the newline of the file's last line.
fn line_end(text: &str, index: usize) -> (usize, &'static str) {
    let newline = |at: usize| match text[..at].ends_with('\r') {
        true => "\r\n",
        false => "\n",
    };
    match text[index..].find('\n') {
        Some(offset) => (index + offset + 1, newline(index + offset)),
        None => (text.len(), text.rfind('\n').map_or("\n", newline)),
    }
}

/// Replace `path` with `contents`: a unique temp file next to it gets `mode`,
/// the contents, and an fsync, then is renamed over `path`, and the directory
/// is fsynced so that the rename survives a crash too.
pub(super) fn replace(path: &Path, contents: &str, mode: Option<u32>) -> Result<()> {
    let dir = path
        .parent()
        .context("config path has no parent directory")?;
    let (temp_path, mut temp) = create_temp(path)?;
    let written = (|| -> Result<()> {
        if let Some(mode) = mode {
            temp.set_permissions(Permissions::from_mode(mode))?;
        }
        temp.write_all(contents.as_bytes())?;
        temp.sync_all()?;
        // A symlink that appeared since the caller checked would be replaced by
        // the rename: look once more, as late as possible.
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Refusal::Symlink(path.to_path_buf()).into());
            }
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
            Ok(_) | Err(_) => {}
        }
        fs::rename(&temp_path, path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    written
        .inspect_err(|_| {
            let _ = fs::remove_file(&temp_path);
        })
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Create a temp file next to `path` that no one else has opened.
fn create_temp(path: &Path) -> Result<(PathBuf, File)> {
    let name = path
        .file_name()
        .context("config path has no file name")?
        .to_string_lossy();
    let pid = std::process::id();
    let mut attempt = 0;
    loop {
        let temp_path = path.with_file_name(format!(".{name}.{pid}.{attempt}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists && attempt < 100 => {
                attempt += 1;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create a temp file next to {}", path.display())
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use super::{Refusal, check_writable, snippet, update_shortcut, with_shortcut};
    use crate::config::{Config, DEFAULT_CONFIG, Paths, ShortcutConfig};
    use crate::shortcut::{Action, Backend};

    const LEGACY: &str = r#"# my config
[provider]
url = "http://127.0.0.1:8000/v1/audio/transcriptions"  # local server
model = "test-model"
"#;

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn gnome(toggle: &str) -> ShortcutConfig {
        ShortcutConfig {
            backend: Backend::GnomeKeybinding,
            toggle: toggle.parse().unwrap(),
            ..ShortcutConfig::default()
        }
    }

    #[test]
    fn edits_change_only_the_shortcut_keys_that_differ() {
        assert_eq!(
            with_shortcut(
                DEFAULT_CONFIG,
                &ShortcutConfig::default(),
                &gnome("Ctrl+Alt+V")
            )
            .unwrap(),
            DEFAULT_CONFIG
                .replace(r#"backend = "none""#, r#"backend = "gnome-keybinding""#)
                .replace(r#"toggle = "Super+Z""#, r#"toggle = "Ctrl+Alt+V""#)
        );
    }

    #[test]
    fn edited_values_keep_their_spacing_and_comment() {
        let text = format!(
            "{LEGACY}\n[shortcut]\nbackend   =   'none'   # pick one\ntoggle = \"super+z\"\n"
        );
        assert_eq!(
            with_shortcut(&text, &ShortcutConfig::default(), &gnome("Super+Z")).unwrap(),
            format!(
                "{LEGACY}\n[shortcut]\nbackend   =   \"gnome-keybinding\"   # pick one\ntoggle = \"super+z\"\nhold = \"Super+X\"\n"
            )
        );
    }

    #[test]
    fn a_config_without_the_section_gets_all_of_it() {
        assert_eq!(
            with_shortcut(LEGACY, &ShortcutConfig::default(), &gnome("Super+Z")).unwrap(),
            format!("{LEGACY}{}", "\n".to_string() + &snippet(&gnome("Super+Z")))
        );
    }

    #[test]
    fn the_snippet_is_the_whole_section() {
        assert_eq!(
            snippet(&gnome("Ctrl+Alt+V")),
            "[shortcut]\nbackend = \"gnome-keybinding\"\ntoggle = \"Ctrl+Alt+V\"\nhold = \"Super+X\"\n"
        );
    }

    #[test]
    fn write_back_keeps_the_mode_and_leaves_no_temp_file() {
        let dir = scratch("write-back");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        assert_eq!(
            update_shortcut(&path, |_| gnome("Ctrl+Alt+V")).unwrap(),
            gnome("Ctrl+Alt+V")
        );

        assert_eq!(
            (
                fs::read_to_string(&path).unwrap(),
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                entries(&dir),
            ),
            (
                with_shortcut(
                    DEFAULT_CONFIG,
                    &ShortcutConfig::default(),
                    &gnome("Ctrl+Alt+V")
                )
                .unwrap(),
                0o640,
                vec!["config.toml".to_string()],
            )
        );
    }

    #[test]
    fn symlinked_and_read_only_configs_are_refused() {
        let dir = scratch("refused");
        let target = dir.join("managed.toml");
        fs::write(&target, DEFAULT_CONFIG).unwrap();
        let link = dir.join("config.toml");
        symlink(&target, &link).unwrap();
        let dangling = dir.join("dangling.toml");
        symlink(dir.join("missing.toml"), &dangling).unwrap();
        let read_only = dir.join("read-only.toml");
        fs::write(&read_only, DEFAULT_CONFIG).unwrap();
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o444)).unwrap();

        assert_eq!(
            [&link, &dangling, &read_only, &dir.join("new.toml")].map(|path| check_writable(path)),
            [
                Err(Refusal::Symlink(link.clone())),
                Err(Refusal::Symlink(dangling.clone())),
                Err(Refusal::ReadOnly(read_only.clone())),
                Ok(()),
            ]
        );
        for path in [&link, &read_only] {
            let error = update_shortcut(path, |_| gnome("Ctrl+Alt+V")).unwrap_err();
            assert!(error.downcast_ref::<Refusal>().is_some(), "{error:#}");
        }
        assert_eq!(
            [&target, &read_only].map(|path| fs::read_to_string(path).unwrap()),
            [DEFAULT_CONFIG, DEFAULT_CONFIG]
        );
    }

    #[test]
    fn invalid_results_are_never_written() {
        let dir = scratch("invalid");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();
        let clash = ShortcutConfig {
            hold: "Super+Z".parse().unwrap(),
            ..gnome("Super+Z")
        };

        assert!(update_shortcut(&path, |_| clash).is_err());
        assert_eq!(
            (fs::read_to_string(&path).unwrap(), entries(&dir)),
            (DEFAULT_CONFIG.to_string(), vec!["config.toml".to_string()])
        );
    }

    fn paths(config_file: PathBuf) -> Paths {
        Paths {
            socket: config_file.with_file_name("limen.sock"),
            log_file: config_file.with_file_name("daemon.log"),
            cache_dir: config_file.with_file_name("cache"),
            config_file,
        }
    }

    #[test]
    fn first_run_writes_the_default_config_atomically() {
        let dir = scratch("first-run");
        let config_dir = dir.join("limen");

        Config::load_or_create(&paths(config_dir.join("config.toml"))).unwrap();

        assert_eq!(
            (
                fs::read_to_string(config_dir.join("config.toml")).unwrap(),
                entries(&config_dir),
            ),
            (DEFAULT_CONFIG.to_string(), vec!["config.toml".to_string()])
        );
    }

    #[test]
    fn first_run_leaves_a_dangling_config_symlink_alone() {
        let dir = scratch("dangling");
        let link = dir.join("config.toml");
        symlink(dir.join("missing.toml"), &link).unwrap();

        assert!(Config::load_or_create(&paths(link.clone())).is_err());
        assert_eq!(
            (
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                entries(&dir),
            ),
            (true, vec!["config.toml".to_string()])
        );
    }

    #[test]
    fn an_unchanged_symlink_is_refused_without_being_written() {
        let dir = scratch("unchanged");
        let target = dir.join("managed.toml");
        fs::write(&target, DEFAULT_CONFIG).unwrap();
        let link = dir.join("config.toml");
        symlink(&target, &link).unwrap();

        assert!(update_shortcut(&link, |current| current).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn a_missing_config_starts_from_the_default() {
        let dir = scratch("missing");
        let path = dir.join("limen").join("config.toml");

        update_shortcut(&path, |_| gnome("Ctrl+Alt+V")).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            with_shortcut(
                DEFAULT_CONFIG,
                &ShortcutConfig::default(),
                &gnome("Ctrl+Alt+V")
            )
            .unwrap()
        );
    }

    #[test]
    fn concurrent_updates_keep_each_other() {
        let dir = scratch("concurrent");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();
        let toggles: Vec<_> = (1..=12).map(|n| format!("Ctrl+F{n}")).collect();
        let holds: Vec<_> = ('a'..='l').map(|key| format!("Ctrl+Alt+{key}")).collect();

        std::thread::scope(|scope| {
            for (action, values) in [(Action::Toggle, &toggles), (Action::Hold, &holds)] {
                let path = &path;
                scope.spawn(move || {
                    for value in values {
                        let shortcut = value.parse().unwrap();
                        update_shortcut(path, |current| match action {
                            Action::Toggle => ShortcutConfig {
                                toggle: shortcut,
                                ..current
                            },
                            Action::Hold => ShortcutConfig {
                                hold: shortcut,
                                ..current
                            },
                        })
                        .unwrap();
                    }
                });
            }
        });

        let config: Config = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            config.shortcut,
            ShortcutConfig {
                toggle: "Ctrl+F12".parse().unwrap(),
                hold: "Ctrl+Alt+L".parse().unwrap(),
                ..ShortcutConfig::default()
            }
        );
    }

    #[test]
    fn crlf_configs_keep_their_line_endings() {
        let crlf = DEFAULT_CONFIG.replace('\n', "\r\n");
        assert_eq!(
            with_shortcut(&crlf, &ShortcutConfig::default(), &gnome("Ctrl+Alt+V")).unwrap(),
            with_shortcut(
                DEFAULT_CONFIG,
                &ShortcutConfig::default(),
                &gnome("Ctrl+Alt+V")
            )
            .unwrap()
            .replace('\n', "\r\n")
        );
    }

    #[test]
    fn mixed_line_endings_stay_as_they_are() {
        let text = "[provider]\r\nurl = \"http://127.0.0.1:8000/v1\"\nmodel = \"m\"\r\n\r\n[shortcut]\nbackend = \"none\"\r\ntoggle = \"Super+Z\"\r\n";
        assert_eq!(
            with_shortcut(text, &ShortcutConfig::default(), &gnome("Super+Z")).unwrap(),
            "[provider]\r\nurl = \"http://127.0.0.1:8000/v1\"\nmodel = \"m\"\r\n\r\n[shortcut]\nbackend = \"gnome-keybinding\"\r\ntoggle = \"Super+Z\"\r\nhold = \"Super+X\"\r\n"
        );
    }

    #[test]
    fn a_missing_final_newline_is_added_before_new_lines() {
        assert_eq!(
            [
                with_shortcut(
                    "[shortcut]\nbackend = \"none\"",
                    &ShortcutConfig::default(),
                    &gnome("Super+Z")
                )
                .unwrap(),
                with_shortcut("x = 1", &ShortcutConfig::default(), &gnome("Super+Z")).unwrap(),
            ],
            [
                "[shortcut]\nbackend = \"gnome-keybinding\"\ntoggle = \"Super+Z\"\nhold = \"Super+X\"\n"
                    .to_string(),
                format!("x = 1\n\n{}", snippet(&gnome("Super+Z"))),
            ]
        );
    }

    #[test]
    fn a_config_that_appeared_meanwhile_is_not_replaced_by_the_default() {
        let dir = scratch("appeared");
        let path = dir.join("config.toml");
        fs::write(&path, LEGACY).unwrap();

        super::create_default(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), LEGACY);
    }

    #[test]
    fn an_invalid_config_is_reported_even_when_nothing_changes() {
        let dir = scratch("invalid-unchanged");
        let path = dir.join("config.toml");
        fs::write(
            &path,
            DEFAULT_CONFIG.replace("chunk_ms = 100", "chunk_ms = 0"),
        )
        .unwrap();

        assert!(update_shortcut(&path, |current| current).is_err());
    }

    #[test]
    fn keys_in_any_order_change_in_place() {
        let text = format!("{LEGACY}\n[shortcut]\ntoggle = \"Super+Z\"\nbackend = \"none\"\n");
        assert_eq!(
            with_shortcut(&text, &ShortcutConfig::default(), &gnome("Ctrl+Alt+V")).unwrap(),
            format!(
                "{LEGACY}\n[shortcut]\ntoggle = \"Ctrl+Alt+V\"\nbackend = \"gnome-keybinding\"\nhold = \"Super+X\"\n"
            )
        );
    }

    #[test]
    fn an_unchanged_section_still_creates_a_missing_config() {
        let dir = scratch("unchanged-missing");
        let path = dir.join("config.toml");

        update_shortcut(&path, |current| current).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
    }

    #[test]
    fn replacement_requires_a_writable_directory() {
        let dir = scratch("readonly-dir");
        let path = dir.join("config.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        let existing = check_writable(&path);
        let missing = check_writable(&dir.join("new.toml"));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            existing.is_err(),
            "a writable file in a read-only directory passed"
        );
        assert!(
            missing.is_err(),
            "a missing config in a read-only directory passed"
        );
    }
}
