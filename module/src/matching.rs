//! Map matching: the roads a trace of points went along, by a hidden Markov model as in Newson and
//! Krumm. Each point may be on a few streets near it. A street is likelier the closer it is, and a
//! step between two of them the closer its route is to the line between the points. Where no route
//! joins two points, a matching ends and another begins.

use crate::cch::{Cch, Scratch};
use crate::graph::{Graph, haversine_m};
use crate::route::{self, Route};
use crate::snap::Snap;
use crate::turn::TurnGraph;

/// How far off the road a GPS point usually is, in metres.
const SIGMA_M: f64 = 10.0;
/// How much longer than the line a route usually is, in metres.
const BETA_M: f64 = 50.0;
/// A route longer than the line by this many times, plus `MAX_EXTRA_M`, does not join two points.
const MAX_DETOUR: f64 = 4.0;
const MAX_EXTRA_M: f64 = 1000.0;

/// Roads that follow a stretch of the trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Matching {
    /// The points of the trace it covers, by their index.
    pub points: Vec<usize>,
    /// Where each of those points lies on the road.
    pub snapped: Vec<[f64; 2]>,
    pub duration_s: f64,
    pub distance_m: f64,
    pub geometry: Vec<[f64; 2]>,
    pub edges: Vec<u32>,
}

/// A street a point may be on, with the best way there from the point before.
struct State {
    score: f64,
    back: Option<usize>,
    leg: Option<Route>,
}

fn emission(distance_m: f64) -> f64 {
    -0.5 * (distance_m / SIGMA_M).powi(2)
}

fn line(a: [f64; 2], b: [f64; 2]) -> f64 {
    haversine_m((a[0], a[1]), (b[0], b[1]))
}

/// A point of the trace, with what it may be.
struct Step<'a> {
    point: usize,
    streets: &'a [Vec<Snap>],
    states: Vec<State>,
}

/// The matching of `steps`, walking back from the best state of the last.
fn walk(steps: &[Step]) -> Option<Matching> {
    let last = &steps.last()?.states;
    let mut at = (0..last.len()).max_by(|a, b| last[*a].score.total_cmp(&last[*b].score))?;
    let mut chosen = vec![0; steps.len()];
    for (i, step) in steps.iter().enumerate().rev() {
        chosen[i] = at;
        at = step.states[at].back.unwrap_or(0);
    }
    let mut matching = Matching { points: Vec::new(), snapped: Vec::new(), duration_s: 0.0, distance_m: 0.0, geometry: Vec::new(), edges: Vec::new() };
    for (i, step) in steps.iter().enumerate() {
        matching.points.push(step.point);
        matching.snapped.push(step.streets[chosen[i]][0].point);
        if let Some(leg) = &step.states[chosen[i]].leg {
            matching.duration_s += leg.duration_s;
            matching.distance_m += leg.distance_m;
            let skip = usize::from(matching.geometry.last() == leg.geometry.first());
            matching.geometry.extend_from_slice(&leg.geometry[skip.min(leg.geometry.len())..]);
            let skip = usize::from(matching.edges.last() == leg.edges.first());
            matching.edges.extend_from_slice(&leg.edges[skip.min(leg.edges.len())..]);
        }
    }
    Some(matching)
}

/// Matches a trace: its points, and for each the streets near it, closest first, each in the
/// directions it can be taken. A point with no street is left out, and so is one that has not moved
/// from the last beyond what GPS wanders: it would only look like going backwards.
pub fn trace(graph: &Graph, turns: &TurnGraph, cch: &Cch, scratch: &mut Scratch, coordinates: &[[f64; 2]], candidates: &[Vec<Vec<Snap>>]) -> Vec<Matching> {
    let mut matchings = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    for (point, streets) in candidates.iter().enumerate() {
        if streets.is_empty() || steps.last().is_some_and(|s| line(coordinates[s.point], coordinates[point]) < 2.0 * SIGMA_M) {
            continue;
        }
        let start = || streets.iter().map(|s| State { score: emission(s[0].distance_m), back: None, leg: None }).collect::<Vec<_>>();
        let Some(Step { streets: before, states: previous, .. }) = steps.last() else {
            steps.push(Step { point, streets, states: start() });
            continue;
        };
        let mut states: Vec<State> = Vec::with_capacity(streets.len());
        for street in streets {
            let mut best: Option<State> = None;
            for (index, (from, state)) in before.iter().zip(previous).enumerate() {
                if state.score == f64::NEG_INFINITY {
                    continue;
                }
                let straight = line(from[0].point, street[0].point);
                let Some(leg) = route::route_cch(graph, turns, cch, scratch, from, street) else { continue };
                if leg.distance_m > straight * MAX_DETOUR + MAX_EXTRA_M {
                    continue;
                }
                let score = state.score + emission(street[0].distance_m) - (leg.distance_m - straight).abs() / BETA_M;
                if best.as_ref().is_none_or(|b| score > b.score) {
                    best = Some(State { score, back: Some(index), leg: Some(leg) });
                }
            }
            states.push(best.unwrap_or(State { score: f64::NEG_INFINITY, back: None, leg: None }));
        }
        if states.iter().all(|s| s.score == f64::NEG_INFINITY) {
            matchings.extend(walk(&steps));
            steps.clear();
            steps.push(Step { point, streets, states: start() });
        } else {
            steps.push(Step { point, streets, states });
        }
    }
    matchings.extend(walk(&steps));
    matchings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closer_street_is_likelier() {
        assert!(emission(5.0) > emission(30.0));
        assert_eq!(emission(0.0), 0.0);
    }
}
