//! The plugin library: the plugins the user can put in the routing graph,
//! kept in a file of their own so that the routing page lists them without
//! looking at the disk. Reading plugins opens each one in a worker, which
//! takes seconds, so it happens only when the user asks for a scan or adds a
//! bundle, never when a page opens.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, TryLockError};

use plugin_graph::vst3::{PluginInfo, Sandbox, Scan, find_bundles};

use super::plugins::{NO_WORKER, PluginChains};

use crate::config::{Paths, load_plugin_library, save_plugin_library};
use crate::ipc::audio::{PluginEntry, PluginLibrary, RemovedPlugin, ScanFailure};

/// Reads the plugins in some bundles. The real one opens them in sandboxed
/// workers; tests give a fake.
pub(crate) type Scanner<'a> = &'a dyn Fn(&[PathBuf]) -> Scan;

/// Owns the saved library. Changes are made one at a time, so that a scan
/// that is still running does not write over a plugin removed meanwhile.
pub(crate) struct Library {
    paths: Paths,
    changing: Mutex<()>,
}

fn entry(info: PluginInfo) -> PluginEntry {
    PluginEntry {
        effect: info.is_effect(),
        path: info.path.display().to_string(),
        class_id: info.class_id,
        name: info.name,
        vendor: info.vendor,
        category: info.category,
    }
}

/// A path as typed: with `~` for the home folder, and made absolute, since
/// the daemon's own working folder means nothing to the user.
fn normalized(text: &str) -> Result<String, String> {
    let text = text.trim();
    let expanded = match text.strip_prefix("~/").or_else(|| text.strip_prefix('~')) {
        Some(rest) if text == "~" || text.starts_with("~/") => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .ok_or_else(|| "the home folder is not known".to_string())?,
        Some(_) | None => PathBuf::from(text),
    };
    match (text.is_empty(), expanded.is_absolute()) {
        (true, _) => Err("the path is empty".to_string()),
        (false, false) => Err(format!("{text} is not an absolute path")),
        // Collecting the components drops a trailing slash.
        (false, true) => Ok(expanded
            .components()
            .collect::<PathBuf>()
            .display()
            .to_string()),
    }
}

/// Whether `path` is one of `bundles` or inside one of `folders`.
fn reached(path: &str, folders: &[String], bundles: &[String]) -> bool {
    bundles.iter().any(|bundle| bundle == path)
        || folders
            .iter()
            .any(|folder| Path::new(path).starts_with(folder))
}

fn sorted(plugins: &mut [PluginEntry]) {
    plugins.sort_by_cached_key(|plugin| {
        (
            plugin.name.to_lowercase(),
            plugin.vendor.to_lowercase(),
            plugin.path.clone(),
            plugin.class_id.clone(),
        )
    });
}

impl Library {
    pub(crate) fn new(paths: Paths) -> Self {
        Self {
            paths,
            changing: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.changing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load(&self) -> Result<PluginLibrary, String> {
        load_plugin_library(&self.paths).map_err(|error| format!("{error:#}"))
    }

    fn save(&self, library: &PluginLibrary) -> Result<PluginLibrary, String> {
        save_plugin_library(&self.paths, library).map_err(|error| format!("{error:#}"))?;
        Ok(library.clone())
    }

    /// The saved library.
    pub(crate) fn get(&self) -> Result<PluginLibrary, String> {
        self.load()
    }

    /// Look in `folders` from now on. Plugins that were only reachable
    /// through a folder that is gone leave the list; a folder that is new
    /// must exist, which catches a mistyped path.
    pub(crate) fn set_folders(&self, folders: Vec<String>) -> Result<PluginLibrary, String> {
        let _changing = self.lock();
        let mut library = self.load()?;
        let mut kept = Vec::new();
        for folder in folders {
            let folder = normalized(&folder)?;
            match (
                kept.contains(&folder),
                library.folders.contains(&folder) || Path::new(&folder).is_dir(),
            ) {
                (true, _) => {}
                (false, true) => kept.push(folder),
                (false, false) => return Err(format!("{folder} is not a folder")),
            }
        }
        library.folders = kept;
        let (folders, added) = (library.folders.clone(), library.added.clone());
        library
            .plugins
            .retain(|plugin| reached(&plugin.path, &folders, &added));
        library
            .failures
            .retain(|failure| reached(&failure.path, &folders, &added));
        self.save(&library)
    }

    /// Read the plugins in the folders and the added bundles again. Refused
    /// while another scan runs.
    pub(crate) fn scan(&self, scanner: Scanner) -> Result<PluginLibrary, String> {
        let _changing = match self.changing.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err("the library is busy: a scan is already running".to_string());
            }
        };
        self.scan_locked(self.load()?, scanner)
    }

