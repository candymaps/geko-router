//! Times the reference Dijkstra on random pairs of points of a built graph, to have a baseline for the
//! faster algorithms (docs/ROUTER_PLAN.md of the host):
//!
//!   cargo run --release -p geko-module-router --example bench -- <graph.bin> [queries] [threads]
//!
//! The points are the middle of random edges of the large components, from a fixed seed, so two runs
//! ask the same. Prints the time of a query (median, 90th and 99th percentile, mean), the
//! throughput with the threads asked for, and what the routes are like (a car that goes 5 km/h on
//! average, or a thousand routes that do not exist, say something is wrong with the graph).

use std::time::Instant;

use geko_module_router::graph_io;
use geko_module_router::cch::{Cch, Scratch};
use geko_module_router::route::{duration_cch, route, route_cch};
use geko_module_router::scc::in_large_component;
use geko_module_router::snap::Snap;
use geko_module_router::turn::{self, TurnProfile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().ok_or("usage: bench <graph.bin> [queries] [threads]")?;
    let queries: usize = args.get(1).map_or(Ok(1000), |a| a.parse())?;
    let threads: usize = args.get(2).map_or(Ok(1), |a| a.parse())?;

    let started = Instant::now();
    let graph = graph_io::read(std::path::Path::new(path))?;
    let turns = turn::build(&graph, &TurnProfile::default());
    let allowed = in_large_component(&turns, 1000);
    println!("loaded in {} ms: {} edges, {} turns", started.elapsed().as_millis(), graph.edges.len(), turns.arcs.len());

    let candidates: Vec<u32> = (0..graph.edges.len() as u32).filter(|e| allowed[*e as usize]).collect();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let pairs: Vec<(Snap, Snap)> = (0..queries)
        .map(|_| {
            let mut pick = || Snap { edge: candidates[(next() % candidates.len() as u64) as usize], fraction: 0.5, distance_m: 0.0, point: [0.0, 0.0] };
            (pick(), pick())
        })
        .collect();

    let run = |slice: &[(Snap, Snap)]| -> Vec<(u128, Option<(f64, f64)>)> {
        slice
            .iter()
            .map(|(a, b)| {
                let t = Instant::now();
                let r = route(&graph, &turns, std::slice::from_ref(a), std::slice::from_ref(b));
                (t.elapsed().as_micros(), r.map(|r| (r.duration_s, r.distance_m)))
            })
            .collect()
    };

    let wall = Instant::now();
    let results: Vec<(u128, Option<(f64, f64)>)> = std::thread::scope(|scope| {
        let chunk = pairs.len().div_ceil(threads.max(1));
        let handles: Vec<_> = pairs.chunks(chunk.max(1)).map(|c| scope.spawn(|| run(c))).collect();
        handles.into_iter().flat_map(|h| h.join().expect("a thread")).collect()
    });
    let wall = wall.elapsed();

    let mut times: Vec<u128> = results.iter().map(|r| r.0).collect();
    times.sort_unstable();
    let at = |p: f64| times[((times.len() as f64 * p) as usize).min(times.len() - 1)];
    let found: Vec<(f64, f64)> = results.iter().filter_map(|r| r.1).collect();
    println!("{} queries, {} threads: {} routes, {} without one", queries, threads, found.len(), queries - found.len());
    println!("time of a query: median {} us, p90 {} us, p99 {} us, mean {:.0} us", at(0.5), at(0.9), at(0.99), times.iter().sum::<u128>() as f64 / times.len() as f64);
    println!("throughput: {:.0} queries/s", queries as f64 / wall.as_secs_f64());
    if !found.is_empty() {
        let n = found.len() as f64;
        let (duration, distance) = found.iter().fold((0.0, 0.0), |a, r| (a.0 + r.0, a.1 + r.1));
        println!("routes: {:.1} km and {:.1} min on average, {:.1} km/h", distance / n / 1000.0, duration / n / 60.0, distance / duration * 3.6);
    }

    let t = Instant::now();
    let file = std::path::Path::new(path).with_file_name("cch.bin");
    let (mut cch, how) = match Cch::read(&file, &turns) {
        Ok(cch) => (cch, "read from the bundle"),
        Err(_) => (Cch::topology(&turns), "worked out"),
    };
    let built = t.elapsed();
    let t = Instant::now();
    cch.customize(&turns);
    println!("CCH: {} nodes, {} shortcuts, depth {}; order and topology {how} in {} ms, customization {} ms", cch.nodes(), cch.edges(), cch.depth(), built.as_millis(), t.elapsed().as_millis());
    let mut scratch = Scratch::new(&cch);
    let (mut fast, mut full, mut wrong) = (Vec::new(), Vec::new(), 0usize);
    let wall = Instant::now();
    for ((a, b), expected) in pairs.iter().zip(&results) {
        let t = Instant::now();
        let d = duration_cch(&turns, &cch, &mut scratch, std::slice::from_ref(a), std::slice::from_ref(b));
        fast.push(t.elapsed().as_micros());
        let t = Instant::now();
        let r = route_cch(&graph, &turns, &cch, &mut scratch, std::slice::from_ref(a), std::slice::from_ref(b));
        full.push(t.elapsed().as_micros());
        let want = expected.1.map(|r| r.0);
        if d != want || r.as_ref().map(|r| r.duration_s) != want {
            wrong += 1;
        }
    }
    let total = wall.elapsed();
    fast.sort_unstable();
    full.sort_unstable();
    let pct = |v: &[u128], p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
    println!("CCH vs Dijkstra: {} of {} differ", wrong, queries);
    println!("CCH duration only: median {} us, p99 {} us", pct(&fast, 0.5), pct(&fast, 0.99));
    println!("CCH with route:    median {} us, p99 {} us ({:.0} queries/s, one thread)", pct(&full, 0.5), pct(&full, 0.99), queries as f64 / total.as_secs_f64() * 2.0);
    Ok(())
}
