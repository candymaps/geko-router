//! Customizable Contraction Hierarchies on the turn graph (docs/ROUTER_PLAN.md of the host).
//!
//! Three phases, each with its own cost:
//!
//! 1. **Order and topology** (`Cch::new`), once per graph and independent of the weights: an order of
//!    the nodes, and the shortcuts that eliminating them in that order makes. The elimination tree
//!    (the parent of a node is its lowest neighbour above it) says where a query has to look.
//! 2. **Customization** (`Cch::customize`), each time the weights change: the weight of every
//!    shortcut, from the triangles it closes. This is what lets live traffic in without rebuilding.
//! 3. **Query** (`Cch::query`): both searches walk up the elimination tree from their node, relaxing
//!    the upward edges, and meet at the best common ancestor. No priority queue.
//!
//! Nodes are renumbered by rank, so that "above" is "greater" and an upward edge goes to a greater
//! number. The order here is minimum degree, which is simple and deterministic; a nested dissection
//! with real separators only changes `order`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::turn::TurnGraph;

const NONE: u32 = u32::MAX;
/// No path. Half of `u32::MAX`, so that adding two weights never overflows.
pub const INF: u32 = u32::MAX / 2;

pub struct Cch {
    /// The rank of each node of the turn graph.
    rank: Vec<u32>,
    /// The node of each rank.
    node: Vec<u32>,
    /// `up[first_up[r]..first_up[r + 1]]`: the neighbours of rank `r` above it, ascending.
    first_up: Vec<u32>,
    up: Vec<u32>,
    /// The lowest neighbour above each rank, `NONE` at the roots.
    parent: Vec<u32>,
    /// By upward edge: the weight going up and the weight coming down, and the node through which
    /// each is a shortcut (`NONE`: an arc of the graph).
    forward: Vec<u32>,
    backward: Vec<u32>,
    forward_via: Vec<u32>,
    backward_via: Vec<u32>,
}

/// What a query leaves for the next, per thread: the distances of the nodes it touched, stamped so
/// that none has to be cleared.
pub struct Scratch {
    stamp: u32,
    forward_stamp: Vec<u32>,
    backward_stamp: Vec<u32>,
    forward: Vec<u32>,
    backward: Vec<u32>,
    forward_pred: Vec<u32>,
    backward_pred: Vec<u32>,
}

impl Scratch {
    pub fn new(cch: &Cch) -> Self {
        let n = cch.rank.len();
        Self { stamp: 0, forward_stamp: vec![0; n], backward_stamp: vec![0; n], forward: vec![0; n], backward: vec![0; n], forward_pred: vec![NONE; n], backward_pred: vec![NONE; n] }
    }
}

/// The nodes in elimination order, and for each the neighbours above it when it was eliminated: the
/// edges of the graph plus the shortcuts that the elimination adds. Always the lowest degree
/// first, the lowest node on a tie.
fn minimum_degree(turns: &TurnGraph) -> (Vec<u32>, Vec<Vec<u32>>) {
    let n = turns.edges();
    let mut adjacent: Vec<Vec<u32>> = vec![Vec::new(); n];
    for a in 0..n as u32 {
        for arc in turns.arcs_from(a) {
            if arc.head != a {
                adjacent[a as usize].push(arc.head);
                adjacent[arc.head as usize].push(a);
            }
        }
    }
    for list in &mut adjacent {
        list.sort_unstable();
        list.dedup();
    }
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = (0..n as u32).map(|v| Reverse((adjacent[v as usize].len() as u32, v))).collect();
    let mut done = vec![false; n];
    let mut sequence = Vec::with_capacity(n);
    let mut above: Vec<Vec<u32>> = vec![Vec::new(); n];
    while let Some(Reverse((degree, v))) = heap.pop() {
        if done[v as usize] || adjacent[v as usize].len() as u32 != degree {
            continue;
        }
        done[v as usize] = true;
        sequence.push(v);
        let neighbours = std::mem::take(&mut adjacent[v as usize]);
        for &a in &neighbours {
            // Its neighbours become those it had, without `v`, and the ones `v` had, without itself.
            let old = std::mem::take(&mut adjacent[a as usize]);
            let mut merged = Vec::with_capacity(old.len() + neighbours.len());
            let (mut i, mut j) = (0, 0);
            while i < old.len() || j < neighbours.len() {
                let next = match (old.get(i), neighbours.get(j)) {
                    (Some(&x), Some(&y)) if x == y => {
                        i += 1;
                        j += 1;
                        x
                    }
                    (Some(&x), Some(&y)) if x < y => {
                        i += 1;
                        x
                    }
                    (Some(_), Some(&y)) => {
                        j += 1;
                        y
                    }
                    (Some(&x), None) => {
                        i += 1;
                        x
                    }
                    (None, Some(&y)) => {
                        j += 1;
                        y
                    }
                    (None, None) => unreachable!(),
                };
                if next != v && next != a {
                    merged.push(next);
                }
            }
            heap.push(Reverse((merged.len() as u32, a)));
            adjacent[a as usize] = merged;
        }
        above[v as usize] = neighbours;
    }
    (sequence, above)
}

