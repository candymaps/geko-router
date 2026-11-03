//! The reference route: Dijkstra on the turn graph, from where the points snapped. It is the
//! simplest algorithm that is right, so the faster ones are checked against it, and its time is
//! the baseline they are measured against.
//!
//! The label of an edge is the time at which a path enters it, in deciseconds. A path starts in
//! the middle of an edge, so its first arcs start from minus the part of that edge already
//! behind it, and it ends in the middle of another, adding the part before the point.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::graph::{Graph, haversine_m};
use crate::snap::Snap;
use crate::turn::TurnGraph;

const NONE: u32 = u32::MAX;
/// Set on the predecessor of an edge reached from where the path starts: the edge it names is the
/// edge that start is on.
const START: u32 = 1 << 31;

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub duration_s: f64,
    pub distance_m: f64,
    /// `[lat, lon]`, from the first point to the last.
    pub geometry: Vec<[f64; 2]>,
    /// The edges it goes along, the first and last of them in part.
    pub edges: Vec<u32>,
}

#[derive(Clone, Copy)]
enum Finish<'a> {
    /// Both points on the same edge, the second further on.
    SameEdge(&'a Snap, &'a Snap),
    /// On this edge, which the path enters, at the point.
    On(u32, &'a Snap),
}

fn improve<'a>(best: &mut Option<(i64, Finish<'a>)>, total: i64, finish: Finish<'a>) {
    if best.is_none_or(|(b, _)| total < b) {
        *best = Some((total, finish));
    }
}

fn part(fraction: f64, ds: u32) -> i64 {
    (fraction * ds as f64).round() as i64
}

/// The fastest route from any of the `from` points to any of the `to` points (the two directions of
/// a street), or `None` if there is none.
pub fn route(graph: &Graph, turns: &TurnGraph, from: &[Snap], to: &[Snap]) -> Option<Route> {
    let mut dist = vec![i64::MAX; turns.edges()];
    let mut pred = vec![NONE; turns.edges()];
    let mut heap = BinaryHeap::new();
    let mut best: Option<(i64, Finish)> = None;

    for f in from {
        for t in to {
            if f.edge == t.edge && t.fraction >= f.fraction {
                improve(&mut best, part(t.fraction - f.fraction, turns.edge_ds[f.edge as usize]), Finish::SameEdge(f, t));
            }
        }
        let behind = part(f.fraction, turns.edge_ds[f.edge as usize]);
        for arc in turns.arcs_from(f.edge) {
            let d = arc.weight_ds as i64 - behind;
            if d < dist[arc.head as usize] {
                dist[arc.head as usize] = d;
                pred[arc.head as usize] = f.edge | START;
                heap.push(Reverse((d, arc.head)));
            }
        }
    }
    let mut targets: HashMap<u32, Vec<&Snap>> = HashMap::new();
    for t in to {
        targets.entry(t.edge).or_default().push(t);
    }

    while let Some(Reverse((d, e))) = heap.pop() {
        if d > dist[e as usize] {
            continue;
        }
        if best.is_some_and(|(b, _)| d >= b) {
            break;
        }
        for t in targets.get(&e).map_or(&[][..], Vec::as_slice) {
            improve(&mut best, d + part(t.fraction, turns.edge_ds[e as usize]), Finish::On(e, t));
        }
        for arc in turns.arcs_from(e) {
            let nd = d + arc.weight_ds as i64;
            if nd < dist[arc.head as usize] {
                dist[arc.head as usize] = nd;
                pred[arc.head as usize] = e;
                heap.push(Reverse((nd, arc.head)));
            }
        }
    }

    let (total, finish) = best?;
    let (edges, start, end) = match finish {
        Finish::SameEdge(f, t) => (vec![f.edge], f, t),
        Finish::On(last, t) => {
            let mut edges = vec![last];
            let mut at = last;
            let start_edge = loop {
                let p = pred[at as usize];
                if p & START != 0 {
                    break p & !START;
                }
                edges.push(p);
                at = p;
            };
            edges.push(start_edge);
            edges.reverse();
            let start = from.iter().find(|f| f.edge == start_edge).expect("the start is one of the points");
            (edges, start, t)
        }
    };
    let (geometry, distance_m) = geometry(graph, &edges, start, end);
    Some(Route { duration_s: total as f64 / 10.0, distance_m, geometry, edges })
}

