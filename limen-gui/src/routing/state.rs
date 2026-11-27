//! The Audio routing page's state: the graph being edited, and what to ask
//! of the daemon after each edit. Nothing here touches the window, so the
//! rules of wiring are tested without one.

use limen_core::{AudioGraphView, NodeStatus, PluginEntry, TestFiles};
use plugin_graph::{Edge, GraphSpec, NodeId, NodeKind, NodeSpec, PluginRef, Port, Sidechain};

use super::geometry::{Side, distance_to_wire, port_point, wire_curve};
use crate::bridge::AudioRequest;

/// How near a click must be to a wire to select it, in pixels.
const WIRE_REACH: f32 = 8.;
/// The range a gain node can be set to, in decibels.
const GAIN_RANGE: std::ops::RangeInclusive<f32> = -24.0..=24.0;

/// What is selected on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Selection {
    Nothing,
    Node(NodeId),
    Wire(Edge),
}

/// What the page tells the user about the last thing they did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// The wire they tried would feed a node's output back into itself.
    WouldLoop,
    /// The daemon refused or failed something.
    Daemon(String),
}

/// The installed plugins, as far as they have been listed.
#[derive(Debug, Clone, PartialEq)]
pub enum Plugins {
    NotListed,
    Listing(u64),
    Listed(Vec<PluginEntry>),
    Failed(String),
}

/// The test recording.
#[derive(Debug, Clone, PartialEq)]
pub enum TestState {
    Idle,
    Recording(u64),
    Ready(TestFiles),
}

/// A node held by the pointer: how far from its corner it was grabbed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grab {
    node: NodeId,
    dx: f32,
    dy: f32,
    /// Whether the pointer has carried it anywhere yet.
    moved: bool,
}

pub struct RoutingState {
    /// The graph shown and edited, ahead of the daemon's while a save is in
    /// flight.
    pub graph: Option<GraphSpec>,
    saved: Option<AudioGraphView>,
    pub plugins: Plugins,
    pub selection: Selection,
    /// An output port that was clicked and waits for an input to wire to.
    pub wiring: Option<Port>,
    pub notice: Option<Notice>,
    /// Whether the list of plugins to add is open.
    pub palette: bool,
    pub test: TestState,
    grab: Option<Grab>,
    next_request: u64,
    /// The newest request about the graph; older replies are ignored.
    latest: Option<u64>,
    /// Whether the graph was asked for since the page was last shown.
    asked: bool,
}

impl Default for RoutingState {
    fn default() -> Self {
        Self {
            graph: None,
            saved: None,
            plugins: Plugins::NotListed,
            selection: Selection::Nothing,
            wiring: None,
            notice: None,
            palette: false,
            test: TestState::Idle,
            grab: None,
            next_request: 0,
            latest: None,
            asked: false,
        }
    }
}

/// Whether following the wires from `start` reaches `goal`.
fn reaches(graph: &GraphSpec, start: NodeId, goal: NodeId) -> bool {
    let mut seen = vec![start];
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        if node == goal {
            return true;
        }
        for edge in graph.edges.iter().filter(|edge| edge.from.node == node) {
            if !seen.contains(&edge.to.node) {
                seen.push(edge.to.node);
                stack.push(edge.to.node);
            }
        }
    }
    false
}

impl RoutingState {
    fn next_id(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    /// Called as the page is drawn: ask for the graph each time the page is
    /// shown, so what it shows is what the daemon has.
    pub fn follow(&mut self, visible: bool) -> Option<AudioRequest> {
        match (visible, self.asked) {
            (true, false) => {
                self.asked = true;
                let id = self.next_id();
                self.latest = Some(id);
                Some(AudioRequest::Get { id })
            }
            (false, true) => {
                self.asked = false;
                self.wiring = None;
                self.palette = false;
                None
            }
            (true, true) | (false, false) => None,
        }
    }

    /// Ask for the graph again after it could not be read.
    pub fn retry(&mut self) {
        self.asked = false;
        self.notice = None;
    }

    /// What the daemon says of `node`.
    pub fn status(&self, node: NodeId) -> Option<&NodeStatus> {
        self.saved
            .as_ref()
            .and_then(|view| view.nodes.iter().find(|status| status.node == node))
    }

    /// The daemon's answer to a read or a save of the graph.
    pub fn graph_reply(&mut self, id: u64, result: Result<AudioGraphView, String>) {
        if self.latest != Some(id) {
            return;
        }
        self.latest = None;
        match result {
            Ok(view) => {
                self.graph = Some(view.graph.clone());
                self.saved = Some(view);
                self.notice = None;
            }
            Err(message) => {
                self.notice = Some(Notice::Daemon(message));
                // Show what the daemon has, not the edit it refused.
                if let Some(view) = &self.saved {
                    self.graph = Some(view.graph.clone());
                }
            }
        }
    }

    pub fn scanned(&mut self, id: u64, result: Result<Vec<PluginEntry>, String>) {
        if self.plugins == Plugins::Listing(id) {
            self.plugins = match result {
                Ok(plugins) => Plugins::Listed(plugins),
                Err(message) => Plugins::Failed(message),
            };
        }
    }

    pub fn tested(&mut self, id: u64, result: Result<TestFiles, String>) {
        if self.test == TestState::Recording(id) {
            self.test = match result {
                Ok(files) => TestState::Ready(files),
                Err(message) => {
                    self.notice = Some(Notice::Daemon(message));
                    TestState::Idle
                }
            };
        }
    }

    pub fn editor_opened(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.notice = Some(Notice::Daemon(message));
        }
    }

