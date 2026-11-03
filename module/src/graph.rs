//! The road graph of a country, from its OSM objects: the nodes where roads meet or end, and one
//! directed edge for each way between two of them, with its length, its time at free-flow speed
//! and its shape. The turn restrictions are read here and kept with their nodes; the turn-aware
//! graph that applies them is the next step (docs/ROUTER_PLAN.md).
//!
//! The builder reads the objects twice, because a PBF has the nodes first: the first pass (ways and
//! relations) finds which nodes matter, the second (nodes) keeps only their positions.

use std::collections::{HashMap, HashSet};

use crate::osm::{Element, MemberKind, tag};
use crate::profile::{Car, Direction};

const EARTH_RADIUS_M: f64 = 6_371_008.8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub osm_id: i64,
    pub lat: f64,
    pub lon: f64,
    /// A traffic light (`highway=traffic_signals`).
    pub signal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub way: i64,
    pub length_m: f32,
    /// At the free-flow speed of the way.
    pub seconds: f32,
    /// Its points between `from` and `to`, both included, in `Graph::shapes` (`shape_start..shape_start + shape_len`);
    /// `reversed` when they are stored the other way round.
    pub shape_start: u32,
    pub shape_len: u32,
    pub reversed: bool,
    /// A crossing by ferry: routes take it, but no point is ever on it.
    pub ferry: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictionKind {
    /// `no_*`: that turn is forbidden.
    Prohibited,
    /// `only_*`: no turn from that way at that node but this one.
    Mandatory,
}

/// A turn restriction through one node (`from` way, `via` node, `to` way). Those through a way are
/// not read yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restriction {
    pub from_way: i64,
    pub via: u32,
    pub to_way: i64,
    pub kind: RestrictionKind,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub shapes: Vec<[f64; 2]>,
    pub restrictions: Vec<Restriction>,
}

impl Graph {
    /// `allowed` without the ferries: the edges a point can snap to. Someone at the shore is on the
    /// road of the port, not on the line of a ferry out at sea.
    pub fn without_ferries(&self, mut allowed: Vec<bool>) -> Vec<bool> {
        allowed.iter_mut().zip(&self.edges).for_each(|(allow, edge)| *allow &= !edge.ferry);
        allowed
    }

    /// The shape of an edge, from its `from` to its `to`.
    pub fn shape(&self, edge: &Edge) -> impl Iterator<Item = [f64; 2]> + '_ {
        let points = &self.shapes[edge.shape_start as usize..(edge.shape_start + edge.shape_len) as usize];
        let (forward, backward) = if edge.reversed { (None, Some(points.iter().rev())) } else { (Some(points.iter()), None) };
        forward.into_iter().flatten().chain(backward.into_iter().flatten()).copied()
    }
}

struct UsableWay {
    id: i64,
    refs: Vec<i64>,
    speed_kmh: f32,
    direction: Direction,
    ferry: bool,
    duration_s: Option<f32>,
}

struct RawRestriction {
    from_way: i64,
    via_node: i64,
    to_way: i64,
    kind: RestrictionKind,
}

pub struct Builder {
    car: Car,
    ways: Vec<UsableWay>,
    restrictions: Vec<RawRestriction>,
    /// How many times each node appears in the usable ways.
    uses: HashMap<i64, u32>,
    coords: HashMap<i64, (f64, f64)>,
    signals: HashSet<i64>,
    /// The extracts of a scope overlap at their borders: an object read twice counts once.
    seen_ways: HashSet<i64>,
    seen_relations: HashSet<i64>,
}

impl Builder {
    pub fn new(car: Car) -> Self {
        Self {
            car,
            ways: Vec::new(),
            restrictions: Vec::new(),
            uses: HashMap::new(),
            coords: HashMap::new(),
            signals: HashSet::new(),
            seen_ways: HashSet::new(),
            seen_relations: HashSet::new(),
        }
    }

    /// First pass: ways and relations (nodes are ignored).
    pub fn first_pass(&mut self, element: &Element) {
        match element {
            Element::Way { id, refs, tags } => {
                let Some(way) = self.car.way(tags) else { return };
                if refs.len() < 2 || !self.seen_ways.insert(*id) {
                    return;
                }
                for node in refs {
                    *self.uses.entry(*node).or_default() += 1;
                }
                self.ways.push(UsableWay { id: *id, refs: refs.clone(), speed_kmh: way.speed_kmh, direction: way.direction, ferry: way.ferry, duration_s: way.duration_s });
            }
            Element::Relation { id, members, tags } => {
                if tag(tags, "type") != Some("restriction") || !self.seen_relations.insert(*id) {
                    return;
                }
                let Some(kind) = tag(tags, "restriction").and_then(restriction_kind) else { return };
                let member = |role: &str, kind: MemberKind| members.iter().find(|m| m.role == role && m.kind == kind).map(|m| m.id);
                let (Some(from_way), Some(via_node), Some(to_way)) = (member("from", MemberKind::Way), member("via", MemberKind::Node), member("to", MemberKind::Way)) else {
                    return;
                };
                self.restrictions.push(RawRestriction { from_way, via_node, to_way, kind });
            }
            Element::Node { .. } => {}
        }
    }

