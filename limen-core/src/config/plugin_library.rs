//! Where the plugin library is kept: beside the config file, as the routing
//! graph is, so the GUI can edit it when a tool owns `config.toml`.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::Paths;
use super::audio_graph::write_atomically;
use crate::ipc::audio::PluginLibrary;

impl Paths {
    /// The plugin library, as TOML.
    pub(crate) fn plugin_library_file(&self) -> PathBuf {
        self.config_file.with_file_name("plugin-library.toml")
    }
}

/// The saved plugin library; the default one, with the usual folders and
/// no plugins, when none is saved.
pub(crate) fn load_plugin_library(paths: &Paths) -> Result<PluginLibrary> {
    let file = paths.plugin_library_file();
    match fs::read_to_string(&file) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("failed to parse {}", file.display()))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(PluginLibrary::default()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", file.display())),
    }
}

/// Save the plugin library.
pub(crate) fn save_plugin_library(paths: &Paths, library: &PluginLibrary) -> Result<()> {
    let text = toml::to_string_pretty(library).context("failed to write the plugin library")?;
    let _lock = super::lock_dir(&paths.config_file)?;
    write_atomically(&paths.plugin_library_file(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{load_plugin_library, save_plugin_library};
    use crate::config::Paths;
    use crate::ipc::audio::{PluginEntry, PluginLibrary, RemovedPlugin, ScanFailure};

    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("limen-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Paths {
            socket: dir.join("limen.sock"),
            log_file: dir.join("daemon.log"),
            cache_dir: dir.join("cache"),
            config_file: dir.join("config.toml"),
        }
    }

    #[test]
    fn a_library_is_saved_beside_the_config_and_comes_back_whole() {
        let paths = scratch("plugin-library");
        let before = load_plugin_library(&paths).unwrap();
        let library = PluginLibrary {
            folders: vec!["/opt/vst3".to_string()],
            added: vec!["/home/me/Mine.vst3".to_string()],
            removed: vec![RemovedPlugin {
                path: "/opt/vst3/Old.vst3".to_string(),
                class_id: "ab".to_string(),
            }],
            plugins: vec![PluginEntry {
                path: "/opt/vst3/Gate.vst3".to_string(),
                class_id: "00ff".to_string(),
                name: "Gate".to_string(),
                vendor: "Acme".to_string(),
                category: "Fx|Dynamics".to_string(),
                effect: true,
            }],
            failures: vec![ScanFailure {
                path: "/opt/vst3/Broken.vst3".to_string(),
                error: "did not load".to_string(),
            }],
        };
        save_plugin_library(&paths, &library).unwrap();
        assert_eq!(
            (before, load_plugin_library(&paths).unwrap()),
            (PluginLibrary::default(), library)
        );
    }

    #[test]
    fn a_hand_written_library_may_leave_out_what_it_does_not_change() {
        let paths = scratch("plugin-library-partial");
        fs::write(paths.plugin_library_file(), "added = [\"/x/Y.vst3\"]\n").unwrap();
        assert_eq!(
            load_plugin_library(&paths).unwrap(),
            PluginLibrary {
                added: vec!["/x/Y.vst3".to_string()],
                ..PluginLibrary::default()
            }
        );
        fs::write(paths.plugin_library_file(), "folders = []\n").unwrap();
        assert_eq!(
            load_plugin_library(&paths).unwrap(),
            PluginLibrary {
                folders: Vec::new(),
                ..PluginLibrary::default()
            }
        );
    }
}
