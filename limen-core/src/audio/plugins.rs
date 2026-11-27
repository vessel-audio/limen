//! The plugins a take runs through. Their workers are loaded when a take
//! first needs them (or the graph is saved) and kept between takes, since
//! starting one takes a couple of hundred milliseconds, which dictation
//! should not wait for each time.
//!
//! A worker that died is loaded again once, at the next take; a plugin that
//! cannot be loaded stays out of the graph, passed through, until the graph
//! is saved again.

use std::collections::{HashMap, HashSet};
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

use super::chain::{Chain, FailureHook, GRAPH_BLOCK, GRAPH_CONFIG, GRAPH_RATE};
use crate::config::{
    Paths, delete_plugin_state, load_audio_graph, load_plugin_state, save_audio_graph,
    save_plugin_state,
};
use crate::ipc::audio::{AudioGraphView, NodeStatus, PluginEntry};

/// How long a plugin may take to process a block before it counts as failed.
const DEADLINE: Duration = Duration::from_millis(50);

/// How often loaded plugins are asked what happened to them.
const POLL: Duration = Duration::from_millis(250);

/// How long a take waits for a plugin that an earlier take or a test is
/// still finishing with.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// What identifies a loaded plugin: a different one needs a new worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadKey {
    path: String,
    class_id: String,
    sidechain: Sidechain,
}

impl LoadKey {
    /// What names the plugin, for the state saved with it: not the
    /// side-chain, which is the same plugin's setting.
    fn identity(&self) -> String {
        format!("{}\n{}", self.path, self.class_id)
    }
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

    /// Wait for a take that is finishing to let go of the plugin.
    fn wait_until_idle(&self) -> bool {
        let steps = IDLE_WAIT.as_millis() / 10;
        for _ in 0..steps {
            if !self.in_use() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        !self.in_use()
    }
}

/// Why each node's plugin did not do its work, as far as a take saw.
type Failures = Arc<Mutex<HashMap<NodeId, String>>>;

/// A loaded plugin as a graph node, shared with the registry so that it
/// survives the graph.
struct Shared {
    node: NodeId,
    processor: Arc<Mutex<Vst3Processor>>,
    layout: PortLayout,
    latency: usize,
    failures: Failures,
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
        let result = self
            .processor
            .lock()
            .map_err(|_| ProcessError("the plugin's processor panicked".to_string()))?
            .process(frames, input, output);
        if let Err(error) = &result
            && let Ok(mut failures) = self.failures.lock()
        {
            failures.insert(self.node, error.0.clone());
        }
        result
    }
}

/// Stands in for a plugin that could not be loaded: a node that is passed
/// through from the first block, with the reason as its failure. It has the
/// channels the saved wires use, so that the rest of the graph still builds.
struct Unavailable {
    reason: String,
    layout: PortLayout,
}

impl Processor for Unavailable {
    fn layout(&self) -> PortLayout {
        self.layout
    }

    fn latency(&self) -> usize {
        0
    }

    fn process(&mut self, _: usize, _: &[f32], _: &mut [f32]) -> Result<(), ProcessError> {
        Err(ProcessError(self.reason.clone()))
    }
}

/// What a plugin that is not loaded is drawn with: one channel in and out.
const UNLOADED_LAYOUT: PortLayout = PortLayout {
    main_inputs: 1,
    sidechain_inputs: 0,
    outputs: 1,
};

/// The channels that the wires of `spec` use on `node`, at least one each
/// way.
fn wired_layout(spec: &GraphSpec, node: NodeId) -> PortLayout {
    let width = |ports: Vec<u16>| {
        ports
            .into_iter()
            .max()
            .map_or(1, |port| usize::from(port) + 1)
    };
    PortLayout {
        main_inputs: width(
            spec.edges
                .iter()
                .filter(|edge| edge.to.node == node)
                .map(|edge| edge.to.port)
                .collect(),
        ),
        sidechain_inputs: 0,
        outputs: width(
            spec.edges
                .iter()
                .filter(|edge| edge.from.node == node)
                .map(|edge| edge.from.port)
                .collect(),
        ),
    }
}

