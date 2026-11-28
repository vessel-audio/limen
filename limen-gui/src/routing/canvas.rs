//! The canvas: nodes you drag, ports you click to wire them, and the wires.

use gpui_kit::base::{ObservedElement, TestSupportExt};
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;
use plugin_graph::{NodeKind, NodeSpec, Port};

use super::geometry::{
    HEADER, NODE_WIDTH, ROW, Side, curve_points, node_height, port_point, slots, wire_curve,
};
use super::state::Selection;
use crate::app::AppView;

/// How tall the canvas is.
const CANVAS_HEIGHT: f32 = 380.;
/// Diameter of a port.
const PORT: f32 = 12.;

/// A node being dragged; it draws nothing under the pointer.
#[derive(Clone)]
struct NodeDrag;

impl Render for NodeDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl AppView {
    /// A window position as a place on the canvas.
    fn routing_point(&self, position: Point<Pixels>) -> (f32, f32) {
        let origin = self.routing_origin.get();
        let pan = self.routing.pan;
        (
            f32::from(position.x - origin.x) + pan.0,
            f32::from(position.y - origin.y) + pan.1,
        )
    }

    fn finish_node_drag(&mut self, cx: &mut Context<Self>) {
        let request = self.routing.end_drag();
        self.send_audio(request);
        cx.notify();
    }
}

/// What a node is called on the canvas, and whether it is switched off.
pub(super) fn title(node: &NodeSpec) -> (String, bool) {
    match &node.kind {
        NodeKind::Input => (rust_i18n::t!("routing.input").to_string(), false),
        NodeKind::Output => (rust_i18n::t!("routing.output").to_string(), false),
        NodeKind::Gain { db } => (
            format!("{} {db:+.0} dB", rust_i18n::t!("routing.gain")),
            false,
        ),
        NodeKind::Plugin(plugin) => (plugin.name.clone(), !plugin.enabled),
    }
}

pub(super) fn canvas_view(
    view: &AppView,
    cx: &mut Context<AppView>,
) -> ObservedElement<Stateful<Div>> {
    let theme = cx.theme();
    let (border, line, chosen_line, backdrop) = (
        theme.border,
        theme.muted_foreground,
        theme.primary,
        theme.muted.opacity(0.3),
    );
    let routing = &view.routing;
    let pan = routing.pan;
    let graph = routing
        .graph
        .clone()
        .unwrap_or_else(|| plugin_graph::GraphSpec {
            nodes: vec![],
            edges: vec![],
        });
    let node = |id| graph.nodes.iter().find(|node| node.id == id);

    // Wires, as the points along each, in canvas coordinates.
    let wires = graph
        .edges
        .iter()
        .filter_map(|edge| {
            let from = port_point(
                node(edge.from.node)?,
                Side::Output,
                usize::from(edge.from.port),
            );
            let to = port_point(node(edge.to.node)?, Side::Input, usize::from(edge.to.port));
            let shift = |(x, y): (f32, f32)| (x - pan.0, y - pan.1);
            Some((
                curve_points(wire_curve(shift(from), shift(to))),
                routing.selection == Selection::Wire(*edge),
            ))
        })
        .collect::<Vec<_>>();
    let origin = view.routing_origin.clone();
    let layer = canvas(
        move |bounds, _, _| origin.set(bounds.origin),
        move |bounds, _, window, _| {
            for (points, chosen) in &wires {
                let mut path = PathBuilder::stroke(px(match chosen {
                    true => 3.,
                    false => 2.,
                }));
                for (index, (x, y)) in points.iter().enumerate() {
                    let at = point(bounds.origin.x + px(*x), bounds.origin.y + px(*y));
                    match index {
                        0 => path.move_to(at),
                        _ => path.line_to(at),
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(
                        path,
                        match chosen {
                            true => chosen_line,
                            false => line,
                        },
                    );
                }
            }
        },
    )
    .absolute()
    .size_full();

    let mut cards = Vec::new();
    let mut ports = Vec::new();
    for spec in &graph.nodes {
        cards.push(node_card(view, spec, cx));
        ports.extend(port_dots(view, spec, cx));
    }

    div()
        .id("routing-canvas")
        .test_support()
        .accessibility_id("routing-canvas")
        .relative()
        .h(px(CANVAS_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .bg(backdrop)
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                let at = this.routing_point(event.position);
                this.routing.click_canvas(at);
                cx.notify();
            }),
        )
        .on_drop(cx.listener(|this, _: &NodeDrag, _, cx| this.finish_node_drag(cx)))
        // The wheel and the trackpad scroll the canvas, so that a node moved
        // out of view can be brought back.
        .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
            let delta = event.delta.pixel_delta(px(20.));
            this.routing
                .scroll_by((-f32::from(delta.x), -f32::from(delta.y)));
            cx.notify();
        }))
        .child(layer)
        .children(cards)
        .children(ports)
}