    /// Second pass: the nodes the ways use.
    pub fn second_pass(&mut self, element: &Element) {
        if let Element::Node { id, lat, lon, tags } = element
            && self.uses.contains_key(id)
        {
            self.coords.insert(*id, (*lat, *lon));
            if tag(tags, "highway") == Some("traffic_signals") {
                self.signals.insert(*id);
            }
        }
    }

    pub fn finish(self) -> Graph {
        let via_nodes: HashSet<i64> = self.restrictions.iter().map(|r| r.via_node).collect();
        let mut graph = Graph::default();
        let mut index: HashMap<i64, u32> = HashMap::new();
        let signals = &self.signals;
        let mut node =|graph: &mut Graph, osm_id: i64, (lat, lon): (f64, f64)| -> u32 {
            *index.entry(osm_id).or_insert_with(|| {
                graph.nodes.push(Node { osm_id, lat, lon, signal: signals.contains(&osm_id) });
                (graph.nodes.len() - 1) as u32
            })
        };

        for way in &self.ways {
            // A way that says how long it lasts shares that out among its edges by length.
            let way_m: f64 = way.refs.windows(2).filter_map(|w| Some(haversine_m(*self.coords.get(&w[0])?, *self.coords.get(&w[1])?))).sum();
            // A node outside the extract cuts the way: what is on each side stays a way of its own.
            let mut run: Vec<(i64, (f64, f64))> = Vec::new();
            let runs = way.refs.split(|r| !self.coords.contains_key(r)).filter(|part| part.len() >= 2);
            for part in runs {
                run.clear();
                run.extend(part.iter().map(|r| (*r, self.coords[r])));
                let mut start = 0;
                for i in 1..run.len() {
                    let (id, _) = run[i];
                    let is_cut = i == run.len() - 1 || self.uses[&id] > 1 || via_nodes.contains(&id);
                    if !is_cut {
                        continue;
                    }
                    let points = &run[start..=i];
                    let shape_start = graph.shapes.len() as u32;
                    graph.shapes.extend(points.iter().map(|(_, (lat, lon))| [*lat, *lon]));
                    let length_m = points.windows(2).map(|w| haversine_m(w[0].1, w[1].1)).sum::<f64>() as f32;
                    let seconds = match way.duration_s {
                        Some(whole) if way_m > 0.0 => whole * (length_m as f64 / way_m) as f32,
                        _ => length_m / (way.speed_kmh / 3.6),
                    };
                    let (a, b) = (node(&mut graph, points[0].0, points[0].1), node(&mut graph, id, run[i].1));
                    let edge = |from, to, reversed| Edge { from, to, way: way.id, length_m, seconds, shape_start, shape_len: points.len() as u32, reversed, ferry: way.ferry };
                    if matches!(way.direction, Direction::Both | Direction::Forward) {
                        graph.edges.push(edge(a, b, false));
                    }
                    if matches!(way.direction, Direction::Both | Direction::Backward) {
                        graph.edges.push(edge(b, a, true));
                    }
                    start = i;
                }
            }
        }

        graph.restrictions = self
            .restrictions
            .iter()
            .filter_map(|r| Some(Restriction { from_way: r.from_way, via: *index.get(&r.via_node)?, to_way: r.to_way, kind: r.kind }))
            .collect();
        graph
    }
}

fn restriction_kind(value: &str) -> Option<RestrictionKind> {
    if value.starts_with("no_") {
        Some(RestrictionKind::Prohibited)
    } else if value.starts_with("only_") {
        Some(RestrictionKind::Mandatory)
    } else {
        None
    }
}

