//! What can be reached from a point in some time: a search on the turn graph up to the longest time
//! asked for, and for each time the outline of the cells of a grid that the roads reached touch.
//! The cells are about a hundredth of the area, so an outline follows the roads, not each of them.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::cch::INF;
use crate::graph::Graph;
use crate::snap::Snap;
use crate::turn::TurnGraph;

/// The cells across the area reached, at the most.
const CELLS: f64 = 100.0;
const MIN_CELL_DEG: f64 = 0.0005;

/// When each edge reached is entered, in tenths of a second; on the edges of the start, before 0 by
/// what the part behind the start would have taken.
pub fn reach(turns: &TurnGraph, from: &[Snap], limit_ds: i64) -> HashMap<u32, i64> {
    let mut entered: HashMap<u32, i64> = HashMap::new();
    let mut heap = BinaryHeap::new();
    for snap in from {
        let at = -((snap.fraction * turns.edge_ds[snap.edge as usize] as f64).round() as i64);
        if entered.get(&snap.edge).is_none_or(|t| at < *t) {
            entered.insert(snap.edge, at);
            heap.push(Reverse((at, snap.edge)));
        }
    }
    while let Some(Reverse((at, edge))) = heap.pop() {
        if entered.get(&edge).is_some_and(|t| at > *t) {
            continue;
        }
        for arc in turns.arcs_from(edge) {
            if arc.weight_ds >= INF {
                continue;
            }
            let next = at + arc.weight_ds as i64;
            if next <= limit_ds && entered.get(&arc.head).is_none_or(|t| next < *t) {
                entered.insert(arc.head, next);
                heap.push(Reverse((next, arc.head)));
            }
        }
    }
    entered
}

/// The points of the roads reached by `limit_ds`: those of each edge, up to where a car gets.
fn points(graph: &Graph, turns: &TurnGraph, entered: &HashMap<u32, i64>, limit_ds: i64) -> Vec<[f64; 2]> {
    let mut points = Vec::new();
    for (edge, at) in entered {
        if *at > limit_ds {
            continue;
        }
        let ds = turns.edge_ds[*edge as usize].max(1) as f64;
        let share = ((limit_ds - at) as f64 / ds).min(1.0);
        let shape: Vec<[f64; 2]> = graph.shape(&graph.edges[*edge as usize]).collect();
        let last = ((shape.len() - 1) as f64 * share).floor() as usize;
        points.extend_from_slice(&shape[..=last]);
    }
    points
}

/// The outline of what `points` touch, as `[lon, lat]`, closed and counterclockwise: the ring of
/// their cells that holds the largest area. A hole in it, or an island apart, is not said.
/// `None` with no points.
pub fn outline(points: &[[f64; 2]]) -> Option<Vec<[f64; 2]>> {
    let first = points.first()?;
    let (mut south, mut north, mut west, mut east) = (first[0], first[0], first[1], first[1]);
    for p in points {
        (south, north, west, east) = (south.min(p[0]), north.max(p[0]), west.min(p[1]), east.max(p[1]));
    }
    let cell = ((north - south).max(east - west) / CELLS).max(MIN_CELL_DEG);
    let at = |p: &[f64; 2]| (((p[1] - west) / cell).floor() as i32, ((p[0] - south) / cell).floor() as i32);
    let mut cells: HashSet<(i32, i32)> = HashSet::new();
    for p in points {
        let (x, y) = at(p);
        // A cell and its neighbours, so that roads a cell apart make one area.
        for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            cells.insert((x + dx, y + dy));
        }
    }
    // The sides with the outside on their right, each from a corner to the next.
    let mut next: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    for &(x, y) in &cells {
        let mut side = |a, b| next.entry(a).or_default().push(b);
        if !cells.contains(&(x, y - 1)) {
            side((x, y), (x + 1, y));
        }
        if !cells.contains(&(x + 1, y)) {
            side((x + 1, y), (x + 1, y + 1));
        }
        if !cells.contains(&(x, y + 1)) {
            side((x + 1, y + 1), (x, y + 1));
        }
        if !cells.contains(&(x - 1, y)) {
            side((x, y + 1), (x, y));
        }
    }
    let mut starts: Vec<(i32, i32)> = next.keys().copied().collect();
    starts.sort_unstable();
    let area = |ring: &[(i32, i32)]| ring.windows(2).map(|w| w[0].0 as i64 * w[1].1 as i64 - w[1].0 as i64 * w[0].1 as i64).sum::<i64>();
    let mut best: Vec<(i32, i32)> = Vec::new();
    for start in starts {
        while next.get(&start).is_some_and(|n| !n.is_empty()) {
            let mut ring = vec![start];
            let mut corner = start;
            while let Some(to) = next.get_mut(&corner).and_then(Vec::pop) {
                ring.push(to);
                corner = to;
                if corner == start {
                    break;
                }
            }
            if area(&ring) > area(&best) {
                best = ring;
            }
        }
    }
    Some(best.iter().map(|(x, y)| [west + *x as f64 * cell, south + *y as f64 * cell]).collect())
}

/// The outline of what is reached by each of `limits_ds`, from what `reach` found up to the last.
pub fn contours(graph: &Graph, turns: &TurnGraph, entered: &HashMap<u32, i64>, limits_ds: &[i64]) -> Vec<Option<Vec<[f64; 2]>>> {
    limits_ds.iter().map(|limit| outline(&points(graph, turns, entered, *limit))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::Arc;

    /// Three edges in a line, of 10 s each.
    fn line() -> TurnGraph {
        let mut graph = TurnGraph { edge_ds: vec![100; 3], ..Default::default() };
        for arcs in [vec![Arc { head: 1, weight_ds: 100 }], vec![Arc { head: 2, weight_ds: 100 }], vec![]] {
            graph.first_arc.push(graph.arcs.len() as u32);
            graph.arcs.extend(arcs);
        }
        graph.first_arc.push(graph.arcs.len() as u32);
        graph
    }

    fn snap(edge: u32, fraction: f64) -> Snap {
        Snap { edge, fraction, distance_m: 0.0, point: [0.0, 0.0] }
    }

    #[test]
    fn it_goes_as_far_as_the_time_allows_from_where_it_starts() {
        let graph = line();
        let found = reach(&graph, &[snap(0, 0.5)], 60);
        assert_eq!(found.get(&0), Some(&-50));
        assert_eq!(found.get(&1), Some(&50));
        assert_eq!(found.get(&2), None, "150 ds away");
    }

    #[test]
    fn a_closed_arc_is_not_taken() {
        let mut graph = line();
        graph.arcs[0].weight_ds = INF;
        assert_eq!(reach(&graph, &[snap(0, 0.0)], 1_000_000).len(), 1);
    }

    #[test]
    fn the_outline_is_a_closed_ring_around_the_points() {
        let points: Vec<[f64; 2]> = (0..50).flat_map(|i| [[40.0 + i as f64 * 0.001, -3.0], [40.0, -3.0 + i as f64 * 0.001]]).collect();
        let ring = outline(&points).unwrap();
        assert_eq!(ring.first(), ring.last());
        assert!(ring.len() > 8);
        let (lons, lats): (Vec<f64>, Vec<f64>) = ring.iter().map(|p| (p[0], p[1])).unzip();
        assert!(lats.iter().cloned().fold(f64::MIN, f64::max) >= 40.049 && lons.iter().cloned().fold(f64::MIN, f64::max) >= -2.951);
        assert_eq!(outline(&[]), None);
    }
}