impl Cch {
    /// The topology of `turns`; the weights are those of `customize`.
    pub fn new(turns: &TurnGraph) -> Self {
        let (node, above) = minimum_degree(turns);
        let n = node.len();
        let mut rank = vec![0u32; n];
        for (r, v) in node.iter().enumerate() {
            rank[*v as usize] = r as u32;
        }
        let mut first_up = Vec::with_capacity(n + 1);
        let mut up = Vec::new();
        let mut parent = Vec::with_capacity(n);
        for v in &node {
            first_up.push(up.len() as u32);
            let mut heads: Vec<u32> = above[*v as usize].iter().map(|x| rank[*x as usize]).collect();
            heads.sort_unstable();
            parent.push(heads.first().copied().unwrap_or(NONE));
            up.extend(heads);
        }
        first_up.push(up.len() as u32);
        let edges = up.len();
        let mut cch = Self { rank, node, first_up, up, parent, forward: vec![INF; edges], backward: vec![INF; edges], forward_via: vec![NONE; edges], backward_via: vec![NONE; edges] };
        cch.customize(turns);
        cch
    }

    pub fn nodes(&self) -> usize {
        self.rank.len()
    }

    /// The edges of the hierarchy: those of the graph and the shortcuts.
    pub fn edges(&self) -> usize {
        self.up.len()
    }

    /// The depth of the deepest branch of the elimination tree: the most nodes a query can walk.
    pub fn depth(&self) -> usize {
        let mut depth = vec![0u32; self.nodes()];
        let mut deepest = 0;
        for r in (0..self.nodes()).rev() {
            depth[r] = if self.parent[r] == NONE { 1 } else { depth[self.parent[r] as usize] + 1 };
            deepest = deepest.max(depth[r]);
        }
        deepest as usize
    }

    /// The upward edge from `low` to `high`, which must exist.
    fn find(&self, low: u32, high: u32) -> usize {
        let (start, end) = (self.first_up[low as usize] as usize, self.first_up[low as usize + 1] as usize);
        start + self.up[start..end].binary_search(&high).expect("a shortcut closes every triangle")
    }

    /// Sets the weight of every edge of the hierarchy from the weights of the arcs of `turns`, lowest
    /// node first: an edge is final once every triangle below it has been looked at.
    pub fn customize(&mut self, turns: &TurnGraph) {
        self.forward.fill(INF);
        self.backward.fill(INF);
        self.forward_via.fill(NONE);
        self.backward_via.fill(NONE);
        for a in 0..turns.edges() as u32 {
            for arc in turns.arcs_from(a) {
                if arc.head == a {
                    continue;
                }
                let (ra, rb) = (self.rank[a as usize], self.rank[arc.head as usize]);
                if ra < rb {
                    let e = self.find(ra, rb);
                    self.forward[e] = self.forward[e].min(arc.weight_ds);
                } else {
                    let e = self.find(rb, ra);
                    self.backward[e] = self.backward[e].min(arc.weight_ds);
                }
            }
        }
        for v in 0..self.nodes() {
            let (start, end) = (self.first_up[v] as usize, self.first_up[v + 1] as usize);
            for i in start..end {
                for j in i + 1..end {
                    let (u, w) = (self.up[i], self.up[j]);
                    let e = self.find(u, w);
                    // u -> v -> w, and w -> v -> u.
                    let (through, back) = (self.backward[i] + self.forward[j], self.backward[j] + self.forward[i]);
                    if through < self.forward[e] {
                        self.forward[e] = through;
                        self.forward_via[e] = v as u32;
                    }
                    if back < self.backward[e] {
                        self.backward[e] = back;
                        self.backward_via[e] = v as u32;
                    }
                }
            }
        }
    }