    fn scan_locked(
        &self,
        mut library: PluginLibrary,
        scanner: Scanner,
    ) -> Result<PluginLibrary, String> {
        let folders = library
            .folders
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let mut bundles = find_bundles(&folders);
        let known = bundles.iter().cloned().collect::<HashSet<_>>();
        bundles.extend(
            library
                .added
                .iter()
                .map(PathBuf::from)
                .filter(|bundle| !known.contains(bundle)),
        );
        let scan = scanner(&bundles);
        library.plugins = scan
            .plugins
            .into_iter()
            .map(entry)
            .filter(|plugin| {
                !library
                    .removed
                    .iter()
                    .any(|gone| gone.path == plugin.path && gone.class_id == plugin.class_id)
            })
            .collect();
        sorted(&mut library.plugins);
        library.failures = scan
            .failures
            .into_iter()
            .map(|(path, error)| ScanFailure {
                path: path.display().to_string(),
                error,
            })
            .collect();
        self.save(&library)
    }

    /// Put the bundle at `path`, wherever it is, in the library. Refused
    /// with the reason when it cannot be read.
    pub(crate) fn add(&self, path: &str, scanner: Scanner) -> Result<PluginLibrary, String> {
        let path = normalized(path)?;
        let bundle = PathBuf::from(&path);
        match (
            bundle
                .extension()
                .is_some_and(|extension| extension == "vst3"),
            bundle.exists(),
        ) {
            (false, _) => return Err(format!("{path} is not a .vst3 bundle")),
            (true, false) => return Err(format!("{path} does not exist")),
            (true, true) => {}
        }
        let _changing = self.lock();
        let mut library = self.load()?;
        let scan = scanner(std::slice::from_ref(&bundle));
        if let Some((_, error)) = scan.failures.into_iter().next() {
            return Err(format!("{path} could not be read: {error}"));
        }
        if scan.plugins.is_empty() {
            return Err(format!("{path} holds no plugins"));
        }
        if !library.added.contains(&path) {
            library.added.push(path.clone());
        }
        library.removed.retain(|gone| gone.path != path);
        library.failures.retain(|failure| failure.path != path);
        library.plugins.retain(|plugin| plugin.path != path);
        library.plugins.extend(scan.plugins.into_iter().map(entry));
        sorted(&mut library.plugins);
        self.save(&library)
    }

    /// Take a plugin out of the library, and keep scans from bringing it
    /// back. Its bundle is not touched.
    pub(crate) fn remove(&self, path: &str, class_id: &str) -> Result<PluginLibrary, String> {
        let _changing = self.lock();
        let mut library = self.load()?;
        library
            .plugins
            .retain(|plugin| !(plugin.path == path && plugin.class_id == class_id));
        let gone = RemovedPlugin {
            path: path.to_string(),
            class_id: class_id.to_string(),
        };
        if !library.removed.contains(&gone) {
            library.removed.push(gone);
        }
        self.save(&library)
    }

    /// Forget what was taken out, and scan, which brings it back.
    pub(crate) fn restore(&self, scanner: Scanner) -> Result<PluginLibrary, String> {
        let _changing = match self.changing.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err("the library is busy: a scan is already running".to_string());
            }
        };
        let mut library = self.load()?;
        library.removed.clear();
        self.scan_locked(library, scanner)
    }
}

