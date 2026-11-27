//! The Audio routing page: reading the daemon's graph, wiring nodes through
//! their ports, adding plugins, dragging nodes, and the test recording. The
//! page edits a draft and asks the daemon to save each edit; the tests play
//! the daemon's part by feeding back the replies the bridge would send.

use gpui_kit::{ScrollDelta, TestAppContext, point, px};
use limen_core::{AudioGraphView, NodeStatus, PluginEntry, TestFiles};
use plugin_graph::{Edge, GraphSpec, NodeKind, NodeSpec, PluginRef, Port, Sidechain};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::bridge::{AudioRequest, BridgeCommand, BridgeEvent};

fn wire(from: (u32, u16), to: (u32, u16)) -> Edge {
    Edge {
        from: Port {
            node: from.0,
            port: from.1,
        },
        to: Port {
            node: to.0,
            port: to.1,
        },
    }
}

fn view_of(graph: GraphSpec) -> AudioGraphView {
    AudioGraphView {
        graph,
        nodes: vec![],
    }
}

/// Open the page and answer its read with the default graph.
fn open(ui: &mut Ui) {
    ui.click("routing-page");
    let sent = ui.sent();
    assert_eq!(
        sent,
        vec![BridgeCommand::Audio(AudioRequest::Get { id: 1 })]
    );
    ui.receive(BridgeEvent::AudioGraph {
        id: 1,
        result: Ok(view_of(GraphSpec::passthrough())),
    });
}

/// Scroll the page to its end, where the lists and the test recording are.
fn scroll_down(ui: &mut Ui) {
    ui.scroll(
        "routing-content",
        ScrollDelta::Pixels(point(px(0.), px(-3000.))),
    );
}

/// The graph of the one `Set` among `sent`, and its id.
fn saved(sent: Vec<BridgeCommand>) -> (u64, GraphSpec) {
    match sent.as_slice() {
        [BridgeCommand::Audio(AudioRequest::Set { id, graph })] => (*id, graph.clone()),
        other => panic!("expected one Set, got {other:?}"),
    }
}

fn gain_graph() -> GraphSpec {
    let mut graph = GraphSpec::passthrough();
    graph.nodes.push(NodeSpec {
        id: 2,
        kind: NodeKind::Gain { db: 0. },
        x: 210.,
        y: 90.,
    });
    graph
}

/// Add a gain node and take the daemon's saving of it.
fn with_gain(ui: &mut Ui) {
    ui.click("add-gain");
    let (id, graph) = saved(ui.sent());
    assert_eq!(graph, gain_graph());
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(view_of(graph)),
    });
}

#[gpui_kit::test]
fn the_page_reads_the_graph_and_shows_its_endpoints_wired(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    assert_eq!(
        (
            ui.label("node-0"),
            ui.label("node-1"),
            ui.label("wire-0"),
            ui.try_find("wire-1").is_none(),
        ),
        (
            t!("routing.input").to_string(),
            t!("routing.output").to_string(),
            format!("{} 1 → {} 1", t!("routing.input"), t!("routing.output")),
            true,
        )
    );
}

#[gpui_kit::test]
fn clicking_an_output_and_an_input_wires_them_and_saves_the_graph(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    with_gain(&mut ui);
    ui.click("port-out-0-0");
    let hint = ui.label("routing-hint");
    ui.click("port-in-2-0");
    let (id, graph) = saved(ui.sent());
    let mut expected = gain_graph();
    expected.edges.push(wire((0, 0), (2, 0)));
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(view_of(graph.clone())),
    });
    assert_eq!(
        (
            hint,
            graph,
            ui.label("wire-1"),
            ui.try_find("routing-hint").is_none()
        ),
        (
            t!("routing.wiring").to_string(),
            expected,
            format!(
                "{} 1 → {} 1",
                t!("routing.input"),
                t!("routing.gain").to_string() + " +0 dB"
            ),
            true,
        )
    );
}

#[gpui_kit::test]
fn a_wire_that_would_loop_is_refused_without_asking_the_daemon(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    with_gain(&mut ui);
    for (out, input) in [
        ("port-out-0-0", "port-in-2-0"),
        ("port-out-2-0", "port-in-1-0"),
    ] {
        ui.click(out);
        ui.click(input);
        let (id, graph) = saved(ui.sent());
        ui.receive(BridgeEvent::AudioGraph {
            id,
            result: Ok(view_of(graph)),
        });
    }
    ui.click("port-out-2-0");
    ui.click("port-in-2-0");
    assert_eq!(
        (ui.label("routing-notice"), ui.sent()),
        (t!("routing.loop").to_string(), vec![])
    );
}

