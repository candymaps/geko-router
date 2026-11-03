//! Snapping: the place on the road network that a coordinate is closest to. It returns the directed
//! edges that pass there (both directions of a two-way street), at which fraction of each, leaving
//! out the ones a car cannot get out of (`scc`).

use std::collections::HashMap;

use crate::graph::{Graph, haversine_m};

const CELL_DEG: f64 = 0.002;
const M_PER_DEG_LAT: f64 = 110_574.0;
const M_PER_DEG_LON_EQUATOR: f64 = 111_320.0;

/// A point on a directed edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub edge: u32,
    /// How far along the edge, from its first node: 0 to 1, by length.
    pub fraction: f64,
    /// From the coordinate asked for to the road.
    pub distance_m: f64,
    /// The point on the road, `[lat, lon]`.
    pub point: [f64; 2],
}

pub struct Snapper {
    cells: HashMap<(i32, i32), Vec<(u32, u32)>>,
    /// The edges that share a shape: the two directions of a street.
    by_shape: HashMap<u32, Vec<u32>>,
    allowed: Vec<bool>,
}

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    ((lat / CELL_DEG).floor() as i32, (lon / CELL_DEG).floor() as i32)
}

/// The closest point of segment `a`-`b` to `p`: its place on the segment (0 to 1), its distance
/// in metres and the point. Planar around `p`, exact enough for the metres that matter.
fn project(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, f64, [f64; 2]) {
    let kx = M_PER_DEG_LON_EQUATOR * p[0].to_radians().cos();
    let (ax, ay) = ((a[1] - p[1]) * kx, (a[0] - p[0]) * M_PER_DEG_LAT);
    let (bx, by) = ((b[1] - p[1]) * kx, (b[0] - p[0]) * M_PER_DEG_LAT);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0) };
    let (qx, qy) = (ax + t * dx, ay + t * dy);
    (t, qx.hypot(qy), [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])])
}

impl Snapper {
    /// `allowed[e]` says whether edge `e` may be snapped to.
    pub fn new(graph: &Graph, allowed: Vec<bool>) -> Self {
        let mut by_shape: HashMap<u32, Vec<u32>> = HashMap::new();
        // Only what can be snapped to: a segment fills the cells of its box, and a ferry in diagonal
        // across a sea, which no point lands on, would fill thousands.
        for (i, e) in graph.edges.iter().enumerate().filter(|(i, _)| allowed[*i]) {
            by_shape.entry(e.shape_start).or_default().push(i as u32);
        }
        let mut cells: HashMap<(i32, i32), Vec<(u32, u32)>> = HashMap::new();
        for (&start, edges) in &by_shape {
            let edge = &graph.edges[edges[0] as usize];
            let points = &graph.shapes[start as usize..(start + edge.shape_len) as usize];
            for (i, pair) in points.windows(2).enumerate() {
                let ((r0, c0), (r1, c1)) = (cell(pair[0][0], pair[0][1]), cell(pair[1][0], pair[1][1]));
                for r in r0.min(r1)..=r0.max(r1) {
                    for c in c0.min(c1)..=c0.max(c1) {
                        cells.entry((r, c)).or_default().push((start, i as u32));
                    }
                }
            }
        }
        Self { cells, by_shape, allowed }
    }

