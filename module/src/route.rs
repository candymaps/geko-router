//! The reference route: Dijkstra on the turn graph, from where the points snapped. It is the
//! simplest algorithm that is right, so the faster ones are checked against it, and its time is
//! the baseline they are measured against.
//!
//! The label of an edge is the time at which a path enters it, in deciseconds. A path starts in
//! the middle of an edge, so its first arcs start from minus the part of that edge already
//! behind it, and it ends in the middle of another, adding the part before the point.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::cch::{Cch, INF, Label, Scratch};
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
    /// How far along its first edge the route starts, and along its last it ends (0 to 1).
    pub start_fraction: f64,
    pub end_fraction: f64,
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
    Some(assemble(graph, edges, start, end, total))
}

fn assemble(graph: &Graph, edges: Vec<u32>, start: &Snap, end: &Snap, total: i64) -> Route {
    let (geometry, distance_m) = geometry(graph, &edges, start, end);
    Route { duration_s: total as f64 / 10.0, distance_m, geometry, edges, start_fraction: start.fraction, end_fraction: end.fraction }
}

struct Candidate<'a> {
    total: i64,
    from: &'a Snap,
    to: &'a Snap,
    edges: Vec<u32>,
}

/// The best way from the points `from` to the points `to` with a customizable contraction
/// hierarchy. Its queries do not search the graph, they walk the elimination tree. `edges` is
/// only filled in when `with_path`: finding the path costs about as much as finding its length.
fn cch_best<'a>(turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, from: &'a [Snap], to: &'a [Snap], with_path: bool) -> Option<Candidate<'a>> {
    // Points on different edges, nearly every route: one search for all their directions. What a
    // start leaves behind is taken off, and a search adds only, so all of them start from `base`.
    if let Some((sources, targets, base)) = ends(turns, from, to) {
        let (length, meet) = cch.query_many(scratch, &sources, &targets)?;
        let edges = if with_path { cch.path(scratch, meet) } else { Vec::new() };
        let start = from.iter().find(|f| edges.first() == Some(&f.edge)).unwrap_or(&from[0]);
        let end = to.iter().find(|t| edges.last() == Some(&t.edge)).unwrap_or(to.first()?);
        return Some(Candidate { total: length as i64 - base, from: start, to: end, edges });
    }
    let mut best: Option<Candidate<'a>> = None;
    for f in from {
        // The part of its edge that a path already has behind it, and the part of the last one before the point.
        let behind = part(f.fraction, turns.edge_ds[f.edge as usize]);
        for t in to {
            let ahead = part(t.fraction, turns.edge_ds[t.edge as usize]);
            if f.edge == t.edge && t.fraction >= f.fraction {
                let total = part(t.fraction - f.fraction, turns.edge_ds[f.edge as usize]);
                if best.as_ref().is_none_or(|b| total < b.total) {
                    best = Some(Candidate { total, from: f, to: t, edges: vec![f.edge] });
                }
            }
            // Behind the start on its own edge, it has to leave and come back round: by each turn.
            let leaves: Vec<(u32, u32)> = if f.edge == t.edge { turns.arcs_from(f.edge).iter().map(|a| (a.head, a.weight_ds)).collect() } else { vec![(f.edge, 0)] };
            for (head, turn) in leaves {
                let Some((distance, meet)) = cch.query(scratch, head, t.edge) else { continue };
                let total = turn as i64 + distance as i64 - behind + ahead;
                if best.as_ref().is_none_or(|b| total < b.total) {
                    let mut edges = Vec::new();
                    if with_path {
                        edges = cch.path(scratch, meet);
                        if head != f.edge {
                            edges.insert(0, f.edge);
                        }
                    }
                    best = Some(Candidate { total, from: f, to: t, edges });
                }
            }
        }
    }
    best
}

type Ends = (Vec<(u32, u32)>, Vec<(u32, u32)>, i64);

/// For points on different edges, nearly every route: where a search starts and ends, each with what
/// starting or ending there adds, and `base`, what all the starts have in common (a start leaves its part
/// of the edge behind, and a search only adds, so all of them start from `base`).
fn ends(turns: &TurnGraph, from: &[Snap], to: &[Snap]) -> Option<Ends> {
    if !from.iter().all(|f| to.iter().all(|t| f.edge != t.edge)) {
        return None;
    }
    let behind = |f: &Snap| part(f.fraction, turns.edge_ds[f.edge as usize]);
    let base = from.iter().map(behind).max()?;
    let sources = from.iter().map(|f| (f.edge, (base - behind(f)) as u32)).collect();
    let targets = to.iter().map(|t| (t.edge, part(t.fraction, turns.edge_ds[t.edge as usize]) as u32)).collect();
    Some((sources, targets, base))
}