#[gpui_kit::test]
fn a_refused_save_shows_why_and_goes_back_to_what_the_daemon_has(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    ui.click("add-gain");
    let (id, _) = saved(ui.sent());
    let drawn = ui.try_find("node-2").is_some();
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Err("a recording is using the plugins".to_string()),
    });
    assert_eq!(
        (
            drawn,
            ui.label("routing-notice"),
            ui.try_find("node-2").is_none()
        ),
        (true, "a recording is using the plugins".to_string(), true)
    );
}

#[gpui_kit::test]
fn dragging_a_node_moves_it_and_saves_where_it_was_let_go(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    with_gain(&mut ui);
    let start = ui.find("node-2").bounds().center();
    let before = ui.read(|view, _| view.routing.graph.clone().unwrap().nodes[2].clone());
    ui.drag(start, point(start.x + px(80.), start.y + px(40.)));
    let (_, graph) = saved(ui.sent());
    let after = graph.nodes[2].clone();
    assert_eq!(
        ((after.x - before.x).round(), (after.y - before.y).round()),
        (80., 40.)
    );
}

fn plugin(name: &str, category: &str) -> PluginEntry {
    PluginEntry {
        path: format!("/Library/Audio/Plug-Ins/VST3/{name}.vst3"),
        class_id: "00ff".to_string(),
        name: name.to_string(),
        vendor: "Vendor".to_string(),
        category: category.to_string(),
        effect: category.starts_with("Fx"),
    }
}

#[gpui_kit::test]
fn a_plugin_is_added_from_the_list_of_effects_and_its_controls_ask_the_daemon(
    cx: &mut TestAppContext,
) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    ui.click("add-plugin");
    let asked = ui.sent();
    ui.receive(BridgeEvent::AudioScanned {
        id: 2,
        result: Ok(vec![
            plugin("Synth", "Instrument|Synth"),
            plugin("Gate", "Fx|Dynamics"),
        ]),
    });
    // Only effects are offered; the synth's row is not there.
    let rows = (
        ui.try_find("plugin-0").is_some(),
        ui.try_find("plugin-1").is_some(),
    );
    ui.click("plugin-1");
    let (id, graph) = saved(ui.sent());
    let gate = PluginRef {
        path: "/Library/Audio/Plug-Ins/VST3/Gate.vst3".to_string(),
        class_id: "00ff".to_string(),
        name: "Gate".to_string(),
        enabled: true,
        sidechain: Sidechain::Off,
    };
    let mut expected = GraphSpec::passthrough();
    expected.nodes.push(NodeSpec {
        id: 2,
        kind: NodeKind::Plugin(gate.clone()),
        x: 210.,
        y: 90.,
    });
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(AudioGraphView {
            graph: graph.clone(),
            nodes: vec![NodeStatus {
                node: 2,
                main_inputs: 1,
                sidechain_inputs: 0,
                outputs: 1,
                problem: Some("Gate: did not load".to_string()),
            }],
        }),
    });
    scroll_down(&mut ui);
    ui.click("edit-node-2");
    let edit = ui.sent();
    ui.click("toggle-node-2");
    let (_, toggled) = saved(ui.sent());
    let mut off = expected.clone();
    off.nodes[2].kind = NodeKind::Plugin(PluginRef {
        enabled: false,
        ..gate
    });
    assert_eq!(
        (asked, rows, graph, edit, toggled),
        (
            vec![BridgeCommand::Audio(AudioRequest::Scan { id: 2 })],
            (false, true),
            expected,
            vec![BridgeCommand::Audio(AudioRequest::OpenEditor { node: 2 })],
            off,
        )
    );
}

#[gpui_kit::test]
fn a_test_recording_is_asked_for_and_offers_both_versions_to_play(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    scroll_down(&mut ui);
    ui.click("record-test");
    let asked = ui.sent();
    let busy = ui.find("record-test").disabled();
    let before = ui.try_find("play-processed").is_some();
    ui.receive(BridgeEvent::AudioTest {
        id: 2,
        result: Ok(TestFiles {
            dry: "/tmp/dry.wav".to_string(),
            wet: "/tmp/wet.wav".to_string(),
        }),
    });
    assert_eq!(
        (
            asked,
            busy,
            before,
            ui.try_find("play-original").is_some(),
            ui.try_find("play-processed").is_some(),
            ui.find("record-test").disabled(),
        ),
        (
            vec![BridgeCommand::Audio(AudioRequest::Test {
                id: 2,
                seconds: 5
            })],
            Some(true),
            false,
            true,
            true,
            None,
        )
    );
}