    /// The closest street within `max_m` of the coordinate, in each direction a car can use it.
    /// Empty when there is none.
    pub fn snap(&self, graph: &Graph, lat: f64, lon: f64, max_m: f64) -> Vec<Snap> {
        let p = [lat, lon];
        let (row, col) = cell(lat, lon);
        let cell_m = CELL_DEG * M_PER_DEG_LAT.min(M_PER_DEG_LON_EQUATOR * lat.to_radians().cos());
        let rings = (max_m / cell_m).ceil() as i32 + 1;
        let mut best: Option<(f64, u32, u32, f64, [f64; 2])> = None;
        for ring in 0..=rings {
            for r in row - ring..=row + ring {
                for c in col - ring..=col + ring {
                    if (r - row).abs().max((c - col).abs()) != ring {
                        continue;
                    }
                    for &(shape, i) in self.cells.get(&(r, c)).map_or(&[][..], Vec::as_slice) {
                        if !self.by_shape[&shape].iter().any(|e| self.allowed[*e as usize]) {
                            continue;
                        }
                        let points = &graph.shapes[shape as usize..];
                        let (t, distance, point) = project(p, points[i as usize], points[i as usize + 1]);
                        if best.is_none_or(|b| distance < b.0) {
                            best = Some((distance, shape, i, t, point));
                        }
                    }
                }
            }
            // Everything within `ring` cells has been seen: nothing closer can still turn up.
            if best.is_some_and(|b| b.0 <= ring as f64 * cell_m) {
                break;
            }
        }
        let Some((distance_m, shape, segment, t, point)) = best.filter(|b| b.0 <= max_m) else { return Vec::new() };
        let edges = &self.by_shape[&shape];
        let points = &graph.shapes[shape as usize..shape as usize + graph.edges[edges[0] as usize].shape_len as usize];
        let length = |a: [f64; 2], b: [f64; 2]| haversine_m((a[0], a[1]), (b[0], b[1]));
        let before: f64 = points[..=segment as usize].windows(2).map(|w| length(w[0], w[1])).sum();
        let total: f64 = points.windows(2).map(|w| length(w[0], w[1])).sum();
        let along = if total == 0.0 { 0.0 } else { ((before + t * length(points[segment as usize], points[segment as usize + 1])) / total).clamp(0.0, 1.0) };
        edges
            .iter()
            .filter(|e| self.allowed[**e as usize])
            .map(|&edge| Snap { edge, fraction: if graph.edges[edge as usize].reversed { 1.0 - along } else { along }, distance_m, point })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Builder;
    use crate::osm::{Element, Tags};
    use crate::profile::Car;

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

    fn graph(elements: &[Element]) -> Graph {
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        builder.finish()
    }

    /// An east-west street from node 1 to node 3 through node 2 (a bend, 0.001° north of the line).
    fn street(extra: &[(&str, &str)]) -> Graph {
        graph(&[node(1, 0.0, 0.000), node(2, 0.001, 0.001), node(3, 0.0, 0.002), way(10, &[1, 2, 3], extra)])
    }

    fn all(graph: &Graph) -> Vec<bool> {
        vec![true; graph.edges.len()]
    }

    #[test]
    fn both_directions_of_a_two_way_street_at_mirrored_fractions() {
        let g = street(&[]);
        let snapper = Snapper::new(&g, all(&g));
        // Next to node 1, 10 m south of the street.
        let snaps = snapper.snap(&g, -0.00009, 0.0, 200.0);
        assert_eq!(snaps.len(), 2);
        let forward = snaps.iter().find(|s| !g.edges[s.edge as usize].reversed).unwrap();
        let backward = snaps.iter().find(|s| g.edges[s.edge as usize].reversed).unwrap();
        assert!(forward.fraction < 0.01, "{}", forward.fraction);
        assert!((forward.fraction + backward.fraction - 1.0).abs() < 1e-9);
        assert!((forward.distance_m - 9.95).abs() < 0.1, "{}", forward.distance_m);
    }

    #[test]
    fn a_one_way_street_gives_its_one_edge() {
        let g = street(&[("oneway", "yes")]);
        let snaps = Snapper::new(&g, all(&g)).snap(&g, 0.0005, 0.0005, 200.0);
        assert_eq!(snaps.len(), 1);
        assert!(snaps[0].fraction > 0.0 && snaps[0].fraction < 0.5, "{}", snaps[0].fraction);
    }

    #[test]
    fn the_fraction_follows_the_bend() {
        let g = street(&[("oneway", "yes")]);
        // Right at the bend, node 2: halfway along the street, since both halves are as long.
        let snaps = Snapper::new(&g, all(&g)).snap(&g, 0.001, 0.001, 50.0);
        assert!((snaps[0].fraction - 0.5).abs() < 0.01, "{}", snaps[0].fraction);
        assert!(snaps[0].distance_m < 0.5);
    }

    #[test]
    fn nothing_beyond_the_limit() {
        let g = street(&[]);
        let snapper = Snapper::new(&g, all(&g));
        assert!(snapper.snap(&g, 0.05, 0.001, 500.0).is_empty());
        assert!(!snapper.snap(&g, 0.05, 0.001, 6000.0).is_empty());
    }

    #[test]
    fn what_cannot_be_snapped_to_takes_no_room_in_the_index() {
        // A long diagonal, like a ferry: its box is thousands of cells.
        let g = graph(&[node(1, 0.0, 0.0), node(2, 1.0, 1.0), node(3, 2.0, 2.0), node(4, 2.0, 2.001), way(10, &[1, 2], &[]), way(11, &[3, 4], &[])]);
        let allowed: Vec<bool> = g.edges.iter().map(|e| e.way == 11).collect();
        let snapper = Snapper::new(&g, allowed);
        assert!(snapper.cells.len() < 10, "{} cells", snapper.cells.len());
    }

    #[test]
    fn edges_a_car_cannot_leave_are_skipped() {
        let g = graph(&[
            node(1, 0.0, 0.000),
            node(2, 0.0, 0.001),
            node(3, 0.0003, 0.000),
            node(4, 0.0003, 0.001),
            way(10, &[1, 2], &[]),
            way(11, &[3, 4], &[]),
        ]);
        let near_11 = (0.0003, 0.0005);
        let mut allowed = all(&g);
        let nearest = Snapper::new(&g, allowed.clone()).snap(&g, near_11.0, near_11.1, 500.0);
        assert_eq!(g.edges[nearest[0].edge as usize].way, 11);
        for (i, e) in g.edges.iter().enumerate() {
            allowed[i] = e.way != 11;
        }
        let snapped = Snapper::new(&g, allowed).snap(&g, near_11.0, near_11.1, 500.0);
        assert!(snapped.iter().all(|s| g.edges[s.edge as usize].way == 10));
        assert!(!snapped.is_empty());
    }
}
