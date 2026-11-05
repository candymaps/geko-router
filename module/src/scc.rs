//! The strongly connected components of the turn graph: the set of edges that can all reach each
//! other. A car that starts in a small one (a car park, a private road) cannot get anywhere, so
//! those edges are never offered as the place a point snaps to.

use crate::turn::TurnGraph;

const UNSEEN: u32 = u32::MAX;

/// The component of each edge. Tarjan's algorithm, with its own stack: a country has chains of
/// hundreds of thousands of edges, which recursion would not survive.
pub fn components(turns: &TurnGraph) -> Vec<u32> {
    let n = turns.edges();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut component = vec![UNSEEN; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut calls: Vec<(u32, u32)> = Vec::new();
    let (mut counter, mut next_component) = (0u32, 0u32);

    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        index[root as usize] = counter;
        low[root as usize] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        calls.push((root, 0));
        while let Some(&(v, next)) = calls.last() {
            let arcs = turns.arcs_from(v);
            if let Some(arc) = arcs.get(next as usize) {
                calls.last_mut().expect("a call").1 += 1;
                let w = arc.head;
                if index[w as usize] == UNSEEN {
                    index[w as usize] = counter;
                    low[w as usize] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[w as usize] = true;
                    calls.push((w, 0));
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(index[w as usize]);
                }
                continue;
            }
            calls.pop();
            if low[v as usize] == index[v as usize] {
                loop {
                    let w = stack.pop().expect("v is on the stack");
                    on_stack[w as usize] = false;
                    component[w as usize] = next_component;
                    if w == v {
                        break;
                    }
                }
                next_component += 1;
            }
            if let Some(&(parent, _)) = calls.last() {
                low[parent as usize] = low[parent as usize].min(low[v as usize]);
            }
        }
    }
    component
}

/// How many edges each component has, by component.
pub fn sizes(component: &[u32]) -> Vec<u32> {
    let count = component.iter().max().map_or(0, |m| *m as usize + 1);
    let mut sizes = vec![0u32; count];
    for c in component {
        sizes[*c as usize] += 1;
    }
    sizes
}

/// For each edge, whether it is in a component of at least `min_size` edges.
pub fn in_large_component(turns: &TurnGraph, min_size: u32) -> Vec<bool> {
    let component = components(turns);
    let sizes = sizes(&component);
    component.iter().map(|c| sizes[*c as usize] >= min_size).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::Arc;

    fn graph(arcs: &[&[u32]]) -> TurnGraph {
        let mut g = TurnGraph { edge_ds: vec![10; arcs.len()], ..Default::default() };
        for heads in arcs {
            g.first_arc.push(g.arcs.len() as u32);
            g.arcs.extend(heads.iter().map(|h| Arc { head: *h, weight_ds: 10 }));
        }
        g.first_arc.push(g.arcs.len() as u32);
        g
    }

    #[test]
    fn a_cycle_with_a_tail_and_a_loner() {
        // 0 -> 1 -> 2 -> 0 is a cycle; 3 leads into it but nothing leads back; 4 is alone.
        let g = graph(&[&[1], &[2], &[0], &[0], &[]]);
        let c = components(&g);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_ne!(c[3], c[0]);
        assert_ne!(c[4], c[0]);
        assert_ne!(c[3], c[4]);
        let sizes = sizes(&c);
        assert_eq!(sizes[c[0] as usize], 3);
        assert_eq!(in_large_component(&g, 2), vec![true, true, true, false, false]);
    }

    #[test]
    fn a_long_chain_does_not_overflow_the_stack() {
        let n = 300_000u32;
        let heads: Vec<Vec<u32>> = (0..n).map(|i| vec![(i + 1) % n]).collect();
        let refs: Vec<&[u32]> = heads.iter().map(Vec::as_slice).collect();
        let c = components(&graph(&refs));
        assert!(c.iter().all(|x| *x == c[0]));
    }
}