/// The points of the path, cut at where it starts and ends, and its length.
fn geometry(graph: &Graph, edges: &[u32], start: &Snap, end: &Snap) -> (Vec<[f64; 2]>, f64) {
    let (mut points, mut length) = (Vec::new(), 0.0);
    for (i, &e) in edges.iter().enumerate() {
        let from = if i == 0 { start.fraction } else { 0.0 };
        let to = if i == edges.len() - 1 { end.fraction } else { 1.0 };
        let (piece, piece_length) = slice(&graph.shape(&graph.edges[e as usize]).collect::<Vec<_>>(), from, to);
        length += piece_length;
        for p in piece {
            if points.last() != Some(&p) {
                points.push(p);
            }
        }
    }
    (points, length)
}

/// The part of a line between two fractions of its length, and how long it is.
fn slice(points: &[[f64; 2]], from: f64, to: f64) -> (Vec<[f64; 2]>, f64) {
    let d = |a: [f64; 2], b: [f64; 2]| haversine_m((a[0], a[1]), (b[0], b[1]));
    let total: f64 = points.windows(2).map(|w| d(w[0], w[1])).sum();
    let (a, b) = (from * total, to * total);
    let mut out = Vec::new();
    let mut walked = 0.0;
    let at = |pair: &[[f64; 2]], distance: f64, length: f64| {
        let t = if length == 0.0 { 0.0 } else { (distance / length).clamp(0.0, 1.0) };
        [pair[0][0] + t * (pair[1][0] - pair[0][0]), pair[0][1] + t * (pair[1][1] - pair[0][1])]
    };
    for pair in points.windows(2) {
        let length = d(pair[0], pair[1]);
        let end = walked + length;
        if end >= a && walked <= b {
            if out.is_empty() {
                out.push(at(pair, a - walked, length));
            }
            if end <= b {
                out.push(pair[1]);
            } else {
                out.push(at(pair, b - walked, length));
            }
        }
        walked = end;
    }
    if out.is_empty() {
        out.push(points[0]);
    }
    (out, (b - a).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Builder;
    use crate::osm::{Element, Tags};
    use crate::profile::Car;
    use crate::snap::Snapper;
    use crate::turn::{self, TurnProfile};

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn node(id: i64, lat: f64, lon: f64) -> Element {
        Element::Node { id, lat, lon, tags: Vec::new() }
    }

    fn way(id: i64, refs: &[i64], pairs: &[(&str, &str)]) -> Element {
        let mut all = vec![("highway", "residential")];
        all.extend_from_slice(pairs);
        Element::Way { id, refs: refs.to_vec(), tags: tags(&all) }
    }

    fn network(elements: &[Element]) -> (Graph, TurnGraph) {
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        let graph = builder.finish();
        let turns = turn::build(&graph, &TurnProfile::default());
        (graph, turns)
    }

    /// The crossing of the turn tests: arms of 111 m to the west (1), east (2), north (3) and south (4)
    /// of node 5, at 10 m/s.
    fn cross() -> (Graph, TurnGraph) {
        network(&[
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.0),
            node(4, -0.001, 0.0),
            node(5, 0.0, 0.0),
            way(10, &[1, 5], &[]),
            way(11, &[5, 2], &[]),
            way(12, &[4, 5], &[]),
            way(13, &[5, 3], &[]),
        ])
    }

    fn route_between(graph: &Graph, turns: &TurnGraph, from: (f64, f64), to: (f64, f64)) -> Option<Route> {
        let snapper = Snapper::new(graph, vec![true; graph.edges.len()]);
        route(graph, turns, &snapper.snap(graph, from.0, from.1, 100.0), &snapper.snap(graph, to.0, to.1, 100.0))
    }

    #[test]
    fn straight_on_and_a_left_turn() {
        let (g, t) = cross();
        // From the middle of the west arm to the middle of the east: 55.5 m + 55.5 m at 10 m/s.
        let straight = route_between(&g, &t, (0.0, -0.0005), (0.0, 0.0005)).unwrap();
        assert!((straight.duration_s - 11.1).abs() < 0.15, "{}", straight.duration_s);
        assert!((straight.distance_m - 111.2).abs() < 0.5, "{}", straight.distance_m);
        // To the middle of the north arm: the same, and a left turn of 6 s.
        let left = route_between(&g, &t, (0.0, -0.0005), (0.0005, 0.0)).unwrap();
        assert!((left.duration_s - 17.1).abs() < 0.15, "{}", left.duration_s);
        assert_eq!(left.geometry.first().map(|p| (p[0], p[1])), Some((0.0, -0.0005)));
        assert!(left.geometry.contains(&[0.0, 0.0]));
        let last = left.geometry.last().unwrap();
        assert!(last[0] > 0.0004 && last[1].abs() < 1e-9, "{last:?}");
    }

    #[test]
    fn the_same_street_in_both_directions() {
        let (g, t) = cross();
        let there = route_between(&g, &t, (0.0, -0.0008), (0.0, -0.0002)).unwrap();
        let back = route_between(&g, &t, (0.0, -0.0002), (0.0, -0.0008)).unwrap();
        assert!((there.duration_s - 6.7).abs() < 0.15, "{}", there.duration_s);
        // Going back means the other direction of the same street, which is as long.
        assert!((back.duration_s - there.duration_s).abs() < 0.15);
        assert_eq!(there.edges.len(), 1);
    }

    #[test]
    fn a_one_way_street_has_to_be_gone_round() {
        // A ring of 444 m, one way, counter-clockwise, in a single edge: from 80 % of it to 20 %
        // means 20 % to the end, a left turn of 6 s at the corner where it closes, and 20 % again.
        let (g, t) = network(&[
            node(1, 0.0, 0.0),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.001),
            node(4, 0.001, 0.0),
            way(10, &[1, 2, 3, 4, 1], &[("oneway", "yes")]),
        ]);
        assert_eq!(g.edges.len(), 1);
        let at = |fraction| Snap { edge: 0, fraction, distance_m: 0.0, point: [0.0, 0.0] };
        let round = route(&g, &t, &[at(0.8)], &[at(0.2)]).unwrap();
        let ring_s = g.edges[0].seconds as f64;
        assert!((round.duration_s - (0.4 * ring_s + 6.0)).abs() < 0.2, "{} vs {}", round.duration_s, 0.4 * ring_s + 6.0);
        let ahead = route(&g, &t, &[at(0.2)], &[at(0.8)]).unwrap();
        assert!((ahead.duration_s - 0.6 * ring_s).abs() < 0.2);
    }

    #[test]
    fn no_route_when_there_is_none() {
        let (g, t) = network(&[node(1, 0.0, 0.0), node(2, 0.0, 0.001), node(3, 0.01, 0.0), node(4, 0.01, 0.001), way(10, &[1, 2], &[]), way(11, &[3, 4], &[])]);
        let snapper = Snapper::new(&g, vec![true; g.edges.len()]);
        let from = snapper.snap(&g, 0.0, 0.0005, 100.0);
        let to = snapper.snap(&g, 0.01, 0.0005, 100.0);
        assert!(!from.is_empty() && !to.is_empty());
        assert_eq!(route(&g, &t, &from, &to), None);
    }

    #[test]
    fn slices_of_a_line() {
        let line = [[0.0, 0.0], [0.0, 0.001], [0.0, 0.002]];
        let (piece, length) = slice(&line, 0.25, 0.75);
        assert_eq!(piece.len(), 3);
        assert!((piece[0][1] - 0.0005).abs() < 1e-9 && (piece[2][1] - 0.0015).abs() < 1e-9);
        assert!((length - 111.2).abs() < 0.5, "{length}");
        let (whole, _) = slice(&line, 0.0, 1.0);
        assert_eq!(whole, line.to_vec());
    }
}