#[gpui_kit::test]
fn the_connection_form_wires_the_picked_ports_without_the_canvas(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    with_gain(&mut ui);
    scroll_down(&mut ui);
    // Outputs: the microphone's then the gain's; inputs: the recognizer's
    // then the gain's. Pick the gain's output for the recognizer.
    ui.click("connect-from");
    let from = ui.label("connect-from");
    ui.click("connect-add");
    let (_, graph) = saved(ui.sent());
    let mut expected = gain_graph();
    expected.edges.push(wire((2, 0), (1, 0)));
    assert_eq!(
        (from.contains(&t!("routing.gain").to_string()), graph),
        (true, expected)
    );
}

#[gpui_kit::test]
fn a_selected_wire_and_a_selected_node_can_be_removed_from_the_toolbar(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    with_gain(&mut ui);
    // A click on empty canvas selects nothing, so there is nothing to remove.
    ui.click_at("routing-canvas", point(px(10.), px(300.)));
    let nothing_selected = ui.find("remove-selected").disabled();
    // The default wire runs level from the input's port to the output's, and
    // is selected by clicking on it, at its middle.
    ui.click_at("routing-canvas", point(px(294.), px(44.)));
    ui.click("remove-selected");
    let (id, graph) = saved(ui.sent());
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(view_of(graph.clone())),
    });
    ui.click("node-2");
    ui.click("remove-selected");
    let (_, without_node) = saved(ui.sent());
    let mut no_wires = gain_graph();
    no_wires.edges.clear();
    assert_eq!(
        (nothing_selected, graph, without_node),
        (Some(true), no_wires, {
            let mut bare = GraphSpec::passthrough();
            bare.edges.clear();
            bare
        })
    );
}

#[gpui_kit::test]
fn the_sidechain_of_a_plugin_is_switched_on_and_saved(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    ui.click("add-plugin");
    ui.sent();
    ui.receive(BridgeEvent::AudioScanned {
        id: 2,
        result: Ok(vec![plugin("Compressor", "Fx|Dynamics")]),
    });
    ui.click("plugin-0");
    let (id, graph) = saved(ui.sent());
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(view_of(graph)),
    });
    scroll_down(&mut ui);
    ui.click("sidechain-node-2");
    let (_, graph) = saved(ui.sent());
    let sidechain = match &graph.nodes[2].kind {
        NodeKind::Plugin(plugin) => plugin.sidechain,
        other => panic!("{other:?}"),
    };
    assert_eq!(sidechain, Sidechain::On);
}

#[gpui_kit::test]
fn only_one_save_is_out_at_a_time_and_what_was_edited_meanwhile_follows_it(
    cx: &mut TestAppContext,
) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    ui.click("add-gain");
    let (id, graph) = saved(ui.sent());
    // Edited before the daemon has answered: nothing is sent yet.
    scroll_down(&mut ui);
    ui.click("gain-up-2");
    let waiting = ui.sent();
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(view_of(graph)),
    });
    let (_, follow) = saved(ui.sent());
    assert_eq!(
        (waiting, follow.nodes[2].kind.clone()),
        (vec![], NodeKind::Gain { db: 1. })
    );
}

#[gpui_kit::test]
fn the_canvas_scrolls_so_a_node_out_of_view_can_be_reached(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    let before = ui.find("node-1").bounds().left();
    ui.scroll(
        "routing-canvas",
        ScrollDelta::Pixels(point(px(-120.), px(0.))),
    );
    let after = ui.find("node-1").bounds().left();
    assert_eq!((before - after).round(), px(120.).round());
}

#[gpui_kit::test]
fn the_microphone_is_wired_to_a_plugins_sidechain_port_and_a_double_click_opens_its_editor(
    cx: &mut TestAppContext,
) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open(&mut ui);
    ui.click("add-plugin");
    ui.sent();
    ui.receive(BridgeEvent::AudioScanned {
        id: 2,
        result: Ok(vec![plugin("Compressor", "Fx|Dynamics")]),
    });
    ui.click("plugin-0");
    let (id, graph) = saved(ui.sent());
    // The daemon says the plugin has a main input and a side-chain input.
    ui.receive(BridgeEvent::AudioGraph {
        id,
        result: Ok(AudioGraphView {
            graph,
            nodes: vec![NodeStatus {
                node: 2,
                main_inputs: 1,
                sidechain_inputs: 1,
                outputs: 1,
                problem: None,
            }],
        }),
    });
    ui.click("port-out-0-0");
    ui.click("port-in-2-1");
    let (_, wired) = saved(ui.sent());
    ui.double_click("node-2");
    assert_eq!(
        (wired.edges.last().copied(), ui.sent()),
        (
            Some(wire((0, 0), (2, 1))),
            vec![BridgeCommand::Audio(AudioRequest::OpenEditor { node: 2 })]
        )
    );
}
