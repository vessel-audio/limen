//! The plugins a take runs through. Their workers are loaded when a take
//! first needs them (or the graph is saved) and kept between takes, since
//! starting one takes a couple of hundred milliseconds, which dictation
//! should not wait for each time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use plugin_graph::vst3::{
    Channels, LoadOptions, PluginEvent, Sandbox, Vst3Control, Vst3Host, Vst3Processor,
    default_folders,
};
use plugin_graph::{
    CompiledGraph, GraphError, GraphSpec, NodeId, NodeKind, PluginRef, PortLayout, ProcessError,
    Processor, Sidechain,
};

use super::chain::{Chain, GRAPH_BLOCK, GRAPH_CONFIG, GRAPH_RATE};
use crate::config::{
    Paths, load_audio_graph, load_plugin_state, save_audio_graph, save_plugin_state,
};
use crate::ipc::audio::{AudioGraphView, NodeStatus, PluginEntry};

/// How long a plugin may take to process a block before it counts as failed.
const DEADLINE: Duration = Duration::from_millis(50);

/// How often loaded plugins are asked what happened to them.
const POLL: Duration = Duration::from_millis(250);

/// What identifies a loaded plugin: a different one needs a new worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadKey {
    path: String,
    sidechain: Sidechain,
}

struct Loaded {
    key: LoadKey,
    /// Owns the worker, which stops when this is dropped.
    control: Vst3Control,
    processor: Arc<Mutex<Vst3Processor>>,
    layout: PortLayout,
}

impl Loaded {
    /// Whether a running take holds the plugin.
    fn in_use(&self) -> bool {
        Arc::strong_count(&self.processor) > 1
    }
}

/// A loaded plugin as a graph node, shared with the registry so that it
/// survives the graph.
struct Shared {
    processor: Arc<Mutex<Vst3Processor>>,
    layout: PortLayout,
    latency: usize,
}

impl Processor for Shared {
    fn layout(&self) -> PortLayout {
        self.layout
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn process(
        &mut self,
        frames: usize,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<(), ProcessError> {
        self.processor
            .lock()
            .map_err(|_| ProcessError("the plugin's processor panicked".to_string()))?
            .process(frames, input, output)
    }
}

/// Stands in for a plugin that could not be loaded: a node that is passed
/// through from the first block, with the reason as its failure.
struct Unavailable(String);

impl Processor for Unavailable {
    fn layout(&self) -> PortLayout {
        UNAVAILABLE_LAYOUT
    }

    fn latency(&self) -> usize {
        0
    }

    fn process(&mut self, _: usize, _: &[f32], _: &mut [f32]) -> Result<(), ProcessError> {
        Err(ProcessError(self.0.clone()))
    }
}

/// What a plugin that is not loaded is drawn with: one channel in and out.
const UNAVAILABLE_LAYOUT: PortLayout = PortLayout {
    main_inputs: 1,
    sidechain_inputs: 0,
    outputs: 1,
};

#[derive(Default)]
pub(super) struct State {
    loaded: HashMap<NodeId, Loaded>,
    /// Why a node's plugin does nothing.
    problems: HashMap<NodeId, String>,
}

/// The routing graph's plugins, and the graphs built from them.
pub(crate) struct PluginChains {
    pub(super) paths: Paths,
    host: Option<Vst3Host>,
    state: Mutex<State>,
    /// Stops the test recording in progress, if any.
    pub(super) test: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

/// The worker executable: named by `LIMEN_VST_WORKER`, else `limen-vst-worker`
/// next to this executable, whether the executable is reached through a
/// symlink (as `bin/limen` is in the macOS app bundle) or not.
fn worker() -> Option<PathBuf> {
    const NAME: &str = "limen-vst-worker";
    std::env::var_os("LIMEN_VST_WORKER")
        .map(PathBuf::from)
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            [Some(exe.clone()), exe.canonicalize().ok()]
                .into_iter()
                .flatten()
                .filter_map(|exe| Some(exe.parent()?.join(NAME)))
                .find(|path| path.is_file())
        })
        .filter(|path| path.is_file())
}

const NO_WORKER: &str = "the plugin worker was not found (set LIMEN_VST_WORKER)";

fn plugins_in(spec: &GraphSpec) -> impl Iterator<Item = (NodeId, &PluginRef)> {
    spec.nodes.iter().filter_map(|node| match &node.kind {
        NodeKind::Plugin(plugin) => Some((node.id, plugin)),
        NodeKind::Input | NodeKind::Output | NodeKind::Gain { .. } => None,
    })
}

impl PluginChains {
    /// The registry for the graph saved under `paths`. A thread of its own
    /// watches the loaded plugins, saving a plugin's state when its editor
    /// closes; it ends with the registry.
    pub(crate) fn new(paths: Paths) -> Arc<Self> {
        let chains = Arc::new(Self {
            paths,
            host: worker().map(Vst3Host::new),
            state: Mutex::new(State::default()),
            test: Mutex::new(None),
        });
        let weak = Arc::downgrade(&chains);
        let spawned = std::thread::Builder::new()
            .name("limen-plugins".into())
            .spawn(move || Self::watch(weak));
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot watch the plugins' editors");
        }
        chains
    }

