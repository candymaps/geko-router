//! Traffic on the turn graph: how much slower than free flow each edge is right now. The sources
//! (a historical profile of each hour, the sensors of a city, the incidents of a road authority)
//! all end up as one number per edge, the share of its free-flow speed that a car gets, and the
//! weights of the graph follow from it. The structure of the graph does not change, so the
//! hierarchy of queries only has to be customized again (`Cch::customize`), never rebuilt.

use crate::cch::INF;
use crate::turn::TurnGraph;

/// Free flow, in thousandths of the free-flow speed.
pub const FREE_FLOW: u16 = 1000;
/// The slowest an edge may be taken to go, a twentieth of its free-flow speed: below that a
/// reading is more likely a broken sensor than a road that has stopped, and a closed road is an
/// incident, not a speed.
pub const SLOWEST: u16 = 50;
/// A closed road: no route goes through it.
pub const CLOSED: u16 = 0;

/// Where the speed of an edge comes from: what `traffic.coverage` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Known {
    /// Nothing is known of it: the speed of its road class.
    #[default]
    Nothing,
    Historical,
    Live,
}

/// The speed of each edge of the road graph, in thousandths of its free-flow speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traffic {
    speed: Vec<u16>,
    /// What is known of each edge, free ones included: a road known to be clear is not a road nothing
    /// is known of.
    known: Vec<Known>,
}

impl Traffic {
    /// No traffic: every one of `edges` at free flow.
    pub fn free(edges: usize) -> Self {
        Self { speed: vec![FREE_FLOW; edges], known: vec![Known::Nothing; edges] }
    }

    pub fn edges(&self) -> usize {
        self.speed.len()
    }

    /// The speed of `edge`, in thousandths of free flow.
    pub fn speed(&self, edge: u32) -> u16 {
        self.speed[edge as usize]
    }

    /// Sets the speed of `edge` from a live source, kept between `SLOWEST` and free flow, or closes it
    /// with `CLOSED`: traffic only slows a road down, and what a free road can do is what the profile
    /// of its class already says.
    pub fn set(&mut self, edge: u32, thousandths: u16) {
        self.put(edge, thousandths, Known::Live);
    }

    /// Sets what is usual on `edge` at this time, for as long as no live source says otherwise.
    pub fn set_historical(&mut self, edge: u32, thousandths: u16) {
        self.put(edge, thousandths, Known::Historical);
    }

    fn put(&mut self, edge: u32, thousandths: u16, known: Known) {
        self.speed[edge as usize] = if thousandths == CLOSED { CLOSED } else { thousandths.clamp(SLOWEST, FREE_FLOW) };
        self.known[edge as usize] = known;
    }

    /// What is known of each edge.
    pub fn known(&self) -> &[Known] {
        &self.known
    }

    pub fn is_free(&self) -> bool {
        self.speed.iter().all(|s| *s == FREE_FLOW)
    }
}

impl TurnGraph {
    /// The same graph with the duration of every edge as `traffic` says, the turns costing what they
    /// cost: the weight of an arc is the duration of the edge it leaves plus its turn, so the turn
    /// is what is left of it when the free-flow duration is taken away.
    ///
    /// `self` must be the graph at free flow, which is why this makes a new one and does not change
    /// `self`: calling it again with other traffic never compounds.
    pub fn with_traffic(&self, traffic: &Traffic) -> TurnGraph {
        assert_eq!(traffic.edges(), self.edges(), "the traffic is of another graph");
        let edge_ds: Vec<u32> = self.edge_ds.iter().zip(&traffic.speed).map(|(ds, speed)| slowed(*ds, *speed)).collect();
        let closed = |edge: usize| traffic.speed[edge] == CLOSED;
        let mut arcs = self.arcs.clone();
        for (tail, window) in self.first_arc.windows(2).enumerate() {
            for arc in &mut arcs[window[0] as usize..window[1] as usize] {
                // Neither in nor out: `INF` is no arc at all for the hierarchy.
                arc.weight_ds = if closed(tail) || closed(arc.head as usize) { INF } else { arc.weight_ds - self.edge_ds[tail] + edge_ds[tail] };
            }
        }
        TurnGraph { edge_ds, first_arc: self.first_arc.clone(), arcs }
    }
}

