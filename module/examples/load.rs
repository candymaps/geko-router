//! Loads a routing server with concurrent clients and reports its throughput and latency:
//!
//!   cargo run --release -p geko-module-router --example load -- <graph.bin> <osrm|geko> <base url> [requests] [clients] [geometry]
//!
//! The pairs are vertices of random streets of the large components of the graph, from a fixed seed, so
//! two servers are asked the same. `osrm` asks `/route/v1/driving/lon,lat;lon,lat`, `geko` asks
//! `/router/v1/route?from=lat,lon&to=lat,lon`; with `geometry` both return the line of the route, and
//! without it only the numbers. The same client and the same pairs for both: what differs is the server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use geko_module_router::graph_io;
use geko_module_router::scc::{components, sizes};
use geko_module_router::turn::{self, TurnProfile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: load <graph.bin> <osrm|geko> <base url> [requests] [clients] [geometry]";
    let graph = graph_io::read(std::path::Path::new(args.first().ok_or(usage)?))?;
    let kind = args.get(1).ok_or(usage)?.as_str();
    let base = args.get(2).ok_or(usage)?.trim_end_matches('/').to_owned();
    let requests: usize = args.get(3).map_or(Ok(2000), |a| a.parse())?;
    let clients: usize = args.get(4).map_or(Ok(16), |a| a.parse())?;
    let geometry = args.get(5).is_some_and(|a| a == "geometry");
    if kind != "osrm" && kind != "geko" {
        return Err(usage.into());
    }

    let turns = turn::build(&graph, &TurnProfile::default());
    let component = components(&turns);
    let component_sizes = sizes(&component);
    let allowed: Vec<bool> = graph.without_ferries(component.iter().map(|c| component_sizes[*c as usize] >= 1000).collect());
    let candidates: Vec<u32> = (0..graph.edges.len() as u32).filter(|e| allowed[*e as usize]).collect();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let vertex = |edge: u32| {
        let points: Vec<[f64; 2]> = graph.shape(&graph.edges[edge as usize]).collect();
        points[points.len() / 2]
    };
    let mut pairs = Vec::new();
    while pairs.len() < requests {
        let (x, y) = (candidates[(next() % candidates.len() as u64) as usize], candidates[(next() % candidates.len() as u64) as usize]);
        if x != y && component[x as usize] == component[y as usize] {
            pairs.push((vertex(x), vertex(y)));
        }
    }
    let urls: Vec<String> = pairs
        .iter()
        .map(|(a, b)| {
            match kind {
                "osrm" => format!("{base}/route/v1/driving/{},{};{},{}?overview={}&geometries=geojson", a[1], a[0], b[1], b[0], if geometry { "full" } else { "false" }),
                _ => format!("{base}/router/v1/route?from={},{}&to={},{}&geometry={geometry}", a[0], a[1], b[0], b[1]),
            }
        })
        .collect();

    let index = AtomicUsize::new(0);
    let wall = Instant::now();
    let results: Vec<(u128, bool)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..clients)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = index.fetch_add(1, Ordering::Relaxed);
                        let Some(url) = urls.get(i) else { break };
                        let started = Instant::now();
                        let ok = geko_fetch::get_text(url, Duration::from_secs(60), 1 << 26).is_ok();
                        mine.push((started.elapsed().as_micros(), ok));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a client")).collect()
    });
    let wall = wall.elapsed();

    let failed = results.iter().filter(|r| !r.1).count();
    let mut times: Vec<u128> = results.iter().filter(|r| r.1).map(|r| r.0).collect();
    times.sort_unstable();
    if times.is_empty() {
        return Err("every request failed".into());
    }
    let at = |p: f64| times[((times.len() as f64 * p) as usize).min(times.len() - 1)];
    println!("{kind}: {} requests, {clients} clients, geometry {geometry}, {failed} failed", results.len());
    println!("throughput {:.0} requests/s over {:.1} s", results.len() as f64 / wall.as_secs_f64(), wall.as_secs_f64());
    println!("latency median {} us, p90 {} us, p99 {} us, max {} us", at(0.5), at(0.9), at(0.99), times[times.len() - 1]);
    Ok(())
}
