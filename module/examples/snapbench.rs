//! Times `Snapper::snap` on points near roads and on points anywhere in the box of the graph (many in
//! the sea or the mountains, the worst case for the rings of the grid).
//!
//!   cargo run --release -p geko-module-router --example snapbench -- <graph.bin> [points] [max_m]

use std::time::Instant;

use geko_module_router::graph_io;
use geko_module_router::scc::in_large_component;
use geko_module_router::snap::Snapper;
use geko_module_router::turn::{self, TurnProfile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().ok_or("usage: snapbench <graph.bin> [points] [max_m]")?;
    let points: usize = args.get(1).map_or(Ok(100_000), |a| a.parse())?;
    let max_m: f64 = args.get(2).map_or(Ok(1000.0), |a| a.parse())?;
    let graph = graph_io::read(std::path::Path::new(path))?;
    let turns = turn::build(&graph, &TurnProfile::default());
    let allowed = graph.without_ferries(in_large_component(&turns, 1000));
    let started = Instant::now();
    let snapper = Snapper::new(&graph, allowed);
    println!("index built in {} ms", started.elapsed().as_millis());

    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let (mut lat0, mut lat1, mut lon0, mut lon1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for n in &graph.nodes {
        lat0 = lat0.min(n.lat);
        lat1 = lat1.max(n.lat);
        lon0 = lon0.min(n.lon);
        lon1 = lon1.max(n.lon);
    }
    let near: Vec<(f64, f64)> = (0..points)
        .map(|_| {
            let n = &graph.nodes[(next() * graph.nodes.len() as f64) as usize];
            (n.lat + (next() - 0.5) * 0.001, n.lon + (next() - 0.5) * 0.001)
        })
        .collect();
    let anywhere: Vec<(f64, f64)> = (0..points).map(|_| (lat0 + next() * (lat1 - lat0), lon0 + next() * (lon1 - lon0))).collect();
    for (name, set) in [("near roads", near), ("anywhere in the box", anywhere)] {
        let mut times = Vec::with_capacity(set.len());
        let mut found = 0;
        for (lat, lon) in &set {
            let t = Instant::now();
            let snaps = snapper.snap(&graph, *lat, *lon, max_m);
            times.push(t.elapsed().as_nanos());
            found += usize::from(!snaps.is_empty());
        }
        times.sort_unstable();
        let at = |p: f64| times[((times.len() - 1) as f64 * p) as usize] as f64 / 1000.0;
        let mean = times.iter().sum::<u128>() as f64 / times.len() as f64 / 1000.0;
        println!("{name}: found {found}/{}; median {:.1} us, p90 {:.1} us, p99 {:.1} us, max {:.1} us, mean {:.1} us", set.len(), at(0.5), at(0.9), at(0.99), at(1.0), mean);
    }
    Ok(())
}
