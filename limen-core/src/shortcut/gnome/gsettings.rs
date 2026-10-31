//! Reading and writing GNOME's custom keybindings through `gsettings`.
//!
//! The list of custom keybindings is shared, and `gsettings` offers no
//! compare-and-swap. Setup reads the list and every entry, plans its change,
//! and re-reads the list before its first write: if anything changed, it
//! writes nothing. Our entry's fields are written next, and the list is
//! re-read once more before the list itself is written; if it changed, the
//! list is left alone and the fields stay at our own path, where the next
//! setup overwrites them. A change between that last read and the list write
//! can still be lost.

use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::{Clear, Conflict, CustomKeybinding, Setup, conflicts, gvariant};
use crate::shortcut::Shortcut;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_KEYBINDING: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const LIST: &str = "custom-keybindings";

/// The schemas whose keybindings a new shortcut must not collide with.
const KEYBINDING_SCHEMAS: [&str; 5] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    MEDIA_KEYS,
];

/// How long one `gsettings` call may take.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Reads and writes GNOME settings. The CLI goes through the `gsettings`
/// command; tests use an in-memory fake to check what is written when.
/// Values are GVariant text, the way `gsettings` prints and parses them.
pub(crate) trait Dconf {
    /// The value of `key`. `schema` may name a relocatable schema with its
    /// path, as `schema:/path/`.
    fn get(&self, schema: &str, key: &str) -> impl Future<Output = Result<String>> + Send;

    fn set(&self, schema: &str, key: &str, value: &str) -> impl Future<Output = Result<()>> + Send;

    /// Restore the default value of `key`.
    fn reset(&self, schema: &str, key: &str) -> impl Future<Output = Result<()>> + Send;

    /// Every key of `schema`, one `schema key value` line each.
    fn list(&self, schema: &str) -> impl Future<Output = Result<String>> + Send;

    /// The installed schemas that are not relocatable, one per line.
    fn schemas(&self) -> impl Future<Output = Result<String>> + Send;
}

/// The `gsettings` command.
pub(crate) struct Gsettings;

impl Dconf for Gsettings {
    async fn get(&self, schema: &str, key: &str) -> Result<String> {
        gsettings(&["get", schema, key]).await
    }

    async fn set(&self, schema: &str, key: &str, value: &str) -> Result<()> {
        gsettings(&["set", schema, key, value]).await.map(drop)
    }

    async fn reset(&self, schema: &str, key: &str) -> Result<()> {
        gsettings(&["reset", schema, key]).await.map(drop)
    }

    async fn list(&self, schema: &str) -> Result<String> {
        gsettings(&["list-recursively", schema]).await
    }

    async fn schemas(&self) -> Result<String> {
        gsettings(&["list-schemas"]).await
    }
}

async fn gsettings(args: &[&str]) -> Result<String> {
    let child = Command::new("gsettings")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("cannot run gsettings; the gnome-keybinding backend needs GNOME")?;
    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .context("gsettings did not answer")??;
    match output.status.success() {
        true => Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()),
        false => bail!(
            "gsettings {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

/// The custom keybindings: the list, and the entries it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keybindings {
    pub(crate) list: Vec<String>,
    pub(crate) entries: Vec<CustomKeybinding>,
}

pub(crate) async fn read(dconf: &impl Dconf) -> Result<Keybindings> {
    let list = read_list(dconf).await?;
    let mut entries = Vec::new();
    for path in &list {
        entries.push(read_entry(dconf, path).await?);
    }
    Ok(Keybindings { list, entries })
}

async fn read_list(dconf: &impl Dconf) -> Result<Vec<String>> {
    let value = dconf.get(MEDIA_KEYS, LIST).await?;
    gvariant::parse_strings(&value).with_context(|| format!("unexpected {LIST} value {value}"))
}

async fn read_entry(dconf: &impl Dconf, path: &str) -> Result<CustomKeybinding> {
    let listing = dconf.list(&entry_schema(path)).await?;
    let field = |name: &str| {
        listing
            .lines()
            .find_map(|line| match line.splitn(3, ' ').collect::<Vec<_>>()[..] {
                [_, key, value] if key == name => gvariant::parse_string(value),
                _ => None,
            })
            .unwrap_or_default()
    };
    Ok(CustomKeybinding {
        path: path.to_string(),
        name: field("name"),
        command: field("command"),
        binding: field("binding"),
    })
}

fn entry_schema(path: &str) -> String {
    format!("{CUSTOM_KEYBINDING}:{path}")
}

/// Fail unless the list is still `expected`.
async fn check_list(dconf: &impl Dconf, expected: &[String]) -> Result<()> {
    match read_list(dconf).await? == expected {
        true => Ok(()),
        false => bail!("the GNOME custom shortcuts changed meanwhile; run the command again"),
    }
}

/// Write `setup`, planned from `read`. If anything fails after the first
/// field write, its partial changes are rolled back before returning the
/// error. A failure of the pre-write list check writes and rolls back nothing.
pub(crate) async fn apply_setup(
    dconf: &impl Dconf,
    read: &Keybindings,
    setup: &Setup,
) -> Result<()> {
    check_list(dconf, &read.list).await?;
    let written = async {
        let schema = entry_schema(&setup.path);
        if let Some(name) = &setup.name {
            dconf.set(&schema, "name", &gvariant::string(name)).await?;
        }
        dconf
            .set(&schema, "command", &gvariant::string(&setup.command))
            .await?;
        dconf
            .set(&schema, "binding", &gvariant::string(&setup.binding))
            .await?;
        // Also for an entry already listed: it must still be listed now.
        check_list(dconf, &read.list).await?;
        match &setup.list {
            Some(list) => dconf.set(MEDIA_KEYS, LIST, &gvariant::strings(list)).await,
            None => Ok(()),
        }
    }
    .await;
    match written {
        Ok(()) => Ok(()),
        Err(error) => match rollback_setup(dconf, read, setup).await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(error.context(format!(
                "rolling back the GNOME shortcut also failed: {rollback_error:#}"
            ))),
        },
    }
}