    /// Send the edited graph to the daemon.
    fn commit(&mut self) -> Option<AudioRequest> {
        let graph = self.graph.clone()?;
        let id = self.next_id();
        self.latest = Some(id);
        Some(AudioRequest::Set { id, graph })
    }

    /// Open the list of plugins to add, listing them the first time.
    pub fn open_palette(&mut self) -> Option<AudioRequest> {
        self.palette = true;
        match self.plugins {
            Plugins::NotListed | Plugins::Failed(_) => {
                let id = self.next_id();
                self.plugins = Plugins::Listing(id);
                Some(AudioRequest::Scan { id })
            }
            Plugins::Listing(_) | Plugins::Listed(_) => None,
        }
    }

    fn add_node(&mut self, kind: NodeKind) -> Option<AudioRequest> {
        let graph = self.graph.as_mut()?;
        let id = graph
            .nodes
            .iter()
            .map(|node| node.id)
            .max()
            .map_or(0, |id| id + 1);
        let placed = graph.nodes.len().saturating_sub(2);
        graph.nodes.push(NodeSpec {
            id,
            kind,
            x: 210. + 30. * (placed % 5) as f32,
            y: 90. + 70. * (placed % 4) as f32,
        });
        self.selection = Selection::Node(id);
        self.palette = false;
        self.commit()
    }

    pub fn add_plugin(&mut self, entry: &PluginEntry) -> Option<AudioRequest> {
        self.add_node(NodeKind::Plugin(PluginRef {
            path: entry.path.clone(),
            class_id: entry.class_id.clone(),
            name: entry.name.clone(),
            enabled: true,
            sidechain: Sidechain::Off,
        }))
    }

    pub fn add_gain(&mut self) -> Option<AudioRequest> {
        self.add_node(NodeKind::Gain { db: 0. })
    }

    /// Remove a node and the wires to it. The input and the output stay.
    pub fn remove_node(&mut self, node: NodeId) -> Option<AudioRequest> {
        let graph = self.graph.as_mut()?;
        let removable = graph.nodes.iter().any(|candidate| {
            candidate.id == node
                && match candidate.kind {
                    NodeKind::Gain { .. } | NodeKind::Plugin(_) => true,
                    NodeKind::Input | NodeKind::Output => false,
                }
        });
        if !removable {
            return None;
        }
        graph.nodes.retain(|candidate| candidate.id != node);
        graph
            .edges
            .retain(|edge| edge.from.node != node && edge.to.node != node);
        self.selection = Selection::Nothing;
        self.wiring = None;
        self.commit()
    }

    fn edit_node(
        &mut self,
        node: NodeId,
        edit: impl FnOnce(&mut NodeKind),
    ) -> Option<AudioRequest> {
        let target = self
            .graph
            .as_mut()?
            .nodes
            .iter_mut()
            .find(|candidate| candidate.id == node)?;
        edit(&mut target.kind);
        self.commit()
    }

    /// Turn a plugin on or off; off passes its main input through.
    pub fn toggle_enabled(&mut self, node: NodeId) -> Option<AudioRequest> {
        self.edit_node(node, |kind| {
            if let NodeKind::Plugin(plugin) = kind {
                plugin.enabled = !plugin.enabled;
            }
        })
    }

    /// Turn a plugin's side-chain input on or off.
    pub fn toggle_sidechain(&mut self, node: NodeId) -> Option<AudioRequest> {
        self.edit_node(node, |kind| {
            if let NodeKind::Plugin(plugin) = kind {
                plugin.sidechain = match plugin.sidechain {
                    Sidechain::Off => Sidechain::On,
                    Sidechain::On => Sidechain::Off,
                };
            }
        })
    }

