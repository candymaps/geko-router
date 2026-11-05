//! The turn-aware graph: the nodes are the directed edges of the road graph, and an arc is a turn
//! from one edge to the next. A turn is there unless a restriction forbids it or it is a U-turn
//! (allowed only at a dead end), and it costs what its angle and its traffic light say. This is the
//! graph that the routing algorithms run on, so that turns are exact instead of approximated.
//!
//! The weight of an arc is the duration of the edge it leaves plus the cost of the turn, in
//! deciseconds: the duration of a path is its arcs plus that of its last edge. Integers, so a
//! sum is exact and the same on every machine.

use std::collections::HashMap;

use crate::graph::{Edge, Graph, Restriction, RestrictionKind};

/// What a turn costs, in seconds. Provisional: calibrated against the golden routes before the
/// first release of the module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnProfile {
    pub u_turn_s: f32,
    /// A left turn crosses the opposite lane (driving on the right).
    pub left_s: f32,
    pub right_s: f32,
    pub signal_s: f32,
    /// A turn up to this angle, in degrees, is going straight on.
    pub straight_deg: f32,
}

impl Default for TurnProfile {
    fn default() -> Self {
        Self { u_turn_s: 20.0, left_s: 6.0, right_s: 3.0, signal_s: 4.0, straight_deg: 25.0 }
    }
}