impl PluginChains {
    /// Reads bundles in workers of their own, sandboxed because a plugin
    /// runs code when it is read.
    fn scanner(&self) -> Result<impl Fn(&[PathBuf]) -> Scan + '_, String> {
        let host = self.host.as_ref().ok_or_else(|| NO_WORKER.to_string())?;
        Ok(move |bundles: &[PathBuf]| {
            let scan = host.scan_bundles(bundles, Sandbox::On);
            for (path, error) in &scan.failures {
                tracing::info!(path = %path.display(), %error, "plugin skipped");
            }
            scan
        })
    }

    /// The saved plugin library.
    pub(crate) fn plugin_library(&self) -> Result<PluginLibrary, String> {
        self.library.get()
    }

    /// Look for plugins in these folders from now on.
    pub(crate) fn set_library_folders(
        &self,
        folders: Vec<String>,
    ) -> Result<PluginLibrary, String> {
        self.library.set_folders(folders)
    }

    /// Read the library's folders and bundles again.
    pub(crate) fn scan_library(&self) -> Result<PluginLibrary, String> {
        self.library.scan(&self.scanner()?)
    }

    /// Put the bundle at `path` in the library.
    pub(crate) fn add_library_bundle(&self, path: &str) -> Result<PluginLibrary, String> {
        self.library.add(path, &self.scanner()?)
    }

    /// Take a plugin out of the library.
    pub(crate) fn remove_library_plugin(
        &self,
        path: &str,
        class_id: &str,
    ) -> Result<PluginLibrary, String> {
        self.library.remove(path, class_id)
    }

    /// Bring back what was taken out of the library.
    pub(crate) fn restore_library(&self) -> Result<PluginLibrary, String> {
        self.library.restore(&self.scanner()?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use plugin_graph::vst3::{PluginInfo, Scan};

    use super::Library;
    use crate::config::Paths;
    use crate::ipc::audio::{PluginLibrary, RemovedPlugin, ScanFailure};

    /// A folder of the user's with these (empty) bundles in it.
    fn folder(name: &str, bundles: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for bundle in bundles {
            fs::create_dir_all(dir.join(bundle)).unwrap();
        }
        dir
    }

    fn library(name: &str) -> Library {
        let dir = folder(name, &[]);
        Library::new(Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        })
    }

    fn info(path: &Path, name: &str, category: &str) -> PluginInfo {
        PluginInfo {
            path: path.to_path_buf(),
            class_id: format!("id-{name}"),
            name: name.to_string(),
            vendor: "Acme".to_string(),
            category: category.to_string(),
        }
    }

    /// Finds a plugin named after each bundle, and fails at any named
    /// `Broken`.
    fn fake(bundles: &[PathBuf]) -> Scan {
        let mut scan = Scan::default();
        for bundle in bundles {
            let name = bundle.file_stem().unwrap().to_str().unwrap();
            match name {
                "Broken" => scan
                    .failures
                    .push((bundle.clone(), "did not load".to_string())),
                _ => scan.plugins.push(info(bundle, name, "Fx|Dynamics")),
            }
        }
        scan
    }

    fn names(library: &PluginLibrary) -> Vec<String> {
        library
            .plugins
            .iter()
            .map(|plugin| plugin.name.clone())
            .collect()
    }

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_scan_lists_the_folders_and_added_bundles_and_says_which_it_could_not_read() {
        let library = library("library-scan");
        let vst = folder(
            "library-scan-folder",
            &["Gate.vst3", "vendor/Eq.vst3", "Broken.vst3"],
        );
        let mine = folder("library-scan-mine", &["Comp.vst3"]);
        library
            .set_folders(vec![vst.display().to_string()])
            .unwrap();
        library
            .add(&mine.join("Comp.vst3").display().to_string(), &fake)
            .unwrap();
        let scanned = library.scan(&fake).unwrap();
        assert_eq!(
            (names(&scanned), scanned.failures),
            (
                list(&["Comp", "Eq", "Gate"]),
                vec![ScanFailure {
                    path: vst.join("Broken.vst3").display().to_string(),
                    error: "did not load".to_string(),
                }]
            )
        );
    }

    #[test]
    fn a_removed_plugin_stays_out_of_scans_until_it_is_added_again_or_restored() {
        let library = library("library-remove");
        let vst = folder("library-remove-folder", &["Gate.vst3", "Eq.vst3"]);
        let gate = vst.join("Gate.vst3").display().to_string();
        library
            .set_folders(vec![vst.display().to_string()])
            .unwrap();
        library.scan(&fake).unwrap();
        let removed = library.remove(&gate, "id-Gate").unwrap();
        let rescanned = library.scan(&fake).unwrap();
        let added = library.add(&gate, &fake).unwrap();
        library.remove(&gate, "id-Gate").unwrap();
        let restored = library.restore(&fake).unwrap();
        assert_eq!(
            (
                names(&removed),
                removed.removed,
                names(&rescanned),
                names(&added),
                added.removed,
                names(&restored),
                restored.removed,
            ),
            (
                list(&["Eq"]),
                vec![RemovedPlugin {
                    path: gate,
                    class_id: "id-Gate".to_string()
                }],
                list(&["Eq"]),
                list(&["Eq", "Gate"]),
                vec![],
                list(&["Eq", "Gate"]),
                vec![],
            )
        );
    }

    #[test]
    fn dropping_a_folder_drops_the_plugins_found_only_there() {
        let library = library("library-folders");
        let (one, two) = (
            folder("library-folders-one", &["Gate.vst3"]),
            folder("library-folders-two", &["Eq.vst3"]),
        );
        let mine = folder("library-folders-mine", &["Comp.vst3"]);
        library
            .set_folders(vec![one.display().to_string(), two.display().to_string()])
            .unwrap();
        library
            .add(&mine.join("Comp.vst3").display().to_string(), &fake)
            .unwrap();
        library.scan(&fake).unwrap();
        let after = library
            .set_folders(vec![two.display().to_string()])
            .unwrap();
        assert_eq!(names(&after), list(&["Comp", "Eq"]));
    }

    #[test]
    fn a_mistyped_folder_or_bundle_is_refused_with_the_reason() {
        let library = library("library-refuse");
        let vst = folder("library-refuse-folder", &["Broken.vst3", "Plain"]);
        let missing = vst.join("nothing");
        let refused = (
            library.set_folders(vec![missing.display().to_string()]),
            library.set_folders(vec!["relative/dir".to_string()]),
            library.add(&vst.join("Plain").display().to_string(), &fake),
            library.add(&vst.join("Missing.vst3").display().to_string(), &fake),
            library.add(&vst.join("Broken.vst3").display().to_string(), &fake),
            library.get().unwrap().added,
        );
        assert_eq!(
            refused,
            (
                Err(format!("{} is not a folder", missing.display())),
                Err("relative/dir is not an absolute path".to_string()),
                Err(format!(
                    "{} is not a .vst3 bundle",
                    vst.join("Plain").display()
                )),
                Err(format!(
                    "{} does not exist",
                    vst.join("Missing.vst3").display()
                )),
                Err(format!(
                    "{} could not be read: did not load",
                    vst.join("Broken.vst3").display()
                )),
                Vec::<String>::new(),
            )
        );
    }

    #[test]
    fn a_second_scan_is_refused_while_one_runs() {
        let library = library("library-busy");
        let inner = std::cell::RefCell::new(None);
        library
            .scan(&|_: &[PathBuf]| {
                *inner.borrow_mut() = Some(library.scan(&|_: &[PathBuf]| Scan::default()));
                Scan::default()
            })
            .unwrap();
        assert_eq!(
            inner.into_inner().map(|result| result.map(|_| ())),
            Some(Err(
                "the library is busy: a scan is already running".to_string()
            ))
        );
    }
}