/// How much longer than the fastest an alternative may take, as a fraction of it.
const MAX_STRETCH: f64 = 0.25;
/// How much of an alternative may be along a route already given, as a fraction of its own duration.
const MAX_SHARING: f64 = 0.8;
/// How much longer than the shortest way the part of an alternative around its meeting point may be,
/// as a fraction of it: the alternative is not a detour that a better road avoids.
const LOCAL_SLACK: f64 = 0.05;
/// How many different paths are looked at to find the alternatives.
const MAX_TRIALS: usize = 64;

/// The fastest route and up to `count` alternatives to it, fastest first. An alternative is a path
/// through another meeting point of the two searches (`Cch::meeting_points`) that is at most
/// `MAX_STRETCH` longer than the fastest, shares at most `MAX_SHARING` of itself with each route already
/// given, and is locally optimal: the part of it around its meeting point, a quarter of the alternative
/// long, is within `LOCAL_SLACK` of the shortest way between its ends. Only for points on different
/// edges; else there are no alternatives.
pub fn alternatives_cch(graph: &Graph, turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, from: &[Snap], to: &[Snap], count: usize) -> Vec<Route> {
    let Some((sources, targets, base)) = ends(turns, from, to).filter(|_| count > 0) else {
        return route_cch(graph, turns, cch, scratch, from, to).into_iter().collect();
    };
    let meets = cch.meeting_points(scratch, &sources, &targets);
    let Some(&(shortest, _)) = meets.first() else { return Vec::new() };
    let limit = shortest as f64 - base as f64;
    let limit = (limit * (1.0 + MAX_STRETCH)) as i64 + base;

    // Unpack the paths first: every later search overwrites what the meeting points left in the scratch.
    let mut paths: Vec<(i64, u32, Vec<u32>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &(length, meet) in &meets {
        if length as i64 > limit || paths.len() >= MAX_TRIALS {
            break;
        }
        let at = cch.node_at(meet);
        if seen.contains(&at) {
            continue;
        }
        let edges = cch.path(scratch, meet);
        seen.extend(edges.iter().copied());
        paths.push((length as i64 - base, at, edges));
    }

    let ds = |e: u32| u64::from(turns.edge_ds[e as usize]);
    let mut given: Vec<(i64, Vec<u32>)> = Vec::new();
    let mut routes = Vec::new();
    for (i, (total, at, edges)) in paths.into_iter().enumerate() {
        if i > 0 {
            if routes.len() > count {
                break;
            }
            let own: u64 = edges.iter().map(|e| ds(*e)).sum();
            let shares_too_much = given.iter().any(|(_, other)| {
                let shared: u64 = edges.iter().filter(|e| other.contains(e)).map(|e| ds(*e)).sum();
                shared as f64 > MAX_SHARING * own as f64
            });
            if shares_too_much || !locally_optimal(turns, cch, scratch, &edges, at, total) {
                continue;
            }
        }
        let start = from.iter().find(|f| edges.first() == Some(&f.edge)).unwrap_or(&from[0]);
        let end = to.iter().find(|t| edges.last() == Some(&t.edge)).unwrap_or(&to[0]);
        routes.push(assemble(graph, edges.clone(), start, end, total));
        given.push((total, edges));
    }
    routes
}

/// Whether the part of `edges` around `at`, up to an eighth of `total` ds each way, is a shortest way
/// between its ends, give or take `LOCAL_SLACK`.
fn locally_optimal(turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, edges: &[u32], at: u32, total: i64) -> bool {
    let Some(k) = edges.iter().position(|e| *e == at) else { return true };
    let mut reached = vec![0u64];
    for pair in edges.windows(2) {
        let weight = turns.arcs_from(pair[0]).iter().find(|a| a.head == pair[1]).map_or(0, |a| u64::from(a.weight_ds));
        reached.push(reached.last().copied().unwrap_or(0) + weight);
    }
    let reach = (total.max(0) as u64) / 8;
    let a = (0..=k).find(|i| reached[k] - reached[*i] <= reach).unwrap_or(k);
    let b = (k..edges.len()).rev().find(|i| reached[*i] - reached[k] <= reach).unwrap_or(k);
    if a >= b {
        return true;
    }
    let own = reached[b] - reached[a];
    cch.query(scratch, edges[a], edges[b]).is_some_and(|(shortest, _)| own as f64 <= shortest as f64 * (1.0 + LOCAL_SLACK))
}

/// The fastest route, the same as `route`.
pub fn route_cch(graph: &Graph, turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, from: &[Snap], to: &[Snap]) -> Option<Route> {
    let best = cch_best(turns, cch, scratch, from, to, true)?;
    Some(assemble(graph, best.edges, best.from, best.to, best.total))
}

/// Only how long the fastest route takes, in seconds: what a matrix of routes asks for.
pub fn duration_cch(turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, from: &[Snap], to: &[Snap]) -> Option<f64> {
    cch_best(turns, cch, scratch, from, to, false).map(|b| b.total as f64 / 10.0)
}

/// What a point of a matrix is for the many-to-many search: what it starts or ends with in each of its
/// directions, and what a start leaves behind.
struct Endpoint {
    labels: Vec<Label>,
    base: i64,
}

/// A cell: its duration, or that it needs a query of its own because two points are on one edge.
enum Cell {
    Done(Option<f64>),
    Own,
}

/// The durations from each point of `from` to each of `to`, in seconds (each point is the list of its
/// snaps; an empty one has no street near and its row or column is `None`), the same as
/// `duration_cch` for every pair but with the work shared: one walk up the elimination tree for each
/// point, and for each cell the sum over the ancestors that the two share. The rows are split among
/// the processors there are.
pub fn durations_cch(turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, from: &[Vec<Snap>], to: &[Vec<Snap>]) -> Vec<Vec<Option<f64>>> {
    let behind = |f: &Snap| part(f.fraction, turns.edge_ds[f.edge as usize]);
    let starts: Vec<Option<Endpoint>> = from
        .iter()
        .map(|snaps| {
            let base = snaps.iter().map(behind).max()?;
            Some(Endpoint { labels: snaps.iter().map(|f| cch.label(scratch, f.edge, (base - behind(f)) as u32, false)).collect(), base })
        })
        .collect();
    let ends: Vec<Option<Endpoint>> = to
        .iter()
        .map(|snaps| {
            let labels = snaps.iter().map(|t| cch.label(scratch, t.edge, part(t.fraction, turns.edge_ds[t.edge as usize]) as u32, true)).collect();
            (!snaps.is_empty()).then_some(Endpoint { labels, base: 0 })
        })
        .collect();

    let cell = |i: usize, j: usize| -> Cell {
        let (Some(start), Some(end)) = (&starts[i], &ends[j]) else { return Cell::Done(None) };
        if from[i].iter().any(|f| to[j].iter().any(|t| f.edge == t.edge)) {
            return Cell::Own;
        }
        let best = start.labels.iter().flat_map(|a| end.labels.iter().map(move |b| Label::meet(a, b))).min().unwrap_or(INF);
        Cell::Done((best < INF).then(|| (best as i64 - start.base) as f64 / 10.0))
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(from.len().max(1));
    let rows_per_thread = from.len().div_ceil(threads).max(1);
    let cell = &cell;
    let mut cells: Vec<Vec<Cell>> = std::thread::scope(|scope| {
        let chunks: Vec<_> = (0..from.len())
            .step_by(rows_per_thread)
            .map(|first| scope.spawn(move || (first..(first + rows_per_thread).min(from.len())).map(|i| (0..to.len()).map(|j| cell(i, j)).collect::<Vec<_>>()).collect::<Vec<_>>()))
            .collect();
        chunks.into_iter().flat_map(|c| c.join().expect("a thread of the matrix")).collect()
    });
    cells
        .iter_mut()
        .enumerate()
        .map(|(i, row)| {
            row.iter_mut()
                .enumerate()
                .map(|(j, c)| match c {
                    Cell::Done(duration) => *duration,
                    Cell::Own => duration_cch(turns, cch, scratch, &from[i], &to[j]),
                })
                .collect()
        })
        .collect()
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

    /// Both algorithms on the same pairs of points of a network: the same time, and a route that
    /// really goes from one to the other.
    fn same_as_dijkstra(graph: &Graph, turns: &TurnGraph, points: &[(f64, f64)]) {
        let cch = Cch::new(turns);
        let mut scratch = Scratch::new(&cch);
        let snapper = Snapper::new(graph, vec![true; graph.edges.len()]);
        for a in points {
            for b in points {
                let (from, to) = (snapper.snap(graph, a.0, a.1, 100.0), snapper.snap(graph, b.0, b.1, 100.0));
                let expected = route(graph, turns, &from, &to);
                let got = route_cch(graph, turns, &cch, &mut scratch, &from, &to);
                assert_eq!(got.as_ref().map(|r| r.duration_s), expected.as_ref().map(|r| r.duration_s), "{a:?} -> {b:?}");
                assert_eq!(duration_cch(turns, &cch, &mut scratch, &from, &to), expected.as_ref().map(|r| r.duration_s));
                if let (Some(got), Some(expected)) = (got, expected) {
                    assert!((got.distance_m - expected.distance_m).abs() < 1.0, "{a:?} -> {b:?}");
                    let (g, e) = (got.geometry[0], expected.geometry[0]);
                    assert!((g[0] - e[0]).abs() < 1e-9 && (g[1] - e[1]).abs() < 1e-9, "{a:?} -> {b:?}");
                }
            }
        }
    }

    /// Two streets from the west stub to the east stub, one straight and one round by the north, and a
    /// third that goes so far south that it is no alternative.
    fn two_ways() -> (Graph, TurnGraph) {
        network(&[
            node(10, 0.0, -0.001),
            node(1, 0.0, 0.0),
            node(2, 0.0, 0.003),
            node(3, 0.0, 0.006),
            node(11, 0.0, 0.007),
            node(4, 0.0002, 0.0),
            node(5, 0.0002, 0.006),
            node(6, -0.003, 0.003),
            way(20, &[10, 1], &[]),
            way(21, &[1, 2, 3], &[]),
            way(22, &[3, 11], &[]),
            way(23, &[1, 4, 5, 3], &[]),
            way(24, &[1, 6, 3], &[]),
        ])
    }

    #[test]
    fn a_route_has_the_alternative_that_is_not_too_long_and_not_the_same() {
        let (g, t) = two_ways();
        let cch = Cch::new(&t);
        let mut scratch = Scratch::new(&cch);
        let snapper = Snapper::new(&g, vec![true; g.edges.len()]);
        let (from, to) = (snapper.snap(&g, 0.0, -0.0005, 100.0), snapper.snap(&g, 0.0, 0.0065, 100.0));
        let best = route_cch(&g, &t, &cch, &mut scratch, &from, &to).unwrap();
        let routes = alternatives_cch(&g, &t, &cch, &mut scratch, &from, &to, 5);
        assert_eq!(routes.len(), 2, "{routes:?}");
        assert_eq!(routes[0], best);
        assert!(routes[1].duration_s > best.duration_s && routes[1].duration_s <= best.duration_s * 1.25, "{} vs {}", routes[1].duration_s, best.duration_s);
        assert_ne!(routes[1].edges, best.edges);
        assert_eq!(alternatives_cch(&g, &t, &cch, &mut scratch, &from, &to, 0), vec![best]);
    }

    #[test]
    fn the_hierarchy_gives_the_same_routes_on_a_crossing() {
        let (g, t) = cross();
        same_as_dijkstra(&g, &t, &[(0.0, -0.0005), (0.0, 0.0005), (0.0005, 0.0), (-0.0005, 0.0), (0.0, -0.0008), (0.0, -0.0002)]);
    }

    #[test]
    fn a_matrix_is_what_each_pair_gives() {
        let (g, t) = cross();
        let cch = Cch::new(&t);
        let mut scratch = Scratch::new(&cch);
        let snapper = Snapper::new(&g, vec![true; g.edges.len()]);
        // Pairs on one edge (both ways of the west arm), points on different arms, and one with no street near.
        let points = [(0.0, -0.0005), (0.0, -0.0008), (0.0, 0.0005), (0.0005, 0.0), (-0.0005, 0.0), (0.0, -0.0002), (1.0, 1.0)];
        let snaps: Vec<Vec<Snap>> = points.iter().map(|p| snapper.snap(&g, p.0, p.1, 100.0)).collect();
        assert!(snaps[6].is_empty());
        let matrix = durations_cch(&t, &cch, &mut scratch, &snaps, &snaps);
        for (i, row) in matrix.iter().enumerate() {
            for (j, got) in row.iter().enumerate() {
                let expected = if snaps[i].is_empty() || snaps[j].is_empty() { None } else { duration_cch(&t, &cch, &mut scratch, &snaps[i], &snaps[j]) };
                assert_eq!(*got, expected, "{:?} -> {:?}", points[i], points[j]);
            }
        }
        assert!(matrix[0][2].is_some() && matrix[6][0].is_none() && matrix[0][6].is_none());
        let none: Vec<Vec<Snap>> = Vec::new();
        assert!(durations_cch(&t, &cch, &mut scratch, &none, &snaps).is_empty());
    }

    #[test]
    fn the_hierarchy_goes_round_a_one_way_ring() {
        let (g, t) = network(&[
            node(1, 0.0, 0.0),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.001),
            node(4, 0.001, 0.0),
            way(10, &[1, 2, 3, 4, 1], &[("oneway", "yes")]),
        ]);
        let cch = Cch::new(&t);
        let mut scratch = Scratch::new(&cch);
        let at = |fraction| Snap { edge: 0, fraction, distance_m: 0.0, point: [0.0, 0.0] };
        for (from, to) in [(0.8, 0.2), (0.2, 0.8), (0.5, 0.5)] {
            let expected = route(&g, &t, &[at(from)], &[at(to)]).unwrap();
            let got = route_cch(&g, &t, &cch, &mut scratch, &[at(from)], &[at(to)]).unwrap();
            assert_eq!(got.duration_s, expected.duration_s, "{from} -> {to}");
        }
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