    /// The length of the shortest path from node `from` to node `to` of the turn graph, in the weights
    /// of its arcs, and the rank of the node where the two searches met.
    pub fn query(&self, scratch: &mut Scratch, from: u32, to: u32) -> Option<(u32, u32)> {
        let (s, t) = (self.rank[from as usize], self.rank[to as usize]);
        if s == t {
            scratch.forward_pred[s as usize] = NONE;
            scratch.backward_pred[s as usize] = NONE;
            return Some((0, s));
        }
        scratch.stamp = scratch.stamp.wrapping_add(1);
        if scratch.stamp == 0 {
            scratch.forward_stamp.fill(0);
            scratch.backward_stamp.fill(0);
            scratch.stamp = 1;
        }
        let stamp = scratch.stamp;
        scratch.forward_stamp[s as usize] = stamp;
        scratch.forward[s as usize] = 0;
        scratch.forward_pred[s as usize] = NONE;
        scratch.backward_stamp[t as usize] = stamp;
        scratch.backward[t as usize] = 0;
        scratch.backward_pred[t as usize] = NONE;

        let (mut a, mut b) = (s, t);
        let (mut best, mut meet) = (INF, NONE);
        while a != NONE || b != NONE {
            let v = a.min(b);
            let vi = v as usize;
            let reached_forward = scratch.forward_stamp[vi] == stamp;
            let reached_backward = scratch.backward_stamp[vi] == stamp;
            if reached_forward && reached_backward && scratch.forward[vi] + scratch.backward[vi] < best {
                best = scratch.forward[vi] + scratch.backward[vi];
                meet = v;
            }
            let (start, end) = (self.first_up[vi] as usize, self.first_up[vi + 1] as usize);
            if a == v && reached_forward && scratch.forward[vi] < best {
                for e in start..end {
                    let weight = self.forward[e];
                    if weight >= INF {
                        continue;
                    }
                    let (u, d) = (self.up[e] as usize, scratch.forward[vi] + weight);
                    if scratch.forward_stamp[u] != stamp || d < scratch.forward[u] {
                        scratch.forward_stamp[u] = stamp;
                        scratch.forward[u] = d;
                        scratch.forward_pred[u] = v;
                    }
                }
            }
            if b == v && reached_backward && scratch.backward[vi] < best {
                for e in start..end {
                    let weight = self.backward[e];
                    if weight >= INF {
                        continue;
                    }
                    let (u, d) = (self.up[e] as usize, scratch.backward[vi] + weight);
                    if scratch.backward_stamp[u] != stamp || d < scratch.backward[u] {
                        scratch.backward_stamp[u] = stamp;
                        scratch.backward[u] = d;
                        scratch.backward_pred[u] = v;
                    }
                }
            }
            if a == v {
                a = self.parent[vi];
            }
            if b == v {
                b = self.parent[vi];
            }
        }
        (best < INF).then_some((best, meet))
    }

    /// The nodes of the turn graph along the path the last `query` found, from `from` to `to`.
    pub fn path(&self, scratch: &Scratch, from: u32, meet: u32) -> Vec<u32> {
        let mut ranks = vec![self.rank[from as usize]];
        let mut climb = Vec::new();
        let mut x = meet;
        while x != NONE {
            climb.push(x);
            x = scratch.forward_pred[x as usize];
        }
        climb.reverse();
        for pair in climb.windows(2) {
            self.unpack(pair[0], pair[1], &mut ranks);
        }
        let mut x = meet;
        while scratch.backward_pred[x as usize] != NONE {
            let next = scratch.backward_pred[x as usize];
            self.unpack(x, next, &mut ranks);
            x = next;
        }
        ranks.into_iter().map(|r| self.node[r as usize]).collect()
    }

