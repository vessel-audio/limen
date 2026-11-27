//! The Audio routing page's state: the graph being edited, and what to ask
//! of the daemon after each edit. Nothing here touches the window, so the
//! rules of wiring are tested without one.

use limen_core::{AudioGraphView, NodeStatus, PluginEntry, TestFiles};
use plugin_graph::{Edge, GraphSpec, NodeId, NodeKind, NodeSpec, PluginRef, Port, Sidechain};

use super::geometry::{Side, distance_to_wire, port_point, slots, wire_curve};
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
    /// The request about the graph that the daemon has not answered. Only
    /// one goes out at a time, so that saves reach the daemon in the order
    /// they were made.
    in_flight: Option<u64>,
    /// Whether the graph was edited since the request in flight was made.
    dirty: bool,
    /// Whether the graph was asked for since the page was last shown.
    asked: bool,
    /// The take, and whether dictation was idle, when the page last asked: a
    /// plugin may have failed in a take that has ended since.
    seen: Option<(u64, bool)>,
    /// The next node id, which never goes back to one used before.
    next_node: NodeId,
    /// How far the canvas is scrolled, in canvas coordinates.
    pub pan: (f32, f32),
    /// Which output and which input the add-connection form has picked, as
    /// positions in [`Self::output_ports`] and [`Self::input_ports`].
    connect_from: usize,
    connect_to: usize,
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
            in_flight: None,
            dirty: false,
            asked: false,
            seen: None,
            next_node: 0,
            pan: (0., 0.),
            connect_from: 0,
            connect_to: 0,
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
    pub fn follow(
        &mut self,
        visible: bool,
        dictation: Option<(u64, bool)>,
    ) -> Option<AudioRequest> {
        // A take that began or ended may have changed how the plugins are
        // doing, so the page asks again.
        if visible && self.asked && dictation != self.seen {
            self.asked = false;
        }
        if visible && !self.asked {
            self.seen = dictation;
        }
        match (visible, self.asked) {
            // Asked again once the request in flight is answered.
            (true, false) if self.in_flight.is_some() => None,
            (true, false) => {
                self.asked = true;
                let id = self.next_id();
                self.in_flight = Some(id);
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

    /// The daemon's answer to a read or a save of the graph. Returns the save
    /// of what was edited meanwhile, if anything was.
    pub fn graph_reply(
        &mut self,
        id: u64,
        result: Result<AudioGraphView, String>,
    ) -> Option<AudioRequest> {
        if self.in_flight != Some(id) {
            return None;
        }
        self.in_flight = None;
        let edited = std::mem::take(&mut self.dirty);
        match result {
            Ok(view) => {
                // Edits made since go on top of what the daemon has now.
                if !edited {
                    self.graph = Some(view.graph.clone());
                }
                self.saved = Some(view);
                self.notice = None;
                match edited {
                    true => self.commit(),
                    false => None,
                }
            }
            Err(message) => {
                self.notice = Some(Notice::Daemon(message));
                // Show what the daemon has, not the edit it refused, and
                // drop what was piled on top of it.
                if let Some(view) = &self.saved {
                    self.graph = Some(view.graph.clone());
                }
                None
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
            // What the daemon says of its plugins may have changed meanwhile.
            self.asked = false;
        }
    }

    pub fn editor_opened(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.notice = Some(Notice::Daemon(message));
        }
        self.asked = false;
    }

    /// Send the edited graph to the daemon, or, while an earlier request is
    /// still out, remember to once it is answered.
    fn commit(&mut self) -> Option<AudioRequest> {
        let graph = self.graph.clone()?;
        if self.in_flight.is_some() {
            self.dirty = true;
            return None;
        }
        let id = self.next_id();
        self.in_flight = Some(id);
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
            .map(|node| node.id + 1)
            .max()
            .unwrap_or(0)
            .max(self.next_node);
        self.next_node = id + 1;
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

    /// Remove what is selected: a wire, or a node that can be removed.
    pub fn remove_selected(&mut self) -> Option<AudioRequest> {
        match self.selection {
            Selection::Wire(edge) => self.remove_wire(edge),
            Selection::Node(node) => self.remove_node(node),
            Selection::Nothing => None,
        }
    }

    /// Scroll the canvas by `delta` canvas pixels, never past its origin.
    pub fn scroll_by(&mut self, delta: (f32, f32)) {
        self.pan = (
            (self.pan.0 + delta.0).max(0.),
            (self.pan.1 + delta.1).max(0.),
        );
    }

    /// Every output channel of the graph, in the order the form offers them.
    pub fn output_ports(&self) -> Vec<Port> {
        self.ports(Side::Output)
    }

    /// Every input channel of the graph, in the order the form offers them.
    pub fn input_ports(&self) -> Vec<Port> {
        self.ports(Side::Input)
    }

    fn ports(&self, side: Side) -> Vec<Port> {
        let Some(graph) = &self.graph else {
            return Vec::new();
        };
        graph
            .nodes
            .iter()
            .flat_map(|node| {
                let size = slots(node, self.status(node.id));
                let count = match side {
                    Side::Input => size.inputs,
                    Side::Output => size.outputs,
                };
                (0..count).map(move |port| Port {
                    node: node.id,
                    port: port as u16,
                })
            })
            .collect()
    }

    /// The output the form has picked.
    pub fn picked_output(&self) -> Option<Port> {
        let ports = self.output_ports();
        ports.get(self.connect_from % ports.len().max(1)).copied()
    }

    /// The input the form has picked.
    pub fn picked_input(&self) -> Option<Port> {
        let ports = self.input_ports();
        ports.get(self.connect_to % ports.len().max(1)).copied()
    }

    /// Pick the next output in the form.
    pub fn next_output(&mut self) {
        self.connect_from = (self.connect_from + 1) % self.output_ports().len().max(1);
    }

    /// Pick the next input in the form.
    pub fn next_input(&mut self) {
        self.connect_to = (self.connect_to + 1) % self.input_ports().len().max(1);
    }

    /// Wire the picked output to the picked input, under the same rules as
    /// clicking them on the canvas.
    pub fn connect_picked(&mut self) -> Option<AudioRequest> {
        let (from, to) = (self.picked_output()?, self.picked_input()?);
        self.wiring = Some(from);
        self.click_input(to)
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

    fn view_of(graph: GraphSpec) -> AudioGraphView {
        AudioGraphView {
            graph,
            nodes: vec![],
        }
    }

    /// A state that has read the default graph from the daemon.
    fn loaded() -> RoutingState {
        let mut state = RoutingState::default();
        let Some(AudioRequest::Get { id }) = state.follow(true, None) else {
            panic!("the page asks for the graph when shown");
        };
        state.graph_reply(id, Ok(view_of(GraphSpec::passthrough())));
        state
    }

    /// Let the daemon save what `request` asks, and return the graph it was
    /// asked to save, with whatever the state sent in reply to the answer.
    fn saved(
        state: &mut RoutingState,
        request: Option<AudioRequest>,
    ) -> Option<(GraphSpec, Option<AudioRequest>)> {
        match request {
            Some(AudioRequest::Set { id, graph }) => {
                let next = state.graph_reply(id, Ok(view_of(graph.clone())));
                Some((graph, next))
            }
            Some(_) | None => None,
        }
    }

    /// Add a gain node (id 2) with no wires, as the page does.
    fn with_gain() -> (RoutingState, GraphSpec) {
        let mut state = loaded();
        let request = state.add_gain();
        let (graph, _) = saved(&mut state, request).unwrap();
        (state, graph)
    }

    /// Wire `from` to `to` by clicking, and let the daemon save it.
    fn wired(state: &mut RoutingState, from: u32, to: u32) {
        state.click_output(port(from));
        let request = state.click_input(port(to));
        saved(state, request).unwrap();
    }

    #[test]
    fn the_graph_is_asked_for_once_each_time_the_page_is_shown() {
        let mut state = RoutingState::default();
        let asked = [
            state.follow(true, None),
            state.follow(true, None),
            state.follow(false, None),
            state.follow(true, None),
        ]
        .map(|request| request.is_some());
        // The second showing waits for the first request's answer.
        assert_eq!(asked, [true, false, false, false]);
        state.graph_reply(1, Ok(view_of(GraphSpec::passthrough())));
        // Once answered, a take that ends makes it ask again, and nothing else.
        let steady = state.follow(true, Some((1, true)));
        let reasked = state.follow(true, Some((1, true)));
        state.graph_reply(2, Ok(view_of(GraphSpec::passthrough())));
        let after_take = state.follow(true, Some((2, true)));
        assert_eq!(
            (steady.is_some(), reasked.is_some(), after_take.is_some()),
            (true, false, true)
        );
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
        let (graph, _) = saved(&mut state, request).unwrap();
        assert_eq!(
            (graph, state.wiring, state.selection),
            (expected.clone(), None, Selection::Wire(expected.edges[1]))
        );
    }

    #[test]
    fn a_wire_that_would_loop_back_or_repeat_is_refused() {
        let (mut state, _) = with_gain();
        wired(&mut state, 0, 2);
        wired(&mut state, 2, 1);
        let wired_graph = state.graph.clone().unwrap();
        state.click_output(port(2));
        let looped = state.click_input(port(2));
        let loop_notice = state.notice.clone();
        state.click_output(port(0));
        let repeated = state.click_input(port(2));
        assert_eq!(
            (looped, loop_notice, repeated, state.graph.unwrap()),
            (None, Some(Notice::WouldLoop), None, wired_graph)
        );
    }

    #[test]
    fn removing_a_node_removes_its_wires_but_never_the_endpoints() {
        let (mut state, _) = with_gain();
        wired(&mut state, 0, 2);
        wired(&mut state, 2, 1);
        let endpoint = state.remove_node(0);
        let request = state.remove_node(2);
        let removed = saved(&mut state, request).map(|(graph, _)| graph);
        // Back to the default graph, whose own wire from input to output
        // was never touched.
        assert_eq!((endpoint, removed), (None, Some(GraphSpec::passthrough())));
    }

    #[test]
    fn edits_made_while_a_save_is_out_wait_and_go_as_one_save_after_it() {
        let mut state = loaded();
        let first = state.add_gain();
        let (second, third) = (state.nudge_gain(2, 3.), state.nudge_gain(2, 2.));
        let Some(AudioRequest::Set { id, graph }) = first else {
            panic!("the first edit is saved at once");
        };
        // A reply to a request nobody is waiting for changes nothing.
        let stale = state.graph_reply(id + 100, Err("old".to_string()));
        let follow = state.graph_reply(id, Ok(view_of(graph)));
        let mut expected = GraphSpec::passthrough();
        expected.nodes.push(plugin_graph::NodeSpec {
            id: 2,
            kind: NodeKind::Gain { db: 5. },
            x: 210.,
            y: 90.,
        });
        assert_eq!(
            (
                second,
                third,
                stale,
                follow.map(|next| match next {
                    AudioRequest::Set { graph, .. } => graph,
                    other => panic!("{other:?}"),
                })
            ),
            (None, None, None, Some(expected))
        );
    }

    #[test]
    fn a_refused_save_shows_what_the_daemon_has_and_drops_what_was_piled_on_it() {
        let mut state = loaded();
        let Some(AudioRequest::Set { id, .. }) = state.add_gain() else {
            panic!("an edit is saved with Set");
        };
        state.nudge_gain(2, 3.);
        let follow = state.graph_reply(id, Err("a plugin did not load".to_string()));
        assert_eq!(
            (follow, state.graph, state.notice),
            (
                None,
                Some(GraphSpec::passthrough()),
                Some(Notice::Daemon("a plugin did not load".to_string()))
            )
        );
    }

    #[test]
    fn a_node_id_is_not_used_again_after_its_node_is_removed() {
        let (mut state, _) = with_gain();
        let request = state.remove_node(2);
        saved(&mut state, request).unwrap();
        let request = state.add_gain();
        let (graph, _) = saved(&mut state, request).unwrap();
        assert_eq!(graph.nodes.last().map(|node| node.id), Some(3));
    }

    #[test]
    fn a_dragged_node_is_saved_where_it_was_let_go() {
        let (mut state, _) = with_gain();
        let node = state.graph.as_ref().unwrap().nodes[2].clone();
        state.begin_drag(2, (node.x + 10., node.y + 5.));
        state.drag_to((500., 300.));
        let moved = state.graph.clone().unwrap().nodes[2].clone();
        let request = state.end_drag();
        let saved_node = saved(&mut state, request).map(|(graph, _)| graph.nodes[2].clone());
        assert_eq!(
            (
                moved.x,
                moved.y,
                saved_node.map(|node| (node.x, node.y)),
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

    #[test]
    fn the_selected_wire_or_node_is_removed_and_the_canvas_scrolls_no_further_back_than_its_origin()
    {
        let (mut state, _) = with_gain();
        wired(&mut state, 0, 2);
        let wire = Edge {
            from: port(0),
            to: port(2),
        };
        state.selection = Selection::Wire(wire);
        let request = state.remove_selected();
        let without_wire = saved(&mut state, request).map(|(graph, _)| graph.edges.len());
        state.selection = Selection::Node(2);
        let request = state.remove_selected();
        let without_node = saved(&mut state, request).map(|(graph, _)| graph.nodes.len());
        state.scroll_by((30., 10.));
        state.scroll_by((-100., -4.));
        assert_eq!(
            (
                without_wire,
                without_node,
                state.remove_selected(),
                state.pan
            ),
            (Some(1), Some(2), None, (0., 6.))
        );
    }

    #[test]
    fn the_connection_form_picks_ports_and_wires_them_under_the_same_rules() {
        let (mut state, mut expected) = with_gain();
        // Outputs: the microphone's, then the gain's. Inputs: the gain's, then
        // the recognizer's.
        let outputs = state.output_ports();
        let inputs = state.input_ports();
        state.next_output();
        let from = state.picked_output();
        let to = state.picked_input();
        let request = state.connect_picked();
        expected.edges.push(Edge {
            from: port(2),
            to: port(1),
        });
        let (graph, _) = saved(&mut state, request).unwrap();
        // The same wire again is a repeat, which is not saved.
        let repeated = state.connect_picked();
        assert_eq!(
            (outputs, inputs, from, to, graph, repeated),
            (
                vec![port(0), port(2)],
                vec![port(1), port(2)],
                Some(port(2)),
                Some(port(1)),
                expected,
                None
            )
        );
    }
}