#[derive(Default)]
pub(super) struct State {
    loaded: HashMap<NodeId, Loaded>,
    /// Why a node's worker failed, as it told the registry.
    problems: HashMap<NodeId, String>,
    /// Plugins that could not be loaded, and why; they stay out until the
    /// graph is saved again.
    disabled: HashMap<NodeId, String>,
    /// Plugins whose latency changed since they loaded, which a new worker
    /// has to pick up.
    stale: HashSet<NodeId>,
    /// How many times each node's worker was found failed and replaced.
    crashes: HashMap<NodeId, u32>,
    /// Workers a compile replaced, kept until it is known to have worked, so
    /// that a graph that is refused leaves the plugins as they were.
    retired: HashMap<NodeId, Loaded>,
}

/// How many failures a plugin gets: it is loaded again once, and a second
/// failure leaves it out until the graph is saved again.
const MAX_FAILURES: u32 = 2;

/// The routing graph's plugins, and the graphs built from them.
pub(crate) struct PluginChains {
    pub(super) paths: Paths,
    host: Option<Vst3Host>,
    state: Mutex<State>,
    failures: Failures,
    notify: fn(&str, &str),
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
    /// The registry for the graph saved under `paths`; `notify` tells the
    /// user when a plugin was passed through. A thread of its own watches the
    /// loaded plugins, saving a plugin's state when its editor closes; it
    /// ends with the registry.
    pub(crate) fn new(paths: Paths, notify: fn(&str, &str)) -> Arc<Self> {
        let chains = Arc::new(Self {
            paths,
            host: worker().map(Vst3Host::new),
            state: Mutex::new(State::default()),
            failures: Failures::default(),
            notify,
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
    /// state of one whose editor closed, note one that failed, and mark one
    /// whose latency changed.
    fn poll(&self) {
        let mut state = self.lock();
        let State {
            loaded,
            problems,
            stale,
            ..
        } = &mut *state;
        for (node, plugin) in loaded.iter_mut() {
            for event in plugin.control.events() {
                match event {
                    PluginEvent::EditorClosed => {
                        let identity = plugin.key.identity();
                        self.save_state(*node, &identity, &mut plugin.control)
                    }
                    PluginEvent::Failed(reason) => {
                        problems.insert(*node, reason);
                    }
                    PluginEvent::LatencyChanged => {
                        stale.insert(*node);
                    }
                }
            }
        }
    }

    fn save_state(&self, node: NodeId, identity: &str, control: &mut Vst3Control) {
        let saved = control
            .save_state()
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                save_plugin_state(&self.paths, node, identity, &bytes)
                    .map_err(|error| format!("{error:#}"))
            });
        if let Err(error) = saved {
            tracing::warn!(node, %error, "could not save a plugin's state");
        }
    }

    /// The signal path for a take, or `None` when the saved graph is the
    /// default one or cannot be used, in which case the microphone goes
    /// straight to the recognizer as ever. Loads plugins that are not loaded
    /// yet, so it may take a while: call it off the async runtime.
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
        let compiled = self.compile(&spec, &mut state);
        state.retired.clear();
        match compiled {
            Ok(graph) => {
                let names = plugins_in(&spec)
                    .map(|(node, plugin)| (node, plugin.name.clone()))
                    .collect::<HashMap<_, _>>();
                let notify = self.notify;
                let hook: FailureHook = Arc::new(move |failures| {
                    let told = failures
                        .iter()
                        .map(|failure| {
                            let name = names.get(&failure.node).map_or("A plugin", String::as_str);
                            format!("{name}: {}", failure.reason)
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    notify("Limen", &format!("Passed through, not processed. {told}"));
                });
                Some(Chain::new(graph).with_hook(hook))
            }
            Err(error) => {
                tracing::warn!(error = %error, "audio graph unusable; recording without it");
                None
            }
        }
    }

    fn compile(&self, spec: &GraphSpec, state: &mut State) -> Result<CompiledGraph, GraphError> {
        let processors = plugins_in(spec)
            .map(|(node, plugin)| {
                let wired = wired_layout(spec, node);
                (node, self.processor(state, node, plugin, wired))
            })
            .collect::<HashMap<_, _>>();
        CompiledGraph::compile(spec, GRAPH_CONFIG, processors)
    }

    fn shared(&self, node: NodeId, loaded: &Loaded) -> Option<Shared> {
        let mut processor = loaded.processor.lock().ok()?;
        processor.restart().ok()?;
        Some(Shared {
            node,
            layout: processor.layout(),
            latency: processor.latency(),
            processor: loaded.processor.clone(),
            failures: self.failures.clone(),
        })
    }

    /// The processor for the plugin at `node`: the loaded one, started over,
    /// or a new one. A worker that died or whose latency changed is loaded
    /// again; one that cannot be loaded leaves the node passed through.
    fn processor(
        &self,
        state: &mut State,
        node: NodeId,
        plugin: &PluginRef,
        wired: PortLayout,
    ) -> Box<dyn Processor> {
        let unavailable = |reason: String| -> Box<dyn Processor> {
            Box::new(Unavailable {
                reason,
                layout: wired,
            })
        };
        let key = LoadKey {
            path: plugin.path.clone(),
            class_id: plugin.class_id.clone(),
            sidechain: plugin.sidechain,
        };
        if let Some(reason) = state.disabled.get(&node) {
            return unavailable(reason.clone());
        }
        let stale = state.stale.contains(&node);
        let same_key = state
            .loaded
            .get(&node)
            .is_some_and(|current| current.key == key);
        // A plugin whose worker died, or that failed in the take just ended,
        // is loaded again once; failing again leaves it out.
        let failed = same_key
            && (state
                .loaded
                .get(&node)
                .is_some_and(|current| !current.control.is_running())
                || self
                    .failures
                    .lock()
                    .is_ok_and(|failures| failures.contains_key(&node)));
        if !same_key {
            state.crashes.remove(&node);
        }
        if failed {
            let crashes = state.crashes.entry(node).or_default();
            *crashes += 1;
            if *crashes >= MAX_FAILURES {
                let reason = format!(
                    "{} keeps failing; save the graph to try it again",
                    plugin.name
                );
                state.disabled.insert(node, reason.clone());
                if let Some(old) = state.loaded.remove(&node) {
                    state.retired.insert(node, old);
                }
                return unavailable(reason);
            }
        }
        let reusable = state
            .loaded
            .get(&node)
            .filter(|current| current.key == key && !failed && !stale);
        if let Some(current) = reusable {
            if !current.wait_until_idle() {
                return unavailable("an earlier take is still using it".to_string());
            }
            if let Some(shared) = self.shared(node, current) {
                if let Ok(mut failures) = self.failures.lock() {
                    failures.remove(&node);
                }
                state.problems.remove(&node);
                return Box::new(shared);
            }
        }
        // A new worker is needed. The old one's settings go on to the new when
        // it is the same plugin and still running; the state saved for a
        // different plugin is never restored into this one. The old worker is
        // kept until the compile is known to have worked, so that a graph that
        // is refused leaves it as it was.
        if let Some(mut old) = state.loaded.remove(&node) {
            let identity = old.key.identity();
            if old.key.identity() == key.identity() && old.control.is_running() {
                self.save_state(node, &identity, &mut old.control);
            }
            state.retired.insert(node, old);
        }
        state.stale.remove(&node);
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
                let saved = load_plugin_state(&self.paths, node, &key.identity())
                    .ok()
                    .flatten();
                host.load(
                    std::path::Path::new(&plugin.path),
                    &plugin.class_id,
                    options,
                    saved.as_deref(),
                )
                .map_err(|error| error.to_string())
            }
        };
        match loaded {
            Ok((control, processor)) => {
                let shared = Shared {
                    node,
                    layout: processor.layout(),
                    latency: processor.latency(),
                    processor: Arc::new(Mutex::new(processor)),
                    failures: self.failures.clone(),
                };
                if let Ok(mut failures) = self.failures.lock() {
                    failures.remove(&node);
                }
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
                state.disabled.insert(node, reason.clone());
                unavailable(reason)
            }
        }
    }