    /// Appends the nodes after `a` up to `b`, expanding the shortcuts between them into the arcs of the
    /// graph.
    fn unpack(&self, a: u32, b: u32, out: &mut Vec<u32>) {
        let mut stack = vec![(a, b)];
        while let Some((a, b)) = stack.pop() {
            let via = if a < b { self.forward_via[self.find(a, b)] } else { self.backward_via[self.find(b, a)] };
            if via == NONE {
                out.push(b);
            } else {
                stack.push((via, b));
                stack.push((a, via));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::Arc;

    /// A reproducible sequence of numbers.
    struct Random(u64);

    impl Random {
        fn next(&mut self, below: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % below
        }
    }

    fn random_graph(n: u32, arcs_per_node: u64, random: &mut Random) -> TurnGraph {
        let mut lists: Vec<Vec<Arc>> = vec![Vec::new(); n as usize];
        // A ring keeps most of it connected; the rest is random.
        for v in 0..n {
            lists[v as usize].push(Arc { head: (v + 1) % n, weight_ds: 1 + random.next(50) as u32 });
            for _ in 0..arcs_per_node {
                let head = random.next(n as u64) as u32;
                if head != v {
                    lists[v as usize].push(Arc { head, weight_ds: 1 + random.next(50) as u32 });
                }
            }
        }
        let mut graph = TurnGraph { edge_ds: vec![10; n as usize], ..Default::default() };
        for list in lists {
            graph.first_arc.push(graph.arcs.len() as u32);
            graph.arcs.extend(list);
        }
        graph.first_arc.push(graph.arcs.len() as u32);
        graph
    }

    fn dijkstra(graph: &TurnGraph, from: u32) -> Vec<u32> {
        let mut dist = vec![INF; graph.edges()];
        let mut heap = BinaryHeap::new();
        dist[from as usize] = 0;
        heap.push(Reverse((0u32, from)));
        while let Some(Reverse((d, v))) = heap.pop() {
            if d > dist[v as usize] {
                continue;
            }
            for arc in graph.arcs_from(v) {
                let nd = d + arc.weight_ds;
                if nd < dist[arc.head as usize] {
                    dist[arc.head as usize] = nd;
                    heap.push(Reverse((nd, arc.head)));
                }
            }
        }
        dist
    }

    /// Every pair, and the path of each, against Dijkstra.
    fn check(graph: &TurnGraph, cch: &Cch) {
        let mut scratch = Scratch::new(cch);
        for from in 0..graph.edges() as u32 {
            let expected = dijkstra(graph, from);
            for to in 0..graph.edges() as u32 {
                let got = cch.query(&mut scratch, from, to);
                if expected[to as usize] >= INF {
                    assert_eq!(got, None, "{from} -> {to}");
                    continue;
                }
                let (distance, meet) = got.unwrap_or_else(|| panic!("{from} -> {to} has a path"));
                assert_eq!(distance, expected[to as usize], "{from} -> {to}");
                let path = cch.path(&scratch, from, meet);
                assert_eq!((path.first(), path.last()), (Some(&from), Some(&to)));
                let length: u32 = path
                    .windows(2)
                    .map(|p| graph.arcs_from(p[0]).iter().filter(|a| a.head == p[1]).map(|a| a.weight_ds).min().unwrap_or_else(|| panic!("{} -> {} is not an arc", p[0], p[1])))
                    .sum();
                assert_eq!(length, distance, "the path of {from} -> {to}");
            }
        }
    }

    #[test]
    fn every_distance_and_path_of_random_graphs_is_dijkstras() {
        for (n, arcs, seed) in [(60, 1, 1), (120, 2, 7), (150, 3, 42), (200, 2, 99)] {
            let graph = random_graph(n, arcs, &mut Random(0x9E37_79B9_7F4A_7C15 ^ seed));
            check(&graph, &Cch::new(&graph));
        }
    }

    #[test]
    fn new_weights_need_only_a_customization() {
        let mut random = Random(12345);
        let mut graph = random_graph(150, 2, &mut random);
        let mut cch = Cch::new(&graph);
        for _ in 0..3 {
            for arc in &mut graph.arcs {
                arc.weight_ds = 1 + random.next(80) as u32;
            }
            cch.customize(&graph);
            check(&graph, &cch);
        }
    }

    #[test]
    fn a_node_to_itself_and_unreachable_nodes() {
        // 0 -> 1, and 2 on its own.
        let mut graph = TurnGraph { edge_ds: vec![10; 3], ..Default::default() };
        graph.first_arc = vec![0, 1, 1, 1];
        graph.arcs = vec![Arc { head: 1, weight_ds: 7 }];
        let cch = Cch::new(&graph);
        let mut scratch = Scratch::new(&cch);
        assert_eq!(cch.query(&mut scratch, 0, 1).map(|r| r.0), Some(7));
        assert_eq!(cch.query(&mut scratch, 1, 0), None);
        assert_eq!(cch.query(&mut scratch, 0, 2), None);
        let (zero, meet) = cch.query(&mut scratch, 2, 2).unwrap();
        assert_eq!(zero, 0);
        assert_eq!(cch.path(&scratch, 2, meet), vec![2]);
    }

    #[test]
    fn the_hierarchy_is_always_the_same() {
        let graph = random_graph(100, 2, &mut Random(5));
        let (a, b) = (Cch::new(&graph), Cch::new(&graph));
        assert_eq!((a.rank, a.up, a.forward, a.backward), (b.rank, b.up, b.forward, b.backward));
    }
}