/// A duration at a share of the speed: at half the speed, twice as long. Never less than it was.
fn slowed(ds: u32, thousandths: u16) -> u32 {
    let speed = thousandths.clamp(SLOWEST, FREE_FLOW) as u64;
    ((ds as u64 * FREE_FLOW as u64).div_ceil(speed)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cch::{Cch, Scratch};
    use crate::turn::Arc;

    /// 0 -> 1 -> 3 and 0 -> 2 -> 3, the way over 1 faster: each edge lasts 10 s, and the turn from 0 to 2
    /// costs 2 s more than the one from 0 to 1.
    fn square() -> TurnGraph {
        // Nodes of this graph are edges: 0, 1, 2, 3.
        let arcs = |list: &[(u32, u32)]| list.iter().map(|(head, weight_ds)| Arc { head: *head, weight_ds: *weight_ds }).collect::<Vec<_>>();
        let lists = [arcs(&[(1, 100), (2, 120)]), arcs(&[(3, 100)]), arcs(&[(3, 120)]), arcs(&[])];
        let mut graph = TurnGraph { edge_ds: vec![100; 4], ..Default::default() };
        for list in lists {
            graph.first_arc.push(graph.arcs.len() as u32);
            graph.arcs.extend(list);
        }
        graph.first_arc.push(graph.arcs.len() as u32);
        graph
    }

    fn distance(graph: &TurnGraph, from: u32, to: u32) -> u32 {
        let cch = Cch::new(graph);
        let mut scratch = Scratch::new(&cch);
        cch.query(&mut scratch, from, to).expect("a path").0
    }

    #[test]
    fn slower_is_longer_and_never_shorter() {
        assert_eq!(slowed(100, 1000), 100);
        assert_eq!(slowed(100, 500), 200);
        assert_eq!(slowed(100, 333), 301);
        assert_eq!(slowed(100, 1), slowed(100, SLOWEST));
        assert_eq!(slowed(100, 5000), 100);
        assert_eq!(slowed(0, 500), 0);
    }

    #[test]
    fn free_flow_changes_nothing() {
        let graph = square();
        let traffic = Traffic::free(graph.edges());
        assert!(traffic.is_free());
        assert_eq!(graph.with_traffic(&traffic), graph);
    }

    #[test]
    fn traffic_slows_the_edge_and_not_its_turns() {
        let graph = square();
        let mut traffic = Traffic::free(graph.edges());
        // Edge 1 at half speed: it lasts 20 s; the turns stay what they were.
        traffic.set(1, 500);
        let slow = graph.with_traffic(&traffic);
        assert_eq!(slow.edge_ds, vec![100, 200, 100, 100]);
        // 0 -> 1 leaves edge 0 (free) plus the turn of 0 ds; 1 -> 3 leaves edge 1, now 200, plus 0.
        assert_eq!(slow.arcs_from(0)[0].weight_ds, 100);
        assert_eq!(slow.arcs_from(1)[0].weight_ds, 200);
        assert_eq!(slow.arcs_from(2)[0].weight_ds, 120);
    }

    #[test]
    fn the_way_round_wins_when_traffic_makes_the_short_way_slow() {
        let graph = square();
        assert_eq!(distance(&graph, 0, 3), 200, "over 1 at free flow");
        let mut traffic = Traffic::free(graph.edges());
        traffic.set(1, 500);
        assert_eq!(distance(&graph.with_traffic(&traffic), 0, 3), 240, "over 2 once 1 is slow");
    }

    #[test]
    fn asking_again_does_not_compound() {
        let graph = square();
        let mut traffic = Traffic::free(graph.edges());
        traffic.set(1, 500);
        let once = graph.with_traffic(&traffic);
        assert_eq!(graph.with_traffic(&traffic), once);
        assert_eq!(graph.with_traffic(&Traffic::free(graph.edges())), graph);
    }

    #[test]
    fn a_hierarchy_customized_again_follows_the_traffic() {
        let graph = square();
        let mut cch = Cch::new(&graph);
        let mut scratch = Scratch::new(&cch);
        assert_eq!(cch.query(&mut scratch, 0, 3).map(|r| r.0), Some(200));
        let mut traffic = Traffic::free(graph.edges());
        traffic.set(1, 500);
        cch.customize(&graph.with_traffic(&traffic));
        assert_eq!(cch.query(&mut scratch, 0, 3).map(|r| r.0), Some(240));
        cch.customize(&graph);
        assert_eq!(cch.query(&mut scratch, 0, 3).map(|r| r.0), Some(200));
    }

    #[test]
    fn speeds_stay_between_the_slowest_and_free_flow() {
        let mut traffic = Traffic::free(2);
        traffic.set(0, 1);
        traffic.set(1, 4000);
        assert_eq!((traffic.speed(0), traffic.speed(1)), (SLOWEST, FREE_FLOW));
    }

    #[test]
    fn no_route_goes_through_a_closed_road() {
        let graph = square();
        let mut traffic = Traffic::free(graph.edges());
        traffic.set(1, CLOSED);
        assert_eq!(traffic.speed(1), CLOSED);
        let closed = graph.with_traffic(&traffic);
        assert_eq!(distance(&closed, 0, 3), 240, "round it, over 2");
        traffic.set(2, CLOSED);
        let cch = {
            let mut cch = Cch::new(&graph);
            cch.customize(&graph.with_traffic(&traffic));
            cch
        };
        assert_eq!(cch.query(&mut Scratch::new(&cch), 0, 3), None, "both ways closed");
    }

    #[test]
    fn a_clear_road_is_measured_and_still_free() {
        let mut traffic = Traffic::free(2);
        assert_eq!(traffic.known(), [Known::Nothing, Known::Nothing]);
        traffic.set(1, FREE_FLOW);
        traffic.set_historical(0, FREE_FLOW);
        assert_eq!(traffic.known(), [Known::Historical, Known::Live]);
        assert!(traffic.is_free());
    }
}