    fn watch(chains: Weak<Self>) {
        while let Some(chains) = chains.upgrade() {
            chains.poll();
            drop(chains);
            std::thread::sleep(POLL);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Handle what happened to the plugins since the last poll: save the
    /// state of one whose editor closed, and note one that failed.
    fn poll(&self) {
        let mut state = self.lock();
        let State { loaded, problems } = &mut *state;
        for (node, plugin) in loaded.iter_mut() {
            for event in plugin.control.events() {
                match event {
                    PluginEvent::EditorClosed => self.save_state(*node, &mut plugin.control),
                    PluginEvent::Failed(reason) => {
                        problems.insert(*node, reason);
                    }
                }
            }
        }
    }

    fn save_state(&self, node: NodeId, control: &mut Vst3Control) {
        let saved = control
            .save_state()
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                save_plugin_state(&self.paths, node, &bytes).map_err(|error| format!("{error:#}"))
            });
        if let Err(error) = saved {
            tracing::warn!(node, %error, "could not save a plugin's state");
        }
    }

    /// The signal path for a take, or `None` when the saved graph changes
    /// nothing or cannot be used, in which case the microphone goes straight
    /// to the recognizer as ever. Loads plugins that are not loaded yet, so
    /// it may take a while: call it off the async runtime.
    pub(crate) fn build(&self) -> Option<Chain> {
        let spec = match load_audio_graph(&self.paths) {
            Ok(spec) => spec,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "audio graph unreadable; recording without it");
                return None;
            }
        };
        let mut state = self.lock();
        state
            .loaded
            .retain(|node, _| plugins_in(&spec).any(|(id, _)| id == *node));
        if spec.is_passthrough() {
            return None;
        }
        match self.compile(&spec, &mut state) {
            Ok(graph) => Some(Chain::new(graph)),
            Err(error) => {
                tracing::warn!(error = %error, "audio graph unusable; recording without it");
                None
            }
        }
    }

    fn compile(&self, spec: &GraphSpec, state: &mut State) -> Result<CompiledGraph, GraphError> {
        let processors = plugins_in(spec)
            .map(|(node, plugin)| (node, self.processor(state, node, plugin)))
            .collect::<HashMap<_, _>>();
        CompiledGraph::compile(spec, GRAPH_CONFIG, processors)
    }

    /// The processor for the plugin at `node`: the loaded one, started over,
    /// or a new one.
    fn processor(&self, state: &mut State, node: NodeId, plugin: &PluginRef) -> Box<dyn Processor> {
        let key = LoadKey {
            path: plugin.path.clone(),
            sidechain: plugin.sidechain,
        };
        let loaded = state.loaded.get(&node).filter(|current| current.key == key);
        if let Some(current) = loaded {
            if current.in_use() {
                return Box::new(Unavailable("an earlier take is still using it".to_string()));
            }
            let restarted = current.processor.lock().ok().and_then(|mut processor| {
                processor.restart().ok()?;
                Some(Shared {
                    layout: processor.layout(),
                    latency: processor.latency(),
                    processor: current.processor.clone(),
                })
            });
            if let Some(shared) = restarted {
                state.problems.remove(&node);
                return Box::new(shared);
            }
        }
        state.loaded.remove(&node);
        let loaded = match &self.host {
            None => Err(NO_WORKER.to_string()),
            Some(host) => {
                let options = LoadOptions {
                    sample_rate: GRAPH_RATE,
                    block: GRAPH_BLOCK,
                    channels: Channels::PreferMono,
                    sidechain: plugin.sidechain,
                    deadline: DEADLINE,
                };
                let saved = load_plugin_state(&self.paths, node).ok().flatten();
                host.load(
                    std::path::Path::new(&plugin.path),
                    options,
                    saved.as_deref(),
                )
                .map_err(|error| error.to_string())
            }
        };
        match loaded {
            Ok((control, processor)) => {
                let shared = Shared {
                    layout: processor.layout(),
                    latency: processor.latency(),
                    processor: Arc::new(Mutex::new(processor)),
                };
                state.problems.remove(&node);
                state.loaded.insert(
                    node,
                    Loaded {
                        key,
                        control,
                        processor: shared.processor.clone(),
                        layout: shared.layout,
                    },
                );
                Box::new(shared)
            }
            Err(error) => {
                tracing::warn!(plugin = %plugin.name, %error, "plugin did not load");
                let reason = format!("{}: {error}", plugin.name);
                state.problems.insert(node, reason.clone());
                Box::new(Unavailable(reason))
            }
        }
    }

    /// The saved routing graph and how each node of it is doing.
    pub(crate) fn view(&self) -> Result<AudioGraphView, String> {
        let graph = load_audio_graph(&self.paths).map_err(|error| format!("{error:#}"))?;
        Ok(self.describe(graph, &self.lock()))
    }

    fn describe(&self, graph: GraphSpec, state: &State) -> AudioGraphView {
        let nodes = graph
            .nodes
            .iter()
            .map(|node| {
                let (layout, problem) = match &node.kind {
                    NodeKind::Input => (
                        PortLayout {
                            main_inputs: 0,
                            sidechain_inputs: 0,
                            outputs: GRAPH_CONFIG.input_channels,
                        },
                        None,
                    ),
                    NodeKind::Output => (
                        PortLayout {
                            main_inputs: GRAPH_CONFIG.output_channels,
                            sidechain_inputs: 0,
                            outputs: 0,
                        },
                        None,
                    ),
                    NodeKind::Gain { .. } => (UNAVAILABLE_LAYOUT, None),
                    NodeKind::Plugin(_) => (
                        state
                            .loaded
                            .get(&node.id)
                            .map_or(UNAVAILABLE_LAYOUT, |loaded| loaded.layout),
                        state.problems.get(&node.id).cloned(),
                    ),
                };
                NodeStatus {
                    node: node.id,
                    main_inputs: layout.main_inputs,
                    sidechain_inputs: layout.sidechain_inputs,
                    outputs: layout.outputs,
                    problem,
                }
            })
            .collect();
        AudioGraphView { graph, nodes }
    }

    /// Save `graph` for the next take on, after loading its plugins and
    /// checking that it can be built. Nothing changes when it cannot.
    pub(crate) fn set_graph(&self, graph: GraphSpec) -> Result<AudioGraphView, String> {
        let mut state = self.lock();
        if state.loaded.values().any(Loaded::in_use) {
            return Err("a recording is using the plugins; try again once it ends".to_string());
        }
        let trial = self
            .compile(&graph, &mut state)
            .map_err(|error| error.to_string())?;
        drop(trial);
        for (node, loaded) in state.loaded.iter_mut() {
            self.save_state(*node, &mut loaded.control);
        }
        save_audio_graph(&self.paths, &graph).map_err(|error| format!("{error:#}"))?;
        state
            .loaded
            .retain(|node, _| plugins_in(&graph).any(|(id, _)| id == *node));
        Ok(self.describe(graph, &state))
    }

    /// Show the editor of the plugin at `node`.
    pub(crate) fn open_editor(&self, node: NodeId) -> Result<(), String> {
        let state = self.lock();
        let plugin = state
            .loaded
            .get(&node)
            .ok_or_else(|| format!("node {node} has no loaded plugin"))?;
        plugin
            .control
            .open_editor()
            .map_err(|error| error.to_string())
    }

    /// The plugins installed on this machine. Opens every bundle in a worker
    /// of its own, sandboxed because a plugin runs code when it is read.
    pub(crate) fn scan(&self) -> Result<Vec<PluginEntry>, String> {
        let host = self.host.as_ref().ok_or_else(|| NO_WORKER.to_string())?;
        let scan = host.scan(&default_folders(), Sandbox::On);
        for (path, error) in &scan.failures {
            tracing::info!(path = %path.display(), %error, "plugin skipped");
        }
        Ok(scan
            .plugins
            .into_iter()
            .map(|plugin| PluginEntry {
                effect: plugin.is_effect(),
                path: plugin.path.display().to_string(),
                class_id: plugin.class_id,
                name: plugin.name,
                vendor: plugin.vendor,
                category: plugin.category,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use plugin_graph::{Edge, GraphSpec, NodeKind, NodeSpec, PluginRef, Port, Sidechain};

    use super::PluginChains;
    use crate::config::{Paths, load_audio_graph};
    use crate::ipc::audio::NodeStatus;

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

    fn wire(from: u32, to: u32) -> Edge {
        Edge {
            from: Port {
                node: from,
                port: 0,
            },
            to: Port { node: to, port: 0 },
        }
    }

    fn with_node(kind: NodeKind) -> GraphSpec {
        let mut graph = GraphSpec::passthrough();
        graph.nodes.push(NodeSpec {
            id: 2,
            kind,
            x: 0.,
            y: 0.,
        });
        graph.edges = vec![wire(0, 2), wire(2, 1)];
        graph
    }

    #[test]
    fn a_graph_is_checked_before_it_is_saved() {
        let paths = scratch("chains-set");
        let chains = PluginChains::new(paths.clone());
        let gain = with_node(NodeKind::Gain { db: -6. });
        let mut looped = gain.clone();
        looped.edges.push(wire(2, 2));

        let saved = chains.set_graph(gain.clone());
        let refused = chains.set_graph(looped).err();
        assert_eq!(
            (
                saved.map(|view| view.graph),
                refused,
                load_audio_graph(&paths).unwrap(),
            ),
            (
                Ok(gain.clone()),
                Some("the wires form a loop".to_string()),
                gain
            )
        );
    }

    #[test]
    fn a_fresh_graph_reports_the_channels_of_its_endpoints() {
        let chains = PluginChains::new(scratch("chains-view"));
        assert_eq!(
            chains.view().unwrap().nodes,
            vec![
                NodeStatus {
                    node: 0,
                    main_inputs: 0,
                    sidechain_inputs: 0,
                    outputs: 1,
                    problem: None
                },
                NodeStatus {
                    node: 1,
                    main_inputs: 1,
                    sidechain_inputs: 0,
                    outputs: 0,
                    problem: None
                },
            ]
        );
    }

    /// Runs a real plugin, given by `LIMEN_TEST_VST3` with the worker in
    /// `LIMEN_VST_WORKER`, through the saved graph the way takes and the
    /// routing page do.
    #[test]
    #[ignore = "needs a worker and a plugin"]
    fn a_real_plugin_is_loaded_saved_run_and_reused() {
        let bundle = std::env::var("LIMEN_TEST_VST3").expect("LIMEN_TEST_VST3");
        let paths = scratch("chains-real");
        let chains = PluginChains::new(paths);
        let graph = with_node(NodeKind::Plugin(PluginRef {
            path: bundle.clone(),
            class_id: String::new(),
            name: "plugin".to_string(),
            enabled: true,
            sidechain: Sidechain::Off,
        }));
        let view = chains.set_graph(graph).unwrap();
        let plugin = view.nodes.iter().find(|node| node.node == 2).unwrap();
        assert_eq!(
            (
                plugin.main_inputs >= 1,
                plugin.outputs >= 1,
                plugin.problem.clone()
            ),
            (true, true, None)
        );

        let take = |chains: &PluginChains| {
            let mut chain = chains.build().expect("a chain");
            let signal = (0..48_000)
                .map(|frame| (frame as f32 * 0.05).sin() * 0.3)
                .collect::<Vec<_>>();
            let mut pcm = signal
                .chunks(4800)
                .flat_map(|chunk| chain.feed(chunk))
                .collect::<Vec<_>>();
            pcm.extend(chain.finish());
            (pcm, chain.failures())
        };
        let (first, failures) = take(&chains);
        let (second, _) = take(&chains);
        let loud = |pcm: &[i16]| pcm.iter().any(|sample| sample.unsigned_abs() > 300);
        assert_eq!(
            (
                first.len().abs_diff(16_000) < 200,
                second.len().abs_diff(16_000) < 200,
                loud(&first),
                loud(&second),
                failures,
                chains
                    .scan()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.path == bundle),
            ),
            (true, true, true, true, vec![], true)
        );
    }
}