    /// The saved routing graph and how each node of it is doing.
    pub(crate) fn view(&self) -> Result<AudioGraphView, String> {
        let graph = load_audio_graph(&self.paths).map_err(|error| format!("{error:#}"))?;
        Ok(self.describe(graph, &self.lock()))
    }

    fn describe(&self, graph: GraphSpec, state: &State) -> AudioGraphView {
        let failed = self
            .failures
            .lock()
            .map(|failures| failures.clone())
            .unwrap_or_default();
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
                    NodeKind::Gain { .. } => (UNLOADED_LAYOUT, None),
                    NodeKind::Plugin(_) => (
                        state
                            .loaded
                            .get(&node.id)
                            .map_or_else(|| wired_layout(&graph, node.id), |loaded| loaded.layout),
                        state
                            .disabled
                            .get(&node.id)
                            .or_else(|| failed.get(&node.id))
                            .or_else(|| state.problems.get(&node.id))
                            .cloned(),
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

    /// Save `graph` for the next take on, after checking its topology,
    /// loading its plugins, and checking that it can be built. Nothing is
    /// saved when it cannot. Saving again is also what lets a plugin that
    /// failed to load be tried again.
    pub(crate) fn set_graph(&self, graph: GraphSpec) -> Result<AudioGraphView, String> {
        graph.validate().map_err(|error| error.to_string())?;
        let mut state = self.lock();
        if state.loaded.values().any(Loaded::in_use) {
            return Err("a recording is using the plugins; try again once it ends".to_string());
        }
        // Saving again is how a plugin that was left out is tried again.
        let (disabled, crashes) = (
            std::mem::take(&mut state.disabled),
            std::mem::take(&mut state.crashes),
        );
        state.retired.clear();
        let trial = match self.compile(&graph, &mut state) {
            Ok(trial) => trial,
            Err(error) => {
                // A graph that is refused leaves every plugin as it was: the
                // workers the compile replaced come back.
                for (node, old) in std::mem::take(&mut state.retired) {
                    state.loaded.insert(node, old);
                }
                (state.disabled, state.crashes) = (disabled, crashes);
                return Err(error.to_string());
            }
        };
        drop(trial);
        state.retired.clear();
        for (node, loaded) in state.loaded.iter_mut() {
            let identity = loaded.key.identity();
            self.save_state(*node, &identity, &mut loaded.control);
        }
        let before = load_audio_graph(&self.paths).unwrap_or_else(|_| GraphSpec::passthrough());
        save_audio_graph(&self.paths, &graph).map_err(|error| format!("{error:#}"))?;
        // A node that is gone takes its settings with it, so that a node
        // added later under the same id does not inherit them.
        for (node, _) in plugins_in(&before) {
            if !plugins_in(&graph).any(|(id, _)| id == node) {
                let _ = delete_plugin_state(&self.paths, node);
            }
        }
        state
            .loaded
            .retain(|node, _| plugins_in(&graph).any(|(id, _)| id == *node));
        Ok(self.describe(graph, &state))
    }

    /// Show the editor of the plugin at `node`, once it has opened.
    pub(crate) fn open_editor(&self, node: NodeId) -> Result<(), String> {
        let mut state = self.lock();
        let plugin = state
            .loaded
            .get_mut(&node)
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

    /// The process ids of the loaded plugins' workers, by node.
    #[cfg(test)]
    fn worker_pids(&self) -> Vec<u32> {
        self.lock()
            .loaded
            .values()
            .map(|loaded| loaded.control.worker_pid())
            .collect()
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

    fn registry(paths: Paths) -> std::sync::Arc<PluginChains> {
        PluginChains::new(paths, |_, _| {})
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

    fn plugin_graph_of(bundle: String) -> GraphSpec {
        with_node(NodeKind::Plugin(PluginRef {
            path: bundle,
            class_id: String::new(),
            name: "plugin".to_string(),
            enabled: true,
            sidechain: Sidechain::Off,
        }))
    }

    #[test]
    fn a_graph_is_checked_before_it_is_saved() {
        let paths = scratch("chains-set");
        let chains = registry(paths.clone());
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
        let chains = registry(scratch("chains-view"));
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

    #[test]
    fn a_plugin_that_cannot_load_keeps_its_wires_and_leaves_the_rest_of_the_graph_alone() {
        // Two wires leave the plugin and two arrive, as a stereo plugin's
        // would; with no worker the plugin is out, but the graph still builds.
        let paths = scratch("chains-unavailable");
        let chains = registry(paths.clone());
        let mut graph = plugin_graph_of("/no/such/plugin.vst3".to_string());
        graph.edges.push(Edge {
            from: Port { node: 0, port: 0 },
            to: Port { node: 2, port: 1 },
        });
        graph.edges.push(Edge {
            from: Port { node: 2, port: 1 },
            to: Port { node: 1, port: 0 },
        });
        crate::config::save_audio_graph(&paths, &graph).unwrap();
        let chain = chains.build();
        let view = chains.view().unwrap();
        let plugin = view.nodes.iter().find(|node| node.node == 2).unwrap();
        assert_eq!(
            (
                chain.is_some(),
                plugin.main_inputs,
                plugin.outputs,
                plugin.problem.is_some()
            ),
            (true, 2, 2, true)
        );
    }

    /// Runs a real plugin, given by `LIMEN_TEST_VST3` with the worker in
    /// `LIMEN_VST_WORKER`, through the saved graph the way takes and the
    /// routing page do.
    #[test]
    #[ignore = "needs a worker and a plugin"]
    fn a_real_plugin_is_loaded_saved_run_and_reused() {
        let bundle = std::env::var("LIMEN_TEST_VST3").expect("LIMEN_TEST_VST3");
        let chains = registry(scratch("chains-real"));
        let view = chains.set_graph(plugin_graph_of(bundle.clone())).unwrap();
        let plugin = view.nodes.iter().find(|node| node.node == 2).unwrap();
        assert_eq!(
            (
                plugin.main_inputs >= 1,
                plugin.outputs >= 1,
                plugin.problem.clone()
            ),
            (true, true, None)
        );

        let (first, failures) = take(&chains, None);
        let (second, _) = take(&chains, None);
        let loud = |pcm: &[i16]| pcm.iter().any(|sample| sample.unsigned_abs() > 300);
        assert_eq!(
            (
                first.len(),
                second.len(),
                loud(&first),
                loud(&second),
                failures,
                chains
                    .scan()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.path == bundle),
            ),
            (16_000, 16_000, true, true, vec![], true)
        );
    }

    /// One second through the saved graph, in 4800-frame chunks. With
    /// `kill`, the plugin's worker is killed after the first half.
    fn take(
        chains: &PluginChains,
        kill: Option<&dyn Fn()>,
    ) -> (Vec<i16>, Vec<plugin_graph::Failure>) {
        let mut chain = chains.build().expect("a chain");
        let signal = (0..48_000)
            .map(|frame| (frame as f32 * 0.05).sin() * 0.3)
            .collect::<Vec<_>>();
        let mut pcm = Vec::new();
        for (index, chunk) in signal.chunks(4800).enumerate() {
            if index == 5
                && let Some(kill) = kill
            {
                kill();
            }
            pcm.extend(chain.feed(chunk));
        }
        pcm.extend(chain.finish());
        (pcm, chain.failures())
    }

    /// A worker that dies in the middle of a take costs that plugin only: the
    /// take still comes out whole, the next one loads the plugin again, and a
    /// plugin that dies again is left out, passed through, from then on.
    #[test]
    #[ignore = "needs a worker and a plugin"]
    fn a_worker_killed_mid_take_is_loaded_again_once_and_then_left_out() {
        let bundle = std::env::var("LIMEN_TEST_VST3").expect("LIMEN_TEST_VST3");
        let chains = registry(scratch("chains-kill"));
        chains.set_graph(plugin_graph_of(bundle)).unwrap();
        let kill = || {
            for pid in chains.worker_pids() {
                // SAFETY: the pid is a child of this process that is still
                // owned by the registry.
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        };
        let (first, first_failures) = take(&chains, Some(&kill));
        let (second, second_failures) = take(&chains, Some(&kill));
        let (third, third_failures) = take(&chains, None);
        let problem = chains
            .view()
            .unwrap()
            .nodes
            .iter()
            .find(|node| node.node == 2)
            .and_then(|node| node.problem.clone());
        assert_eq!(
            (
                [first.len(), second.len(), third.len()],
                [first_failures.len(), second_failures.len()],
                third_failures.len(),
                problem.is_some_and(|problem| problem.contains("keeps failing")),
            ),
            ([16_000; 3], [1, 1], 1, true)
        );
    }

    /// A graph that is refused leaves the plugin's worker as it was.
    #[test]
    #[ignore = "needs a worker and a plugin"]
    fn a_refused_graph_leaves_the_running_worker_alone() {
        let bundle = std::env::var("LIMEN_TEST_VST3").expect("LIMEN_TEST_VST3");
        let chains = registry(scratch("chains-refused"));
        let graph = plugin_graph_of(bundle);
        chains.set_graph(graph.clone()).unwrap();
        let before = chains.worker_pids();
        // The side-chain is turned on, which replaces the worker, and a wire
        // to a channel nothing has is added, which refuses the graph.
        let mut refused = graph;
        if let NodeKind::Plugin(plugin) = &mut refused.nodes[2].kind {
            plugin.sidechain = Sidechain::On;
        }
        refused.edges.push(Edge {
            from: Port { node: 0, port: 0 },
            to: Port { node: 2, port: 9 },
        });
        let error = chains.set_graph(refused).err();
        assert_eq!(
            (error.is_some(), chains.worker_pids() == before),
            (true, true)
        );
    }
}