pub fn haversine_m((lat1, lon1): (f64, f64), (lat2, lon2): (f64, f64)) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let a = ((p2 - p1) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * ((lon2 - lon1).to_radians() / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{Member, Tags};

    const CLASSES: &str = "[free_flow_kmh]\nprimary = 90\nresidential = 36\n";

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn node(id: i64, lat: f64, lon: f64) -> Element {
        Element::Node { id, lat, lon, tags: Vec::new() }
    }

    #[test]
    fn a_ferry_lasts_what_it_says_and_no_point_lands_on_it() {
        let road = |id, refs: &[i64]| Element::Way { id, refs: refs.to_vec(), tags: tags(&[("highway", "residential")]) };
        let elements = [
            node(1, 0.0, -0.01),
            node(2, 0.0, 0.0),
            node(3, 0.0, 0.1),
            node(4, 0.0, 0.2),
            node(5, 0.0, 0.21),
            node(6, 0.01, 0.1),
            road(10, &[1, 2]),
            Element::Way { id: 20, refs: vec![2, 3, 4], tags: tags(&[("route", "ferry"), ("duration", "1:00")]) },
            road(11, &[4, 5]),
            road(12, &[3, 6]),
        ];
        let mut builder = Builder::new(Car::from_road_classes(CLASSES).unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        let graph = builder.finish();
        let ferries: Vec<&Edge> = graph.edges.iter().filter(|e| e.ferry).collect();
        assert_eq!(ferries.len(), 4, "two halves, each way");
        assert!(ferries.iter().all(|e| (e.seconds - 1800.0).abs() < 1.0), "{ferries:?}");
        let allowed = graph.without_ferries(vec![true; graph.edges.len()]);
        assert_eq!(allowed.iter().filter(|a| **a).count(), graph.edges.len() - 4);
        assert!(graph.edges.iter().zip(&allowed).all(|(e, a)| e.ferry != *a));
    }

    fn signal(id: i64, lat: f64, lon: f64) -> Element {
        Element::Node { id, lat, lon, tags: tags(&[("highway", "traffic_signals")]) }
    }

    fn way(id: i64, refs: &[i64], pairs: &[(&str, &str)]) -> Element {
        Element::Way { id, refs: refs.to_vec(), tags: tags(pairs) }
    }

    fn build(elements: &[Element]) -> Graph {
        let mut builder = Builder::new(Car::from_road_classes(CLASSES).unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        builder.finish()
    }

    /// Nodes 1-2-3 on a line along the equator, 0.001° apart (about 111 m), and 3-4 going north.
    fn t_junction() -> Vec<Element> {
        vec![
            node(1, 0.0, 0.000),
            node(2, 0.0, 0.001),
            node(3, 0.0, 0.002),
            node(4, 0.001, 0.002),
            way(10, &[1, 2, 3], &[("highway", "residential")]),
            way(11, &[3, 4], &[("highway", "primary"), ("oneway", "yes")]),
        ]
    }

    #[test]
    fn a_way_is_cut_only_where_roads_meet_or_end() {
        let graph = build(&t_junction());
        // Nodes 1, 3 and 4: node 2 is only a point of the shape of the first way.
        let ids: Vec<i64> = graph.nodes.iter().map(|n| n.osm_id).collect();
        assert_eq!(ids.len(), 3);
        assert!(!ids.contains(&2));
        // Way 10, both ways, and way 11, one way.
        assert_eq!(graph.edges.len(), 3);
        let long = graph.edges.iter().find(|e| e.way == 10 && !e.reversed).unwrap();
        assert_eq!(long.shape_len, 3);
    }

    #[test]
    fn lengths_times_and_shapes() {
        let graph = build(&t_junction());
        let forward = graph.edges.iter().find(|e| e.way == 10 && !e.reversed).unwrap();
        let backward = graph.edges.iter().find(|e| e.way == 10 && e.reversed).unwrap();
        // 0.002° of longitude on the equator is 222.4 m, at 36 km/h (10 m/s).
        assert!((forward.length_m - 222.4).abs() < 0.5, "{}", forward.length_m);
        assert!((forward.seconds - 22.24).abs() < 0.05, "{}", forward.seconds);
        let from_to: Vec<[f64; 2]> = graph.shape(forward).collect();
        let back: Vec<[f64; 2]> = graph.shape(backward).collect();
        assert_eq!(from_to.first(), Some(&[0.0, 0.0]));
        assert_eq!(from_to.last(), Some(&[0.0, 0.002]));
        assert_eq!(back, from_to.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(graph.nodes[forward.from as usize].osm_id, 1);
        assert_eq!(graph.nodes[backward.from as usize].osm_id, 3);
    }

    #[test]
    fn one_way_streets_have_one_edge() {
        let graph = build(&t_junction());
        let one_way: Vec<&Edge> = graph.edges.iter().filter(|e| e.way == 11).collect();
        assert_eq!(one_way.len(), 1);
        assert_eq!(graph.nodes[one_way[0].from as usize].osm_id, 3);
        assert_eq!(graph.nodes[one_way[0].to as usize].osm_id, 4);
    }

    #[test]
    fn ways_a_car_cannot_use_leave_no_trace() {
        let mut elements = t_junction();
        elements.push(node(5, 0.002, 0.002));
        elements.push(way(12, &[4, 5], &[("highway", "footway")]));
        elements.push(way(13, &[3, 5], &[("highway", "residential"), ("access", "private")]));
        let graph = build(&elements);
        assert_eq!(graph.edges.len(), 3);
        assert!(graph.nodes.iter().all(|n| n.osm_id != 5));
    }

    #[test]
    fn a_missing_node_cuts_the_way() {
        let elements = vec![
            node(1, 0.0, 0.000),
            node(2, 0.0, 0.001),
            // Node 3 is outside the extract.
            node(4, 0.0, 0.003),
            node(5, 0.0, 0.004),
            way(10, &[1, 2, 3, 4, 5], &[("highway", "residential"), ("oneway", "yes")]),
        ];
        let graph = build(&elements);
        assert_eq!(graph.edges.len(), 2);
        let ends: Vec<(i64, i64)> = graph.edges.iter().map(|e| (graph.nodes[e.from as usize].osm_id, graph.nodes[e.to as usize].osm_id)).collect();
        assert_eq!(ends, vec![(1, 2), (4, 5)]);
    }

    #[test]
    fn a_loop_keeps_its_closing_node() {
        let elements = vec![
            node(1, 0.0, 0.000),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.001),
            way(10, &[1, 2, 3, 1], &[("highway", "residential"), ("oneway", "yes")]),
        ];
        let graph = build(&elements);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, graph.edges[0].to);
    }

    #[test]
    fn a_via_node_of_a_restriction_is_always_a_node() {
        let mut elements = vec![node(1, 0.0, 0.000), node(2, 0.0, 0.001), node(3, 0.0, 0.002), way(10, &[1, 2, 3], &[("highway", "residential")])];
        elements.push(Element::Relation {
            id: 100,
            members: vec![
                Member { kind: MemberKind::Way, id: 10, role: "from".into() },
                Member { kind: MemberKind::Node, id: 2, role: "via".into() },
                Member { kind: MemberKind::Way, id: 10, role: "to".into() },
            ],
            tags: tags(&[("type", "restriction"), ("restriction", "no_u_turn")]),
        });
        let graph = build(&elements);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.restrictions.len(), 1);
        let r = graph.restrictions[0];
        assert_eq!((graph.nodes[r.via as usize].osm_id, r.from_way, r.to_way, r.kind), (2, 10, 10, RestrictionKind::Prohibited));
    }

    #[test]
    fn restrictions_by_kind_and_the_ones_not_read() {
        let relation = |restriction: &str, via_kind| Element::Relation {
            id: 100,
            members: vec![
                Member { kind: MemberKind::Way, id: 10, role: "from".into() },
                Member { kind: via_kind, id: 2, role: "via".into() },
                Member { kind: MemberKind::Way, id: 11, role: "to".into() },
            ],
            tags: tags(&[("type", "restriction"), ("restriction", restriction)]),
        };
        let base = || vec![node(1, 0.0, 0.0), node(2, 0.0, 0.001), node(3, 0.001, 0.001), way(10, &[1, 2], &[("highway", "residential")]), way(11, &[2, 3], &[("highway", "residential")])];
        let with = |relation| {
            let mut elements = base();
            elements.push(relation);
            build(&elements).restrictions
        };
        assert_eq!(with(relation("only_straight_on", MemberKind::Node))[0].kind, RestrictionKind::Mandatory);
        assert_eq!(with(relation("no_left_turn", MemberKind::Node))[0].kind, RestrictionKind::Prohibited);
        // Through a way, and a value that is neither `no_*` nor `only_*`.
        assert!(with(relation("no_left_turn", MemberKind::Way)).is_empty());
        assert!(with(relation("something_else", MemberKind::Node)).is_empty());
    }

    #[test]
    fn traffic_signals_mark_their_node() {
        let mut elements = t_junction();
        elements[2] = signal(3, 0.0, 0.002);
        let graph = build(&elements);
        let signals: Vec<i64> = graph.nodes.iter().filter(|n| n.signal).map(|n| n.osm_id).collect();
        assert_eq!(signals, vec![3]);
    }

    #[test]
    fn objects_read_twice_count_once() {
        let once = build(&t_junction());
        let twice = build(&[t_junction(), t_junction()].concat());
        assert_eq!(once, twice);
    }

    #[test]
    fn haversine_known_distance() {
        // One degree of latitude is about 111.2 km.
        assert!((haversine_m((0.0, 0.0), (1.0, 0.0)) - 111_195.0).abs() < 50.0);
        assert_eq!(haversine_m((40.0, -3.0), (40.0, -3.0)), 0.0);
    }
}
