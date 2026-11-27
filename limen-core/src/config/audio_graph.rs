//! Where the audio routing graph and its plugins' state are kept: beside the
//! config file rather than in it, so the GUI can edit them even when a tool
//! such as home-manager owns `config.toml`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use plugin_graph::{GraphSpec, NodeId};

use super::Paths;

impl Paths {
    /// The routing graph, as TOML.
    pub(crate) fn audio_graph_file(&self) -> PathBuf {
        self.config_file.with_file_name("audio-graph.toml")
    }

    /// The saved state of the plugin at node `node` of the graph.
    pub(crate) fn plugin_state_file(&self, node: NodeId) -> PathBuf {
        self.config_file
            .with_file_name("audio-plugins")
            .join(format!("{node}.state"))
    }
}

/// The saved routing graph; the default one, which changes nothing, when
/// none is saved.
pub(crate) fn load_audio_graph(paths: &Paths) -> Result<GraphSpec> {
    let file = paths.audio_graph_file();
    match fs::read_to_string(&file) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("failed to parse {}", file.display()))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(GraphSpec::passthrough()),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", file.display())),
    }
}

/// Save the routing graph.
pub(crate) fn save_audio_graph(paths: &Paths, graph: &GraphSpec) -> Result<()> {
    let text = toml::to_string_pretty(graph).context("failed to write the audio graph")?;
    let _lock = super::lock_dir(&paths.config_file)?;
    write_atomically(&paths.audio_graph_file(), text.as_bytes())
}

/// The saved state of a plugin, if it has one.
pub(crate) fn load_plugin_state(paths: &Paths, node: NodeId) -> Result<Option<Vec<u8>>> {
    let file = paths.plugin_state_file(node);
    match fs::read(&file) {
        Ok(state) => Ok(Some(state)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", file.display())),
    }
}

/// Save a plugin's state.
pub(crate) fn save_plugin_state(paths: &Paths, node: NodeId, state: &[u8]) -> Result<()> {
    let _lock = super::lock_dir(&paths.config_file)?;
    write_atomically(&paths.plugin_state_file(node), state)
}

/// Forget a plugin's saved state, as when its node is gone or holds another
/// plugin now.
pub(crate) fn delete_plugin_state(paths: &Paths, node: NodeId) -> Result<()> {
    let _lock = super::lock_dir(&paths.config_file)?;
    match fs::remove_file(paths.plugin_state_file(node)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to remove a plugin's saved state"),
    }
}

/// Write `contents` to a temp file beside `file` and rename it into place, so
/// a reader sees the old contents or the new, never a part.
fn write_atomically(file: &Path, contents: &[u8]) -> Result<()> {
    let dir = file.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let temp = file.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&temp, contents).with_context(|| format!("failed to write {}", temp.display()))?;
    fs::rename(&temp, file).with_context(|| format!("failed to replace {}", file.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use plugin_graph::{GraphSpec, NodeKind};

    use super::{load_audio_graph, load_plugin_state, save_audio_graph, save_plugin_state};
    use crate::config::Paths;

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
    fn a_graph_and_a_plugins_state_are_saved_beside_the_config() {
        let paths = scratch("audio-graph");
        let before = (
            load_audio_graph(&paths).unwrap(),
            load_plugin_state(&paths, 2).unwrap(),
        );
        let mut graph = GraphSpec::passthrough();
        graph.nodes[0].x = 12.;
        graph.nodes.push(plugin_graph::NodeSpec {
            id: 2,
            kind: NodeKind::Gain { db: -6. },
            x: 3.,
            y: 4.,
        });
        save_audio_graph(&paths, &graph).unwrap();
        save_plugin_state(&paths, 2, &[0, 255, 7]).unwrap();
        assert_eq!(
            (
                before,
                load_audio_graph(&paths).unwrap(),
                load_plugin_state(&paths, 2).unwrap(),
            ),
            (
                (GraphSpec::passthrough(), None),
                graph,
                Some(vec![0, 255, 7])
            )
        );
    }

    #[test]
    fn a_graph_that_does_not_parse_is_an_error_not_the_default() {
        let paths = scratch("audio-graph-bad");
        fs::write(paths.audio_graph_file(), "nodes = 3").unwrap();
        assert!(load_audio_graph(&paths).is_err());
    }
}
