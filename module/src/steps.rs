//! The steps of a route, to say it in words: where it departs, each turn or change of street, and
//! where it arrives. They come from the edges the route goes along: a turn is read from the angle
//! between the edge that arrives and the one that leaves, and a change of street from their names.

use crate::graph::{Graph, NO_NAME};
use crate::route::Route;
use crate::turn::{TurnGraph, bearings, turn_angle};

/// Under this angle, in degrees, a junction is going straight on.
const STRAIGHT_DEG: f32 = 20.0;
const SLIGHT_DEG: f32 = 45.0;
const SHARP_DEG: f32 = 135.0;
const U_TURN_DEG: f32 = 170.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// `depart`, `turn`, `new name` (the street changes and the road goes on) or `arrive`.
    pub maneuver: &'static str,
    /// For a `turn` or a `new name`: `slight left`, `left`, `sharp left`, `straight`, `slight right`,
    /// `right`, `sharp right` or `uturn`.
    pub modifier: Option<&'static str>,
    /// The street the step goes along, when it has a name (or a reference, like `Ma-13`).
    pub name: Option<String>,
    pub distance_m: f64,
    pub duration_s: f64,
    /// `[lat, lon]` where the step starts.
    pub location: [f64; 2],
}

fn modifier(angle: f32) -> &'static str {
    let size = angle.abs();
    if size >= U_TURN_DEG {
        "uturn"
    } else if size < STRAIGHT_DEG {
        "straight"
    } else {
        match (angle > 0.0, size < SLIGHT_DEG, size >= SHARP_DEG) {
            (true, true, _) => "slight right",
            (true, _, true) => "sharp right",
            (true, ..) => "right",
            (false, true, _) => "slight left",
            (false, _, true) => "sharp left",
            (false, ..) => "left",
        }
    }
}

