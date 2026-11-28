//! Where the routing canvas draws nodes, ports and wires. Everything follows
//! from the positions saved in the graph and the channels each node has, so
//! wires end exactly at the ports they belong to without anything measured.

use limen_core::NodeStatus;
use plugin_graph::{NodeKind, NodeSpec};

pub const NODE_WIDTH: f32 = 188.;
pub const HEADER: f32 = 32.;
pub const ROW: f32 = 24.;
const FOOTER: f32 = 8.;
/// Segments a wire is drawn and hit-tested as.
const SEGMENTS: usize = 24;

/// Which edge of a node a port sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Input,
    Output,
}

/// How many channels a node is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub inputs: usize,
    pub outputs: usize,
}

/// The channels of `node`: those of its loaded plugin when the daemon has
/// said, else what its kind usually has.
pub fn slots(node: &NodeSpec, status: Option<&NodeStatus>) -> Slots {
    match (&node.kind, status) {
        (_, Some(status)) => Slots {
            inputs: status.main_inputs + status.sidechain_inputs,
            outputs: status.outputs,
        },
        (NodeKind::Input, None) => Slots {
            inputs: 0,
            outputs: 1,
        },
        (NodeKind::Output, None) => Slots {
            inputs: 1,
            outputs: 0,
        },
        (NodeKind::Gain { .. } | NodeKind::Plugin(_), None) => Slots {
            inputs: 1,
            outputs: 1,
        },
    }
}

pub fn node_height(slots: Slots) -> f32 {
    HEADER + ROW * slots.inputs.max(slots.outputs).max(1) as f32 + FOOTER
}

/// The centre of port `index` on `side` of `node`.
pub fn port_point(node: &NodeSpec, side: Side, index: usize) -> (f32, f32) {
    let x = match side {
        Side::Input => node.x,
        Side::Output => node.x + NODE_WIDTH,
    };
    (x, node.y + HEADER + ROW * (index as f32 + 0.5))
}

/// A wire's cubic Bézier, leaving and entering horizontally.
pub fn wire_curve(from: (f32, f32), to: (f32, f32)) -> [(f32, f32); 4] {
    let reach = ((to.0 - from.0).abs() * 0.5).max(40.);
    [from, (from.0 + reach, from.1), (to.0 - reach, to.1), to]
}

/// Points along a wire, from its start to its end.
pub fn curve_points(curve: [(f32, f32); 4]) -> Vec<(f32, f32)> {
    let [a, b, c, d] = curve;
    (0..=SEGMENTS)
        .map(|step| {
            let t = step as f32 / SEGMENTS as f32;
            let u = 1. - t;
            let blend = |a: f32, b: f32, c: f32, d: f32| {
                u * u * u * a + 3. * u * u * t * b + 3. * u * t * t * c + t * t * t * d
            };
            (blend(a.0, b.0, c.0, d.0), blend(a.1, b.1, c.1, d.1))
        })
        .collect()
}

/// How far `point` is from the nearest part of the wire.
pub fn distance_to_wire(curve: [(f32, f32); 4], point: (f32, f32)) -> f32 {
    curve_points(curve)
        .windows(2)
        .map(|segment| distance_to_segment(segment[0], segment[1], point))
        .fold(f32::INFINITY, f32::min)
}

fn distance_to_segment(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = match length == 0. {
        true => 0.,
        false => (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0., 1.),
    };
    let nearest = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - nearest.0).powi(2) + (p.1 - nearest.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use plugin_graph::{NodeKind, NodeSpec};

    use super::{
        HEADER, NODE_WIDTH, ROW, Side, curve_points, distance_to_wire, port_point, wire_curve,
    };

    fn node_at(x: f32, y: f32) -> NodeSpec {
        NodeSpec {
            id: 2,
            kind: NodeKind::Gain { db: 0. },
            x,
            y,
        }
    }

    #[test]
    fn ports_sit_on_the_node_edges_one_row_apart() {
        let node = node_at(100., 50.);
        assert_eq!(
            [
                port_point(&node, Side::Input, 0),
                port_point(&node, Side::Output, 0),
                port_point(&node, Side::Input, 2),
            ],
            [
                (100., 50. + HEADER + ROW * 0.5),
                (100. + NODE_WIDTH, 50. + HEADER + ROW * 0.5),
                (100., 50. + HEADER + ROW * 2.5),
            ]
        );
    }

    #[test]
    fn a_wire_runs_from_its_start_to_its_end_and_is_hit_near_it() {
        let curve = wire_curve((0., 0.), (300., 100.));
        let points = curve_points(curve);
        let middle = points[points.len() / 2];
        assert_eq!(
            (
                points.first().copied(),
                points.last().copied(),
                distance_to_wire(curve, middle) < 0.01,
                distance_to_wire(curve, (150., 400.)) > 100.,
            ),
            (Some((0., 0.)), Some((300., 100.)), true, true)
        );
    }
}