impl TurnProfile {
    /// `angle_deg` is positive to the right and negative to the left.
    pub fn cost_s(&self, angle_deg: f32, u_turn: bool, signal: bool) -> f32 {
        let turn = if u_turn {
            self.u_turn_s
        } else {
            let a = angle_deg.abs();
            if a <= self.straight_deg {
                0.0
            } else {
                let side = if angle_deg > 0.0 { self.right_s } else { self.left_s };
                side * ((a - self.straight_deg) / (90.0 - self.straight_deg)).min(1.3)
            }
        };
        turn + if signal { self.signal_s } else { 0.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arc {
    /// The edge of the road graph it goes to.
    pub head: u32,
    pub weight_ds: u32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TurnGraph {
    /// The duration of each edge of the road graph, in deciseconds.
    pub edge_ds: Vec<u32>,
    /// `arcs[first_arc[e]..first_arc[e + 1]]` leave the edge `e`.
    pub first_arc: Vec<u32>,
    pub arcs: Vec<Arc>,
}

impl TurnGraph {
    pub fn edges(&self) -> usize {
        self.edge_ds.len()
    }

    pub fn arcs_from(&self, edge: u32) -> &[Arc] {
        &self.arcs[self.first_arc[edge as usize] as usize..self.first_arc[edge as usize + 1] as usize]
    }
}

pub fn to_ds(seconds: f32) -> u32 {
    (seconds * 10.0).round().max(1.0) as u32
}

/// 0° north, clockwise; planar, which is exact enough over the metres of a street.
fn bearing(from: [f64; 2], to: [f64; 2]) -> f32 {
    let dy = to[0] - from[0];
    let dx = (to[1] - from[1]) * ((from[0] + to[0]) / 2.0).to_radians().cos();
    dx.atan2(dy).to_degrees() as f32
}

/// The bearings with which an edge leaves its first node and arrives at its last, from its first
/// and last two distinct points.
pub(crate) fn bearings(graph: &Graph, edge: &Edge) -> (f32, f32) {
    let points = &graph.shapes[edge.shape_start as usize..(edge.shape_start + edge.shape_len) as usize];
    let at = |i: usize| if edge.reversed { points[points.len() - 1 - i] } else { points[i] };
    let n = points.len();
    let start = (1..n).map(at).find(|p| *p != at(0)).map_or(0.0, |p| bearing(at(0), p));
    let end = (0..n - 1).rev().map(at).find(|p| *p != at(n - 1)).map_or(0.0, |p| bearing(p, at(n - 1)));
    (start, end)
}

pub(crate) fn turn_angle(arriving: f32, leaving: f32) -> f32 {
    let mut a = leaving - arriving;
    while a > 180.0 {
        a -= 360.0;
    }
    while a <= -180.0 {
        a += 360.0;
    }
    a
}

pub fn build(graph: &Graph, profile: &TurnProfile) -> TurnGraph {
    let nodes = graph.nodes.len();
    let mut out_first = vec![0u32; nodes + 1];
    for e in &graph.edges {
        out_first[e.from as usize + 1] += 1;
    }
    for i in 0..nodes {
        out_first[i + 1] += out_first[i];
    }
    let mut out = vec![0u32; graph.edges.len()];
    let mut fill = out_first.clone();
    for (i, e) in graph.edges.iter().enumerate() {
        out[fill[e.from as usize] as usize] = i as u32;
        fill[e.from as usize] += 1;
    }

    let bearings: Vec<(f32, f32)> = graph.edges.iter().map(|e| bearings(graph, e)).collect();
    let mut restrictions: HashMap<(u32, i64), Vec<&Restriction>> = HashMap::new();
    for r in &graph.restrictions {
        restrictions.entry((r.via, r.from_way)).or_default().push(r);
    }

    let mut result = TurnGraph { edge_ds: graph.edges.iter().map(|e| to_ds(e.seconds)).collect(), first_arc: Vec::with_capacity(graph.edges.len() + 1), arcs: Vec::new() };
    for (i, e1) in graph.edges.iter().enumerate() {
        result.first_arc.push(result.arcs.len() as u32);
        let via = e1.to;
        let outgoing = &out[out_first[via as usize] as usize..out_first[via as usize + 1] as usize];
        let rules: &[&Restriction] = restrictions.get(&(via, e1.way)).map_or(&[], Vec::as_slice);
        // A restriction from a way to itself is about turning back, not about going on along it.
        let applies = |r: &Restriction, e2: &Edge| r.to_way == e2.way && (r.from_way != r.to_way || e2.to == e1.from);
        let mandatory: Vec<&&Restriction> = rules.iter().filter(|r| r.kind == RestrictionKind::Mandatory).collect();
        for &j in outgoing {
            let e2 = &graph.edges[j as usize];
            let u_turn = e2.way == e1.way && e2.to == e1.from && e2.shape_start == e1.shape_start && e2.reversed != e1.reversed;
            if u_turn && outgoing.len() > 1 {
                continue;
            }
            if rules.iter().any(|r| r.kind == RestrictionKind::Prohibited && applies(r, e2)) {
                continue;
            }
            if !mandatory.is_empty() && !mandatory.iter().any(|r| applies(r, e2)) {
                continue;
            }
            let angle = turn_angle(bearings[i].1, bearings[j as usize].0);
            let cost = profile.cost_s(angle, u_turn, graph.nodes[via as usize].signal);
            result.arcs.push(Arc { head: j, weight_ds: result.edge_ds[i] + to_ds_or_zero(cost) });
        }
    }
    result.first_arc.push(result.arcs.len() as u32);
    result
}

/// A cost of zero stays zero: `to_ds` is for durations, which are never nothing.
fn to_ds_or_zero(seconds: f32) -> u32 {
    (seconds * 10.0).round().max(0.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Builder;
    use crate::osm::{Element, Member, MemberKind, Tags};
    use crate::profile::Car;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn node(id: i64, lat: f64, lon: f64, signal: bool) -> Element {
        Element::Node { id, lat, lon, tags: if signal { tags(&[("highway", "traffic_signals")]) } else { Vec::new() } }
    }

    fn way(id: i64, refs: &[i64]) -> Element {
        Element::Way { id, refs: refs.to_vec(), tags: tags(&[("highway", "residential")]) }
    }

    fn restriction(kind: &str, from: i64, via: i64, to: i64) -> Element {
        Element::Relation {
            id: 1000 + from * 10 + to,
            members: vec![
                Member { kind: MemberKind::Way, id: from, role: "from".into() },
                Member { kind: MemberKind::Node, id: via, role: "via".into() },
                Member { kind: MemberKind::Way, id: to, role: "to".into() },
            ],
            tags: tags(&[("type", "restriction"), ("restriction", kind)]),
        }
    }

    /// A crossing at node 5 (the origin) of an east-west and a north-south street, 111 m long each
    /// side, with a way for each arm so that restrictions name one: 10 is the west arm, 11 the east,
    /// 12 the south and 13 the north. The nodes at the ends of the arms are 1 (W), 2 (E), 4 (S), 3 (N).
    fn cross(signal: bool, extra: Vec<Element>) -> (Graph, TurnGraph) {
        let mut elements = vec![
            node(1, 0.0, -0.001, false),
            node(2, 0.0, 0.001, false),
            node(3, 0.001, 0.0, false),
            node(4, -0.001, 0.0, false),
            node(5, 0.0, 0.0, signal),
            way(10, &[1, 5]),
            way(11, &[5, 2]),
            way(12, &[4, 5]),
            way(13, &[5, 3]),
        ];
        elements.extend(extra);
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        let graph = builder.finish();
        let turns = build(&graph, &TurnProfile::default());
        (graph, turns)
    }

    fn edge(graph: &Graph, from: i64, to: i64) -> u32 {
        graph.edges.iter().position(|e| graph.nodes[e.from as usize].osm_id == from && graph.nodes[e.to as usize].osm_id == to).unwrap() as u32
    }

    fn heads(graph: &Graph, turns: &TurnGraph, from: i64, to: i64) -> Vec<(i64, i64)> {
        let mut list: Vec<(i64, i64)> = turns
            .arcs_from(edge(graph, from, to))
            .iter()
            .map(|a| {
                let e = &graph.edges[a.head as usize];
                (graph.nodes[e.from as usize].osm_id, graph.nodes[e.to as usize].osm_id)
            })
            .collect();
        list.sort();
        list
    }

    #[test]
    fn a_crossing_allows_every_turn_but_the_u_turn() {
        let (graph, turns) = cross(false, vec![]);
        assert_eq!(graph.edges.len(), 8);
        // From the west: on to the east, left to the north, right to the south; not back west.
        assert_eq!(heads(&graph, &turns, 1, 5), vec![(5, 2), (5, 3), (5, 4)]);
        // Each of the four arms is a dead end: there the U-turn is the only way out.
        assert_eq!(heads(&graph, &turns, 5, 2), vec![(2, 5)]);
        // 4 approaches with 3 turns each, and 4 dead ends.
        assert_eq!(turns.arcs.len(), 16);
        assert_eq!(turns.first_arc.len(), 9);
    }

    #[test]
    fn what_a_turn_costs() {
        let (graph, turns) = cross(false, vec![]);
        let weight = |head: (i64, i64)| turns.arcs_from(edge(&graph, 1, 5)).iter().find(|a| a.head == edge(&graph, head.0, head.1)).unwrap().weight_ds;
        let edge_ds = turns.edge_ds[edge(&graph, 1, 5) as usize];
        // 111 m at 36 km/h (10 m/s) is 11.1 s.
        assert!((edge_ds as i32 - 111).abs() <= 1, "{edge_ds}");
        assert_eq!(weight((5, 2)), edge_ds);
        assert_eq!(weight((5, 4)), edge_ds + 30);
        assert_eq!(weight((5, 3)), edge_ds + 60);
        // The U-turn at the end of the arm.
        let back = turns.arcs_from(edge(&graph, 5, 2))[0].weight_ds;
        assert_eq!(back, turns.edge_ds[edge(&graph, 5, 2) as usize] + 200);
    }

    #[test]
    fn a_traffic_light_costs_every_turn_through_it() {
        let (graph, turns) = cross(true, vec![]);
        let straight = turns.arcs_from(edge(&graph, 1, 5)).iter().find(|a| a.head == edge(&graph, 5, 2)).unwrap().weight_ds;
        assert_eq!(straight, turns.edge_ds[edge(&graph, 1, 5) as usize] + 40);
        // A dead end has none.
        assert_eq!(turns.arcs_from(edge(&graph, 5, 2))[0].weight_ds, turns.edge_ds[edge(&graph, 5, 2) as usize] + 200);
    }

    #[test]
    fn a_prohibited_turn_has_no_arc_and_only_that_approach_loses_it() {
        let (graph, turns) = cross(false, vec![restriction("no_left_turn", 10, 5, 13)]);
        assert_eq!(heads(&graph, &turns, 1, 5), vec![(5, 2), (5, 4)]);
        // From the east, the same street to the north is a right turn: not covered.
        assert_eq!(heads(&graph, &turns, 2, 5), vec![(5, 1), (5, 3), (5, 4)]);
    }

    #[test]
    fn a_mandatory_turn_leaves_only_that_one() {
        let (graph, turns) = cross(false, vec![restriction("only_straight_on", 10, 5, 11)]);
        assert_eq!(heads(&graph, &turns, 1, 5), vec![(5, 2)]);
        assert_eq!(heads(&graph, &turns, 2, 5).len(), 3);
    }

    #[test]
    fn a_restriction_through_an_unknown_node_is_ignored() {
        let (graph, turns) = cross(false, vec![restriction("no_left_turn", 10, 99, 13)]);
        assert_eq!(heads(&graph, &turns, 1, 5).len(), 3);
        assert_eq!(turns.arcs.len(), 16);
    }

    #[test]
    fn a_no_u_turn_closes_a_dead_end() {
        let (graph, turns) = cross(false, vec![restriction("no_u_turn", 11, 2, 11)]);
        assert!(turns.arcs_from(edge(&graph, 5, 2)).is_empty());
        assert_eq!(turns.arcs.len(), 15);
    }

    #[test]
    fn the_angle_cost_curve() {
        let p = TurnProfile::default();
        assert_eq!(p.cost_s(0.0, false, false), 0.0);
        assert_eq!(p.cost_s(25.0, false, false), 0.0);
        assert_eq!(p.cost_s(90.0, false, false), 3.0);
        assert_eq!(p.cost_s(-90.0, false, false), 6.0);
        assert!(p.cost_s(120.0, false, false) > p.cost_s(90.0, false, false));
        // Never more than 1.3 times the side cost.
        assert!((p.cost_s(179.0, false, false) - 3.9).abs() < 1e-4);
        assert_eq!(p.cost_s(0.0, true, true), 24.0);
    }

    #[test]
    fn angles_wrap_around_north() {
        assert_eq!(turn_angle(350.0, 10.0), 20.0);
        assert_eq!(turn_angle(10.0, 350.0), -20.0);
        assert_eq!(turn_angle(90.0, -90.0), 180.0);
    }

    #[test]
    fn durations_are_never_zero() {
        assert_eq!(to_ds(0.0), 1);
        assert_eq!(to_ds(11.1), 111);
    }
}