/// Undo the parts of `setup` that may have been written before a failure.
/// Other applications' list changes are preserved. A new entry is disabled
/// before removing only its path; an existing entry gets its old fields back.
async fn rollback_setup(dconf: &impl Dconf, read: &Keybindings, setup: &Setup) -> Result<()> {
    let schema = entry_schema(&setup.path);
    match read.entries.iter().find(|entry| entry.path == setup.path) {
        Some(original) => {
            if setup.name.is_some() && setup.name.as_deref() != Some(original.name.as_str()) {
                dconf
                    .set(&schema, "name", &gvariant::string(&original.name))
                    .await?;
            }
            if setup.command != original.command {
                dconf
                    .set(&schema, "command", &gvariant::string(&original.command))
                    .await?;
            }
            if setup.binding != original.binding {
                dconf
                    .set(&schema, "binding", &gvariant::string(&original.binding))
                    .await?;
            }
            Ok(())
        }
        None => {
            dconf.set(&schema, "binding", &gvariant::string("")).await?;
            let current = read_list(dconf).await?;
            if current.contains(&setup.path) {
                let remaining: Vec<String> = current
                    .into_iter()
                    .filter(|path| *path != setup.path)
                    .collect();
                dconf
                    .set(MEDIA_KEYS, LIST, &gvariant::strings(&remaining))
                    .await?;
            }
            Ok(())
        }
    }
}

/// Carry out `clear`, planned from `read`.
pub(crate) async fn apply_clear(
    dconf: &impl Dconf,
    read: &Keybindings,
    clear: &Clear,
) -> Result<()> {
    match clear {
        Clear::Nothing => Ok(()),
        Clear::Disable { path } => {
            check_list(dconf, &read.list).await?;
            dconf
                .set(&entry_schema(path), "binding", &gvariant::string(""))
                .await
        }
        Clear::Remove { path, list } => {
            check_list(dconf, &read.list).await?;
            let schema = entry_schema(path);
            for key in ["name", "command", "binding"] {
                dconf.reset(&schema, key).await?;
            }
            check_list(dconf, &read.list).await?;
            dconf.set(MEDIA_KEYS, LIST, &gvariant::strings(list)).await
        }
    }
}