/// The steps of `route`, which go from its first point to its last: distances and durations add up to
/// those of the route.
pub fn steps(graph: &Graph, turns: &TurnGraph, route: &Route) -> Vec<Step> {
    let edges = &route.edges;
    let (Some(&first), Some(&last)) = (edges.first(), edges.last()) else { return Vec::new() };
    let name = |e: u32| graph.name(&graph.edges[e as usize]).map(str::to_owned);
    let (start, end) = (route.geometry.first().copied().unwrap_or_default(), route.geometry.last().copied().unwrap_or_default());

    // What each edge adds to the route: the part of its length that is on it, and the time from entering
    // it to entering the next, with the turn at its end; the last adds the part before the point.
    let ds = |e: u32| f64::from(turns.edge_ds[e as usize]);
    let part_of = |i: usize| -> (f64, f64) {
        let e = edges[i];
        let length = f64::from(graph.edges[e as usize].length_m);
        let (from, to) = (if i == 0 { route.start_fraction } else { 0.0 }, if i == edges.len() - 1 { route.end_fraction } else { 1.0 });
        let own = (to - from).max(0.0);
        let time = if i + 1 < edges.len() {
            let next = edges[i + 1];
            let arc = turns.arcs_from(e).iter().find(|a| a.head == next).map_or(ds(e), |a| f64::from(a.weight_ds));
            arc - from * ds(e)
        } else {
            own * ds(e)
        };
        (length * own, time / 10.0)
    };

    let mut out = vec![Step { maneuver: "depart", modifier: None, name: name(first), distance_m: 0.0, duration_s: 0.0, location: start }];
    for i in 0..edges.len() {
        if i > 0 {
            let (previous, current) = (&graph.edges[edges[i - 1] as usize], &graph.edges[edges[i] as usize]);
            let angle = turn_angle(bearings(graph, previous).1, bearings(graph, current).0);
            let renamed = previous.name != current.name && !(previous.name == NO_NAME && current.name == NO_NAME);
            let choice = turns.arcs_from(edges[i - 1]).len() > 1;
            let at = graph.nodes[current.from as usize];
            let location = [at.lat, at.lon];
            if angle.abs() >= STRAIGHT_DEG && (choice || renamed) {
                out.push(Step { maneuver: "turn", modifier: Some(modifier(angle)), name: name(edges[i]), distance_m: 0.0, duration_s: 0.0, location });
            } else if renamed {
                out.push(Step { maneuver: "new name", modifier: Some("straight"), name: name(edges[i]), distance_m: 0.0, duration_s: 0.0, location });
            }
        }
        let (distance, time) = part_of(i);
        let step = out.last_mut().expect("a step to add to");
        step.distance_m += distance;
        step.duration_s += time;
    }
    out.push(Step { maneuver: "arrive", modifier: None, name: name(last), distance_m: 0.0, duration_s: 0.0, location: end });
    out
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

    fn way(id: i64, refs: &[i64], name: &str) -> Element {
        Element::Way { id, refs: refs.to_vec(), tags: tags(&[("highway", "residential"), ("name", name)]) }
    }

    /// Calle Mayor goes east along latitude 0 from node 1 to node 3; Calle Sol leaves node 3 to the
    /// south (a right turn) to node 4, and Calle Luna goes on east from node 3 to node 5.
    fn town() -> (Graph, TurnGraph) {
        let elements = [
            node(1, 0.0, 0.000),
            node(2, 0.0, 0.002),
            node(3, 0.0, 0.004),
            node(4, -0.002, 0.004),
            node(5, 0.0, 0.006),
            way(10, &[1, 2, 3], "Calle Mayor"),
            way(11, &[3, 4], "Calle Sol"),
            way(12, &[3, 5], "Calle Luna"),
        ];
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]
residential = 36
").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        let graph = builder.finish();
        let turns = turn::build(&graph, &TurnProfile::default());
        (graph, turns)
    }

    fn route_along(graph: &Graph, turns: &TurnGraph, from: (f64, f64), to: (f64, f64)) -> Route {
        let snapper = Snapper::new(graph, vec![true; graph.edges.len()]);
        let (a, b) = (snapper.snap(graph, from.0, from.1, 50.0), snapper.snap(graph, to.0, to.1, 50.0));
        crate::route::route(graph, turns, &a, &b).expect("a route")
    }

    #[test]
    fn a_right_turn_into_another_street_is_a_step() {
        let (graph, turns) = town();
        let route = route_along(&graph, &turns, (0.0, 0.0005), (-0.0015, 0.004));
        let steps = steps(&graph, &turns, &route);
        let kinds: Vec<_> = steps.iter().map(|s| (s.maneuver, s.modifier, s.name.as_deref())).collect();
        assert_eq!(kinds, [("depart", None, Some("Calle Mayor")), ("turn", Some("right"), Some("Calle Sol")), ("arrive", None, Some("Calle Sol"))]);
        let (distance, time) = steps.iter().fold((0.0, 0.0), |sum, s| (sum.0 + s.distance_m, sum.1 + s.duration_s));
        assert!((distance - route.distance_m).abs() < 1.0, "{distance} vs {}", route.distance_m);
        assert!((time - route.duration_s).abs() < 0.2, "{time} vs {}", route.duration_s);
    }

    #[test]
    fn going_straight_on_into_another_street_is_a_new_name() {
        let (graph, turns) = town();
        let route = route_along(&graph, &turns, (0.0, 0.0005), (0.0, 0.0055));
        let kinds: Vec<_> = steps(&graph, &turns, &route).iter().map(|s| (s.maneuver, s.name.clone())).collect();
        assert_eq!(kinds, [("depart", Some("Calle Mayor".to_owned())), ("new name", Some("Calle Luna".to_owned())), ("arrive", Some("Calle Luna".to_owned()))]);
    }

    #[test]
    fn a_street_without_a_junction_is_one_step() {
        let (graph, turns) = town();
        let route = route_along(&graph, &turns, (0.0, 0.0005), (0.0, 0.0035));
        let steps = steps(&graph, &turns, &route);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].name.as_deref(), Some("Calle Mayor"));
    }

    #[test]
    fn angles_are_named_by_their_size_and_side() {
        assert_eq!(modifier(10.0), "straight");
        assert_eq!(modifier(30.0), "slight right");
        assert_eq!(modifier(90.0), "right");
        assert_eq!(modifier(150.0), "sharp right");
        assert_eq!(modifier(-30.0), "slight left");
        assert_eq!(modifier(-90.0), "left");
        assert_eq!(modifier(-150.0), "sharp left");
        assert_eq!(modifier(179.0), "uturn");
    }
}