fn node_card(view: &AppView, spec: &NodeSpec, cx: &mut Context<AppView>) -> Stateful<Div> {
    let theme = cx.theme();
    let routing = &view.routing;
    let status = routing.status(spec.id);
    let size = slots(spec, status);
    let (name, off) = title(spec);
    let id = spec.id;
    let chosen = routing.selection == Selection::Node(id);
    let failed = status.is_some_and(|status| status.problem.is_some());
    let (border, text, muted) = (
        match (chosen, failed) {
            (true, _) => theme.primary,
            (false, true) => theme.danger,
            (false, false) => theme.border,
        },
        theme.foreground,
        theme.muted_foreground,
    );
    let opens_editor = matches!(spec.kind, NodeKind::Plugin(_));
    let key = SharedString::from(format!("node-{id}"));
    let header = div()
        .id(key.clone())
        .test_support()
        .accessibility_id(key)
        .role(Role::Group)
        .aria_label(name.clone())
        .h(px(HEADER))
        .px_3()
        .flex()
        .items_center()
        .cursor_grab()
        .text_sm()
        .font_medium()
        .whitespace_nowrap()
        .overflow_hidden()
        .child(name)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let at = this.routing_point(event.position);
                this.routing.begin_drag(id, at);
                cx.notify();
            }),
        )
        // Double-clicking a plugin's name opens its own editor.
        .on_click(cx.listener(move |this, event: &ClickEvent, _, _| {
            if event.click_count() == 2 && opens_editor {
                this.send_audio(Some(crate::bridge::AudioRequest::OpenEditor { node: id }));
            }
        }))
        .on_drag(NodeDrag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_drag_move(cx.listener(|this, event: &DragMoveEvent<NodeDrag>, _, cx| {
            let at = this.routing_point(event.event.position);
            this.routing.drag_to(at);
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_node_drag(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_node_drag(cx)),
        );
    let main_inputs = status.map_or(size.inputs, |status| status.main_inputs);
    let rows = (0..size.inputs.max(size.outputs).max(1)).map(|index| {
        let input = (index < size.inputs).then(|| match index < main_inputs {
            true => format!("{} {}", rust_i18n::t!("routing.in"), index + 1),
            false => rust_i18n::t!("routing.key").to_string(),
        });
        let output = (index < size.outputs)
            .then(|| format!("{} {}", rust_i18n::t!("routing.out"), index + 1));
        div()
            .h(px(ROW))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .text_xs()
            .text_color(muted)
            .child(input.unwrap_or_default())
            .child(output.unwrap_or_default())
    });
    div()
        .id(SharedString::from(format!("node-card-{id}")))
        .absolute()
        .left(px(spec.x - view.routing.pan.0))
        .top(px(spec.y - view.routing.pan.1))
        .w(px(NODE_WIDTH))
        .h(px(node_height(size)))
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(theme.background)
        .text_color(text)
        .opacity(match off {
            true => 0.55,
            false => 1.,
        })
        .shadow_sm()
        .child(header)
        .children(rows)
}

/// The dots on a node's edges that wires start and end at.
fn port_dots(
    view: &AppView,
    spec: &NodeSpec,
    cx: &mut Context<AppView>,
) -> Vec<ObservedElement<Stateful<Div>>> {
    let routing = &view.routing;
    let status = routing.status(spec.id);
    let size = slots(spec, status);
    let (name, _) = title(spec);
    let main_inputs = status.map_or(size.inputs, |status| status.main_inputs);
    let theme = cx.theme();
    let (border, idle, active) = (theme.border, theme.background, theme.primary);
    let inputs = (0..size.inputs).map(|index| (Side::Input, index));
    let outputs = (0..size.outputs).map(|index| (Side::Output, index));
    inputs
        .chain(outputs)
        .map(|(side, index)| {
            let (x, y) = port_point(spec, side, index);
            let (x, y) = (x - routing.pan.0, y - routing.pan.1);
            let port = Port {
                node: spec.id,
                port: index as u16,
            };
            let (key, what) = match side {
                Side::Input => (
                    "in",
                    match index < main_inputs {
                        true => rust_i18n::t!("routing.port_in"),
                        false => rust_i18n::t!("routing.port_key"),
                    },
                ),
                Side::Output => ("out", rust_i18n::t!("routing.port_out")),
            };
            let id = SharedString::from(format!("port-{key}-{}-{index}", spec.id));
            let waiting = side == Side::Output && routing.wiring == Some(port);
            div()
                .id(id.clone())
                .test_support()
                .accessibility_id(id)
                .role(Role::Button)
                .aria_label(format!("{name}: {what} {}", index + 1))
                .absolute()
                .left(px(x - PORT / 2.))
                .top(px(y - PORT / 2.))
                .size(px(PORT))
                .rounded_full()
                .border_2()
                .border_color(match waiting {
                    true => active,
                    false => border,
                })
                .bg(match waiting {
                    true => active,
                    false => idle,
                })
                .cursor_pointer()
                .hover(move |style| style.border_color(active))
                // The canvas clears a half-made wire when it is clicked; a
                // port is not the canvas.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let request = match side {
                        Side::Output => {
                            this.routing.click_output(port);
                            None
                        }
                        Side::Input => this.routing.click_input(port),
                    };
                    this.send_audio(request);
                    cx.notify();
                }))
        })
        .collect()
}