/// What already uses `shortcut` in GNOME's keybinding schemas and `others`,
/// and the schemas left unchecked because this GNOME version lacks them.
pub(crate) async fn find_conflicts(
    dconf: &impl Dconf,
    shortcut: Shortcut,
    others: &[CustomKeybinding],
) -> Result<(Vec<Conflict>, Vec<&'static str>)> {
    let installed = dconf.schemas().await?;
    let (present, missing): (Vec<_>, Vec<_>) = KEYBINDING_SCHEMAS
        .into_iter()
        .partition(|schema| installed.lines().any(|line| line == *schema));
    let mut listing = String::new();
    for schema in present {
        listing.push_str(&dconf.list(schema).await?);
        listing.push('\n');
    }
    Ok((conflicts(shortcut, &listing, others), missing))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use anyhow::Result;

    use super::{
        CUSTOM_KEYBINDING, Dconf, KEYBINDING_SCHEMAS, Keybindings, LIST, MEDIA_KEYS, apply_clear,
        apply_setup, find_conflicts, read,
    };
    use crate::shortcut::gnome::tests::{PREFIX, paths, wechat};
    use crate::shortcut::gnome::{
        Clear, Conflict, CustomKeybinding, OUR_PATH, clear, gvariant, setup,
    };

    /// dconf in memory. `changes` replaces the list on the given read of it,
    /// standing in for another program that edits the list meanwhile.
    #[derive(Default)]
    struct FakeDconf {
        values: Mutex<BTreeMap<(String, String), String>>,
        list_reads: Mutex<usize>,
        changes: BTreeMap<usize, String>,
        writes: Mutex<Vec<String>>,
    }

    impl FakeDconf {
        fn with(entries: &[CustomKeybinding]) -> Self {
            let fake = Self::default();
            let paths: Vec<String> = entries.iter().map(|entry| entry.path.clone()).collect();
            fake.put(MEDIA_KEYS, LIST, &gvariant::strings(&paths));
            for entry in entries {
                let schema = format!("{CUSTOM_KEYBINDING}:{}", entry.path);
                fake.put(&schema, "name", &gvariant::string(&entry.name));
                fake.put(&schema, "command", &gvariant::string(&entry.command));
                fake.put(&schema, "binding", &gvariant::string(&entry.binding));
            }
            fake
        }

        fn put(&self, schema: &str, key: &str, value: &str) {
            self.values
                .lock()
                .unwrap()
                .insert((schema.to_string(), key.to_string()), value.to_string());
        }

        fn value(&self, schema: &str, key: &str) -> String {
            self.values
                .lock()
                .unwrap()
                .get(&(schema.to_string(), key.to_string()))
                .cloned()
                .unwrap_or_else(|| "''".to_string())
        }

        fn writes(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }

        async fn state(&self) -> Keybindings {
            read(self).await.unwrap()
        }
    }

    impl Dconf for FakeDconf {
        async fn get(&self, schema: &str, key: &str) -> Result<String> {
            if (schema, key) == (MEDIA_KEYS, LIST) {
                let mut reads = self.list_reads.lock().unwrap();
                *reads += 1;
                if let Some(list) = self.changes.get(&reads) {
                    self.put(MEDIA_KEYS, LIST, list);
                }
            }
            Ok(self.value(schema, key))
        }

        async fn set(&self, schema: &str, key: &str, value: &str) -> Result<()> {
            self.writes
                .lock()
                .unwrap()
                .push(format!("set {key} {value}"));
            self.put(schema, key, value);
            Ok(())
        }

        async fn reset(&self, schema: &str, key: &str) -> Result<()> {
            self.writes.lock().unwrap().push(format!("reset {key}"));
            self.values
                .lock()
                .unwrap()
                .remove(&(schema.to_string(), key.to_string()));
            Ok(())
        }

        async fn schemas(&self) -> Result<String> {
            Ok(KEYBINDING_SCHEMAS
                .into_iter()
                .filter(|schema| *schema != "org.gnome.mutter.wayland.keybindings")
                .collect::<Vec<_>>()
                .join("\n"))
        }

        async fn list(&self, schema: &str) -> Result<String> {
            match schema {
                "org.gnome.shell.keybindings" => Ok(format!(
                    "{schema} toggle-message-tray ['<Super>v', '<Super>m']"
                )),
                schema if schema.starts_with(CUSTOM_KEYBINDING) => {
                    Ok(["name", "command", "binding"]
                        .map(|key| format!("{CUSTOM_KEYBINDING} {key} {}", self.value(schema, key)))
                        .join("\n"))
                }
                _ => Ok(String::new()),
            }
        }
    }

    fn own(binding: &str) -> CustomKeybinding {
        CustomKeybinding {
            path: OUR_PATH.to_string(),
            name: "Limen dictation".to_string(),
            command: "/bin/limen toggle".to_string(),
            binding: binding.to_string(),
        }
    }

    async fn set_up(dconf: &FakeDconf) -> Result<()> {
        let before = read(dconf).await?;
        let plan = setup(
            &before.list,
            &before.entries,
            "Super+Z".parse().unwrap(),
            "/bin/limen toggle".to_string(),
        );
        apply_setup(dconf, &before, &plan).await
    }

    #[tokio::test]
    async fn setup_keeps_the_other_entries_and_repeats_cleanly() {
        let dconf = FakeDconf::with(&wechat());
        set_up(&dconf).await.unwrap();
        let expected = Keybindings {
            list: [wechat(), vec![own("<Super>z")]]
                .concat()
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
            entries: [wechat(), vec![own("<Super>z")]].concat(),
        };
        assert_eq!(dconf.state().await, expected);

        set_up(&dconf).await.unwrap();
        assert_eq!(dconf.state().await, expected);
    }

    #[tokio::test]
    async fn a_list_changed_before_the_first_write_writes_nothing() {
        let dconf = FakeDconf {
            // The first read plans; the second is the check before writing.
            changes: BTreeMap::from([(2, "@as []".to_string())]),
            ..FakeDconf::with(&wechat())
        };
        assert!(set_up(&dconf).await.is_err());
        assert_eq!(dconf.writes(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_list_changed_after_the_fields_keeps_the_list() {
        let dconf = FakeDconf {
            changes: BTreeMap::from([(3, "@as []".to_string())]),
            ..FakeDconf::with(&wechat())
        };
        assert!(set_up(&dconf).await.is_err());
        assert_eq!(
            dconf.writes(),
            [
                "set name 'Limen dictation'",
                "set command '/bin/limen toggle'",
                "set binding '<Super>z'",
                "set binding ''",
            ]
        );
    }

    #[tokio::test]
    async fn clearing_removes_our_entry_or_disables_an_adopted_one() {
        let dconf = FakeDconf::with(&[wechat(), vec![own("<Super>z")]].concat());
        let before = dconf.state().await;
        apply_clear(&dconf, &before, &clear(&before.list, &before.entries))
            .await
            .unwrap();
        assert_eq!(
            dconf.state().await,
            Keybindings {
                list: wechat().iter().map(|entry| entry.path.clone()).collect(),
                entries: wechat(),
            }
        );

        let adopted = CustomKeybinding {
            path: format!("{PREFIX}custom0/"),
            ..own("<Super>d")
        };
        let dconf = FakeDconf::with(std::slice::from_ref(&adopted));
        let before = dconf.state().await;
        let plan = clear(&before.list, &before.entries);
        assert_eq!(
            plan,
            Clear::Disable {
                path: adopted.path.clone()
            }
        );
        apply_clear(&dconf, &before, &plan).await.unwrap();
        assert_eq!(
            dconf.state().await,
            Keybindings {
                list: vec![adopted.path.clone()],
                entries: vec![CustomKeybinding {
                    binding: String::new(),
                    ..adopted
                }],
            }
        );
    }

    #[tokio::test]
    async fn conflicts_report_the_schemas_left_unchecked() {
        let dconf = FakeDconf::with(&wechat());
        assert_eq!(
            find_conflicts(&dconf, "Super+V".parse().unwrap(), &wechat())
                .await
                .unwrap(),
            (
                vec![Conflict::Setting {
                    schema: "org.gnome.shell.keybindings".to_string(),
                    key: "toggle-message-tray".to_string(),
                }],
                vec!["org.gnome.mutter.wayland.keybindings"],
            )
        );
    }

    #[tokio::test]
    async fn updating_an_entry_removed_meanwhile_fails() {
        let dconf = FakeDconf {
            // Our entry disappears from the list while its fields are written.
            changes: BTreeMap::from([(3, gvariant::strings(&paths(&wechat())))]),
            ..FakeDconf::with(&[wechat(), vec![own("<Super>x")]].concat())
        };
        assert!(set_up(&dconf).await.is_err());
    }

    #[tokio::test]
    async fn rollback_restores_fields_without_undoing_other_list_changes() {
        let ours = own("<Super>x");
        let initial = [wechat(), vec![ours.clone()]].concat();
        let changed_list: Vec<String> = initial
            .iter()
            .rev()
            .map(|entry| entry.path.clone())
            .collect();
        let dconf = FakeDconf {
            changes: BTreeMap::from([(3, gvariant::strings(&changed_list))]),
            ..FakeDconf::with(&initial)
        };
        let before = dconf.state().await;
        let plan = setup(
            &before.list,
            &before.entries,
            "Super+Z".parse().unwrap(),
            "/new/limen toggle".to_string(),
        );
        assert!(apply_setup(&dconf, &before, &plan).await.is_err());

        assert_eq!(
            dconf.state().await,
            Keybindings {
                list: changed_list,
                entries: vec![ours, wechat()[1].clone(), wechat()[0].clone()],
            }
        );
    }
}