    /// Change a gain node's level by `decibels`.
    pub fn nudge_gain(&mut self, node: NodeId, decibels: f32) -> Option<AudioRequest> {
        self.edit_node(node, |kind| {
            if let NodeKind::Gain { db } = kind {
                *db = (*db + decibels).clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end());
            }
        })
    }

    /// An output port was clicked: it starts a wire, or stops the one it
    /// started.
    pub fn click_output(&mut self, port: Port) {
        self.wiring = match self.wiring {
            Some(started) if started == port => None,
            Some(_) | None => Some(port),
        };
        self.selection = Selection::Nothing;
    }

    /// An input port was clicked: it ends the wire that was started.
    pub fn click_input(&mut self, port: Port) -> Option<AudioRequest> {
        let from = self.wiring.take()?;
        let graph = self.graph.as_mut()?;
        let edge = Edge { from, to: port };
        if graph.edges.contains(&edge) {
            return None;
        }
        if from.node == port.node || reaches(graph, port.node, from.node) {
            self.notice = Some(Notice::WouldLoop);
            return None;
        }
        graph.edges.push(edge);
        self.selection = Selection::Wire(edge);
        self.notice = None;
        self.commit()
    }

    pub fn remove_wire(&mut self, edge: Edge) -> Option<AudioRequest> {
        let graph = self.graph.as_mut()?;
        graph.edges.retain(|candidate| *candidate != edge);
        self.selection = Selection::Nothing;
        self.commit()
    }

    /// The wire nearest `point` on the canvas, if one is near enough.
    pub fn wire_at(&self, point: (f32, f32)) -> Option<Edge> {
        let graph = self.graph.as_ref()?;
        let node = |id: NodeId| graph.nodes.iter().find(|node| node.id == id);
        graph
            .edges
            .iter()
            .filter_map(|edge| {
                let from = port_point(
                    node(edge.from.node)?,
                    Side::Output,
                    usize::from(edge.from.port),
                );
                let to = port_point(node(edge.to.node)?, Side::Input, usize::from(edge.to.port));
                Some((*edge, distance_to_wire(wire_curve(from, to), point)))
            })
            .filter(|(_, distance)| *distance <= WIRE_REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(edge, _)| edge)
    }

    /// A click on the canvas itself: select the wire under it, or nothing.
    pub fn click_canvas(&mut self, point: (f32, f32)) {
        self.wiring = None;
        self.selection = match self.wire_at(point) {
            Some(edge) => Selection::Wire(edge),
            None => Selection::Nothing,
        };
    }

    /// A node was grabbed at `point`, in canvas coordinates.
    pub fn begin_drag(&mut self, node: NodeId, point: (f32, f32)) {
        let Some(found) = self
            .graph
            .as_ref()
            .and_then(|graph| graph.nodes.iter().find(|candidate| candidate.id == node))
        else {
            return;
        };
        self.grab = Some(Grab {
            node,
            dx: point.0 - found.x,
            dy: point.1 - found.y,
            moved: false,
        });
        self.selection = Selection::Node(node);
    }

    /// The pointer moved to `point` while a node is held.
    pub fn drag_to(&mut self, point: (f32, f32)) {
        let (Some(grab), Some(graph)) = (self.grab, self.graph.as_mut()) else {
            return;
        };
        self.grab = Some(Grab {
            moved: true,
            ..grab
        });
        if let Some(node) = graph.nodes.iter_mut().find(|node| node.id == grab.node) {
            node.x = (point.0 - grab.dx).max(0.);
            node.y = (point.1 - grab.dy).max(0.);
        }
    }

    /// The node was let go: save where it ended up, if it went anywhere.
    pub fn end_drag(&mut self) -> Option<AudioRequest> {
        match self.grab.take()?.moved {
            true => self.commit(),
            false => None,
        }
    }

    pub fn record_test(&mut self, seconds: u32) -> Option<AudioRequest> {
        match self.test {
            TestState::Recording(_) => None,
            TestState::Idle | TestState::Ready(_) => {
                let id = self.next_id();
                self.test = TestState::Recording(id);
                self.notice = None;
                Some(AudioRequest::Test { id, seconds })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use limen_core::AudioGraphView;
    use plugin_graph::{Edge, GraphSpec, NodeKind, Port};

    use super::{Notice, RoutingState, Selection};
    use crate::bridge::AudioRequest;

    fn port(node: u32) -> Port {
        Port { node, port: 0 }
    }

    /// A state that has read the default graph from the daemon.
    fn loaded() -> RoutingState {
        let mut state = RoutingState::default();
        let Some(AudioRequest::Get { id }) = state.follow(true) else {
            panic!("the page asks for the graph when shown");
        };
        state.graph_reply(
            id,
            Ok(AudioGraphView {
                graph: GraphSpec::passthrough(),
                nodes: vec![],
            }),
        );
        state
    }

    /// Add a gain node (id 2) with no wires, as the page does.
    fn with_gain() -> (RoutingState, GraphSpec) {
        let mut state = loaded();
        state.add_gain().unwrap();
        let graph = state.graph.clone().unwrap();
        (state, graph)
    }

    fn set_graph(request: Option<AudioRequest>) -> Option<GraphSpec> {
        match request {
            Some(AudioRequest::Set { graph, .. }) => Some(graph),
            Some(_) | None => None,
        }
    }

    #[test]
    fn the_graph_is_asked_for_once_each_time_the_page_is_shown() {
        let mut state = RoutingState::default();
        let asked = [
            state.follow(true),
            state.follow(true),
            state.follow(false),
            state.follow(true),
        ]
        .map(|request| request.is_some());
        assert_eq!(asked, [true, false, false, true]);
    }

    #[test]
    fn clicking_an_output_and_then_an_input_wires_them() {
        let (mut state, mut expected) = with_gain();
        state.click_output(port(0));
        let request = state.click_input(port(2));
        expected.edges.push(Edge {
            from: port(0),
            to: port(2),
        });
        assert_eq!(
            (set_graph(request), state.wiring, state.selection),
            (
                Some(expected.clone()),
                None,
                Selection::Wire(expected.edges[1])
            )
        );
    }

    #[test]
    fn a_wire_that_would_loop_back_or_repeat_is_refused() {
        let (mut state, _) = with_gain();
        // Input -> gain -> output, then output... gain's output back to its own input.
        for (from, to) in [(0, 2), (2, 1)] {
            state.click_output(port(from));
            state.click_input(port(to));
        }
        let wired = state.graph.clone().unwrap();
        state.click_output(port(2));
        let looped = state.click_input(port(2));
        let loop_notice = state.notice.clone();
        state.click_output(port(0));
        let repeated = state.click_input(port(2));
        assert_eq!(
            (looped, loop_notice, repeated, state.graph.unwrap()),
            (None, Some(Notice::WouldLoop), None, wired)
        );
    }

    #[test]
    fn removing_a_node_removes_its_wires_but_never_the_endpoints() {
        let (mut state, _) = with_gain();
        for (from, to) in [(0, 2), (2, 1)] {
            state.click_output(port(from));
            state.click_input(port(to));
        }
        let endpoint = state.remove_node(0);
        let removed = set_graph(state.remove_node(2));
        // Back to the default graph, whose own wire from input to output
        // was never touched.
        assert_eq!((endpoint, removed), (None, Some(GraphSpec::passthrough())));
    }

    #[test]
    fn a_stale_reply_is_ignored_and_a_refusal_shows_what_the_daemon_has() {
        let mut state = loaded();
        let first = state.add_gain().unwrap();
        let second = state.nudge_gain(2, 3.).unwrap();
        let (AudioRequest::Set { id: first, .. }, AudioRequest::Set { id: second, graph }) =
            (first, second)
        else {
            panic!("edits are saved with Set");
        };
        // The older reply, whatever it says, changes nothing.
        state.graph_reply(first, Err("old".to_string()));
        let before = (state.graph.clone().unwrap(), state.notice.clone());
        state.graph_reply(second, Err("a plugin did not load".to_string()));
        assert_eq!(
            (before, state.graph, state.notice),
            (
                (graph, None),
                Some(GraphSpec::passthrough()),
                Some(Notice::Daemon("a plugin did not load".to_string()))
            )
        );
    }

    #[test]
    fn a_dragged_node_is_saved_where_it_was_let_go() {
        let (mut state, _) = with_gain();
        let node = state.graph.as_ref().unwrap().nodes[2].clone();
        state.begin_drag(2, (node.x + 10., node.y + 5.));
        state.drag_to((500., 300.));
        let moved = state.graph.clone().unwrap().nodes[2].clone();
        let saved = set_graph(state.end_drag()).map(|graph| graph.nodes[2].clone());
        assert_eq!(
            (
                moved.x,
                moved.y,
                saved.map(|node| (node.x, node.y)),
                state.end_drag()
            ),
            (490., 295., Some((490., 295.)), None)
        );
        assert!(matches!(moved.kind, NodeKind::Gain { .. }));
    }

    #[test]
    fn a_node_that_was_only_clicked_is_not_saved() {
        let (mut state, _) = with_gain();
        state.begin_drag(2, (230., 100.));
        assert_eq!(state.end_drag(), None);
    }
}
