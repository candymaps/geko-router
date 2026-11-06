//! Snapping: the place on the road network that a coordinate is closest to. It returns the directed
//! edges that pass there (both directions of a two-way street), at which fraction of each, leaving
//! out the ones a car cannot get out of (`scc`).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{BuildHasherDefault, Hasher};

use crate::graph::{Graph, haversine_m};

/// The hash of the multiplicative family that rustc uses: keys here are small integers the program
/// itself makes, so the protection against collisions that `SipHash` pays for is not needed, and
/// the two maps of the snapper have millions of entries.
#[derive(Default)]
struct IntHasher(u64);

impl IntHasher {
    fn add(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for IntHasher {
    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|b| self.add(u64::from(*b)));
    }
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }
    fn write_i32(&mut self, value: i32) {
        self.add(u64::from(value as u32));
    }
    fn finish(&self) -> u64 {
        // The low bits of a product depend only on the low bits of the factors: fold the high ones in.
        self.0 ^ (self.0 >> 32)
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<IntHasher>>;

/// The segments that cross each cell: shape and segment within it.
type Cells = Map<(i32, i32), Vec<(u32, u32)>>;

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

/// How many maps the cells are split in: a fixed number, so that the index does not depend on the machine.
const BANDS: usize = 16;

fn band_of(row: i32) -> usize {
    row.rem_euclid(BANDS as i32) as usize
}

pub struct Snapper {
    /// By band of rows (`band_of`).
    cells: Vec<Cells>,
    /// The edges that share a shape: the two directions of a street.
    by_shape: Map<u32, Vec<u32>>,
    allowed: Vec<bool>,
}

/// The closest place of a shape to a point: how far, on which segment, where on it, and its coordinates.
type Closest = (f64, u32, f64, [f64; 2]);

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
        let mut by_shape: Map<u32, Vec<u32>> = Map::default();
        let mut shapes: Vec<(u32, u32)> = Vec::new();
        // Only what can be snapped to: a segment fills the cells of its box, and a ferry in diagonal
        // across a sea, which no point lands on, would fill thousands.
        for (i, e) in graph.edges.iter().enumerate().filter(|(i, _)| allowed[*i]) {
            match by_shape.entry(e.shape_start) {
                Entry::Occupied(mut edges) => edges.get_mut().push(i as u32),
                Entry::Vacant(slot) => {
                    slot.insert(vec![i as u32]);
                    shapes.push((e.shape_start, e.shape_len));
                }
            }
        }
        // In the order of the graph, which follows the ways of the extract and so goes street by street:
        // the cells of one shape are next to those of the previous one, which the cache likes.
        shapes.sort_unstable();
        // The cells are split in bands of rows, each built by a thread that keeps the cells of its own:
        // the same content, in the same order, as one map would have, and nobody waits for anybody.
        let cells = std::thread::scope(|scope| {
            let (shapes, graph) = (&shapes, graph);
            let builders: Vec<_> = (0..BANDS).map(|b| scope.spawn(move || Self::band(graph, shapes, b))).collect();
            builders.into_iter().map(|builder| builder.join().expect("a thread of the snapper")).collect()
        });
        Self { cells, by_shape, allowed }
    }

    /// The cells of the rows of band `b` that the segments of `shapes` (start and length of each) cross.
    fn band(graph: &Graph, shapes: &[(u32, u32)], b: usize) -> Cells {
        let mut cells = Cells::default();
        for &(start, len) in shapes {
            let points = &graph.shapes[start as usize..(start + len) as usize];
            for (i, pair) in points.windows(2).enumerate() {
                let ((r0, c0), (r1, c1)) = (cell(pair[0][0], pair[0][1]), cell(pair[1][0], pair[1][1]));
                for r in (r0.min(r1)..=r0.max(r1)).filter(|r| band_of(*r) == b) {
                    for c in c0.min(c1)..=c0.max(c1) {
                        cells.entry((r, c)).or_default().push((start, i as u32));
                    }
                }
            }
        }
        cells
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
                    for &(shape, i) in self.cells[band_of(r)].get(&(r, c)).map_or(&[][..], Vec::as_slice) {
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
        self.on_shape(graph, shape, segment, t, distance_m, point)
    }

    /// Up to `count` streets within `max_m`, closest first, each in the directions it can be used.
    pub fn near(&self, graph: &Graph, lat: f64, lon: f64, max_m: f64, count: usize) -> Vec<Vec<Snap>> {
        let p = [lat, lon];
        let (row, col) = cell(lat, lon);
        let cell_m = CELL_DEG * M_PER_DEG_LAT.min(M_PER_DEG_LON_EQUATOR * lat.to_radians().cos());
        let rings = (max_m / cell_m).ceil() as i32 + 1;
        let mut by_shape: Map<u32, Closest> = Map::default();
        for r in row - rings..=row + rings {
            for c in col - rings..=col + rings {
                for &(shape, i) in self.cells[band_of(r)].get(&(r, c)).map_or(&[][..], Vec::as_slice) {
                    if !self.by_shape[&shape].iter().any(|e| self.allowed[*e as usize]) {
                        continue;
                    }
                    let points = &graph.shapes[shape as usize..];
                    let (t, distance, point) = project(p, points[i as usize], points[i as usize + 1]);
                    if distance <= max_m && by_shape.get(&shape).is_none_or(|b| distance < b.0) {
                        by_shape.insert(shape, (distance, i, t, point));
                    }
                }
            }
        }
        let mut found: Vec<(u32, Closest)> = by_shape.into_iter().collect();
        found.sort_by(|a, b| a.1.0.total_cmp(&b.1.0).then(a.0.cmp(&b.0)));
        found.truncate(count);
        found.into_iter().map(|(shape, (distance, segment, t, point))| self.on_shape(graph, shape, segment, t, distance, point)).collect()
    }

    /// The directions of a street at a point of it.
    fn on_shape(&self, graph: &Graph, shape: u32, segment: u32, t: f64, distance_m: f64, point: [f64; 2]) -> Vec<Snap> {
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
    use crate::profile::Vehicle;

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
        let mut builder = Builder::new(Vehicle::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
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
        let cells: usize = snapper.cells.iter().map(Map::len).sum();
        assert!(cells < 10, "{cells} cells");
    }

    #[test]
    fn the_bands_hold_the_same_cells_as_one_map_would() {
        // Streets over many rows, so that every band has some: each cell must be where `snap` looks.
        let mut elements = Vec::new();
        for i in 0..40i64 {
            elements.push(node(2 * i + 1, i as f64 * 0.0021, 0.0));
            elements.push(node(2 * i + 2, i as f64 * 0.0021, 0.01));
            elements.push(way(100 + i, &[2 * i + 1, 2 * i + 2], &[]));
        }
        let g = graph(&elements);
        let snapper = Snapper::new(&g, all(&g));
        for (band, map) in snapper.cells.iter().enumerate() {
            assert!(!map.is_empty(), "band {band} is empty");
            assert!(map.keys().all(|(row, _)| band_of(*row) == band));
        }
        for i in 0..40 {
            let snaps = snapper.snap(&g, i as f64 * 0.0021 + 0.0001, 0.005, 100.0);
            assert_eq!(snaps.len(), 2, "street {i}");
        }
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
