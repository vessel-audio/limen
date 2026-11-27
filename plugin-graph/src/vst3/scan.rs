//! Finding the plugins a machine has.

use std::path::{Path, PathBuf};

use vst_audio::{PluginDescriptor, PluginFormat, SandboxPolicy};

use super::host::Vst3Host;

/// How deep below a plugin folder bundles are looked for: vendors keep
/// theirs in folders of their own.
const DEPTH: usize = 3;

/// One plugin class found in a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    pub path: PathBuf,
    /// The class inside the bundle, as lowercase hexadecimal.
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    /// The plugin's own subcategories, such as `Fx|Dynamics` or
    /// `Instrument|Synth`.
    pub category: String,
}

impl PluginInfo {
    /// Whether it processes audio rather than making it.
    pub fn is_effect(&self) -> bool {
        self.category.starts_with("Fx")
    }
}

/// Whether the scan's workers may reach the network, run programs, or write
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    Off,
    /// Some plugins refuse to load under it.
    On,
}

/// What a scan found.
#[derive(Debug, Default)]
pub struct Scan {
    pub plugins: Vec<PluginInfo>,
    /// Bundles that could not be read, and why.
    pub failures: Vec<(PathBuf, String)>,
}

/// Where VST3 plugins are installed by convention.
pub fn default_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let under_home = |relative: &str| home.as_ref().map(|home| home.join(relative));
    match cfg!(target_os = "macos") {
        true => ["/Library/Audio/Plug-Ins/VST3".into()]
            .into_iter()
            .chain(under_home("Library/Audio/Plug-Ins/VST3"))
            .collect(),
        false => ["/usr/lib/vst3", "/usr/local/lib/vst3"]
            .map(PathBuf::from)
            .into_iter()
            .chain(under_home(".vst3"))
            .chain(under_home(".nix-profile/lib/vst3"))
            .collect(),
    }
}

fn bundles(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        match (
            path.extension()
                .is_some_and(|extension| extension == "vst3"),
            path.is_dir(),
        ) {
            (true, _) => found.push(path),
            (false, true) if depth > 0 => bundles(&path, depth - 1, found),
            (false, true) | (false, false) => {}
        }
    }
}

impl Vst3Host {
    /// List the plugin classes in the bundles under `folders`. Each bundle is
    /// opened in a worker of its own, so a plugin that crashes while it is
    /// read costs only its own entry.
    pub fn scan(&self, folders: &[PathBuf], sandbox: Sandbox) -> Scan {
        let mut paths = Vec::new();
        folders
            .iter()
            .for_each(|folder| bundles(folder, DEPTH, &mut paths));
        let policy = SandboxPolicy::default();
        let mut scan = Scan::default();
        for path in paths {
            let descriptor = PluginDescriptor::new(&path, PluginFormat::Vst3);
            let classes = match sandbox {
                Sandbox::Off => self.inner().scan(&descriptor),
                Sandbox::On => self.inner().scan_sandboxed(&descriptor, &policy),
            };
            match classes {
                Ok(classes) => scan
                    .plugins
                    .extend(classes.into_iter().map(|class| PluginInfo {
                        path: path.clone(),
                        class_id: class.id.iter().map(|byte| format!("{byte:02x}")).collect(),
                        name: class.name,
                        vendor: class.vendor,
                        category: class.category,
                    })),
                Err(error) => scan.failures.push((path, error.to_string())),
            }
        }
        scan
    }
}
