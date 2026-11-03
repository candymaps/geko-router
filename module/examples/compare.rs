//! Compares the routes of the router with those of an OSRM server on the same extract:
//!
//!   cargo run --release -p geko-module-router --example compare -- <graph.bin> <osrm url> [pairs] [seed]
//!
//! The points are vertices of random streets of the same component of the graph (so both should
//! find a route), from a fixed seed unless one is given. For each pair it asks both and prints how
//! the durations and the distances compare, and the pairs that differ most, to look at on a map.
//! The distance says whether they choose the same roads; the duration, also whether they agree on
//! how fast a car goes, which OSRM's `car.lua` and the speeds of the pack set differently.

use std::time::Duration;

use geko_module_router::graph_io;
use geko_module_router::route::route;
use geko_module_router::scc::{components, sizes};
use geko_module_router::snap::Snapper;
use geko_module_router::turn::{self, TurnProfile};

struct Pair {
    from: [f64; 2],
    to: [f64; 2],
}

fn osrm(base: &str, pair: &Pair) -> Option<(f64, f64)> {
    let url = format!("{}/route/v1/driving/{},{};{},{}?overview=false", base.trim_end_matches('/'), pair.from[1], pair.from[0], pair.to[1], pair.to[0]);
    let body = geko_fetch::get_text(&url, Duration::from_secs(30), 1 << 20).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let route = json["routes"].get(0)?;
    Some((route["duration"].as_f64()?, route["distance"].as_f64()?))
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

fn summary(label: &str, mut ratios: Vec<f64>) {
    ratios.sort_by(|a, b| a.total_cmp(b));
    println!(
        "{label:<10} p5 {:.3}  p25 {:.3}  median {:.3}  p75 {:.3}  p95 {:.3}",
        percentile(&ratios, 0.05),
        percentile(&ratios, 0.25),
        percentile(&ratios, 0.5),
        percentile(&ratios, 0.75),
        percentile(&ratios, 0.95)
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, base) = (args.first().ok_or("usage: compare <graph.bin> <osrm url> [pairs] [seed]")?, args.get(1).ok_or("usage: compare <graph.bin> <osrm url> [pairs] [seed]")?);
    let count: usize = args.get(2).map_or(Ok(300), |a| a.parse())?;
    let seed: u64 = args.get(3).map_or(Ok(0x9E37_79B9_7F4A_7C15), |a| a.parse())?;

    let graph = graph_io::read(std::path::Path::new(path))?;
    let turns = turn::build(&graph, &TurnProfile::default());
    let component = components(&turns);
    let component_sizes = sizes(&component);
    let allowed: Vec<bool> = component.iter().map(|c| component_sizes[*c as usize] >= 1000).collect();
    let snapper = Snapper::new(&graph, allowed.clone());

    let candidates: Vec<u32> = (0..graph.edges.len() as u32).filter(|e| allowed[*e as usize]).collect();
    let mut state = seed;
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
    while pairs.len() < count {
        let (a, b) = (candidates[(next() % candidates.len() as u64) as usize], candidates[(next() % candidates.len() as u64) as usize]);
        if component[a as usize] == component[b as usize] && a != b {
            pairs.push(Pair { from: vertex(a), to: vertex(b) });
        }
    }

    let (mut duration, mut distance) = (Vec::new(), Vec::new());
    let mut rows: Vec<(f64, f64, f64, [f64; 2], [f64; 2])> = Vec::new();
    let (mut only_ours, mut only_osrm, mut neither) = (0, 0, 0);
    for pair in &pairs {
        let ours = {
            let (a, b) = (snapper.snap(&graph, pair.from[0], pair.from[1], 200.0), snapper.snap(&graph, pair.to[0], pair.to[1], 200.0));
            route(&graph, &turns, &a, &b)
        };
        match (ours, osrm(base, pair)) {
            (Some(ours), Some((osrm_duration, osrm_distance))) => {
                duration.push(ours.duration_s / osrm_duration);
                distance.push(ours.distance_m / osrm_distance);
                rows.push((ours.distance_m / osrm_distance, ours.duration_s, osrm_duration, pair.from, pair.to));
            }
            (Some(_), None) => only_ours += 1,
            (None, Some(_)) => only_osrm += 1,
            (None, None) => neither += 1,
        }
    }

    println!("{} pairs: {} compared, {} only we found, {} only OSRM found, {} neither", pairs.len(), duration.len(), only_ours, only_osrm, neither);
    if duration.is_empty() {
        return Ok(());
    }
    println!("ratio ours / OSRM:");
    summary("distance", distance.clone());
    summary("duration", duration.clone());
    let close = distance.iter().filter(|r| (**r - 1.0).abs() <= 0.02).count();
    let same_speed = duration.iter().zip(&distance).map(|(t, d)| t / d).collect::<Vec<_>>();
    println!("distance within 2 %: {:.1} % of the pairs", close as f64 / distance.len() as f64 * 100.0);
    summary("time/dist", same_speed);

    rows.sort_by(|a, b| (b.0 - 1.0).abs().total_cmp(&(a.0 - 1.0).abs()));
    println!("pairs whose distance differs most (lat,lon -> lat,lon: distance ratio, our time, OSRM time):");
    for (ratio, ours, theirs, from, to) in rows.iter().take(5) {
        println!("  {:.5},{:.5} -> {:.5},{:.5}: {:.3}, {:.0} s, {:.0} s", from[0], from[1], to[0], to[1], ratio, ours, theirs);
    }
    Ok(())
}
