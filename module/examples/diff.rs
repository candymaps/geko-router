//! Shows where our route and OSRM's part ways for one pair of points:
//!
//!   cargo run --release -p geko-module-router --example diff -- <graph.bin> <osrm url> <lat,lon> <lat,lon>
//!
//! Prints the OSM nodes both routes pass through (OSRM's from `annotations=nodes`) and the stretch
//! where they differ, with the ways and speeds of our edges there.

use std::time::Duration;

use geko_module_router::cch::{Cch, Scratch};
use geko_module_router::graph_io;
use geko_module_router::route::route_cch;
use geko_module_router::scc::in_large_component;
use geko_module_router::snap::Snapper;
use geko_module_router::turn::{self, TurnProfile};

fn point(text: &str) -> Option<(f64, f64)> {
    let (a, b) = text.split_once(',')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: diff <graph.bin> <osrm url> <lat,lon> <lat,lon>";
    let graph = graph_io::read(std::path::Path::new(args.first().ok_or(usage)?))?;
    let base = args.get(1).ok_or(usage)?;
    let (from, to) = (point(args.get(2).ok_or(usage)?).ok_or(usage)?, point(args.get(3).ok_or(usage)?).ok_or(usage)?);
    let turns = turn::build(&graph, &TurnProfile::default());
    let snapper = Snapper::new(&graph, graph.without_ferries(in_large_component(&turns, 1000)));
    let mut cch = Cch::new(&turns);
    cch.customize(&turns);
    let mut scratch = Scratch::new(&cch);
    let (a, b) = (snapper.snap(&graph, from.0, from.1, 200.0), snapper.snap(&graph, to.0, to.1, 200.0));
    let ours = route_cch(&graph, &turns, &cch, &mut scratch, &a, &b).ok_or("no route of ours")?;

    let url = format!("{}/route/v1/driving/{},{};{},{}?overview=false&annotations=nodes&steps=false", base.trim_end_matches('/'), from.1, from.0, to.1, to.0);
    let json: serde_json::Value = serde_json::from_str(&geko_fetch::get_text(&url, Duration::from_secs(30), 1 << 24)?)?;
    let route = &json["routes"][0];
    let theirs: Vec<i64> = route["legs"][0]["annotation"]["nodes"].as_array().ok_or("no nodes")?.iter().filter_map(|n| n.as_i64()).collect();
    println!("ours: {:.0} m {:.0} s; OSRM: {:.0} m {:.0} s", ours.distance_m, ours.duration_s, route["distance"].as_f64().unwrap_or(0.0), route["duration"].as_f64().unwrap_or(0.0));

    let mut ours_nodes: Vec<i64> = Vec::new();
    for e in &ours.edges {
        let edge = &graph.edges[*e as usize];
        if ours_nodes.is_empty() {
            ours_nodes.push(graph.nodes[edge.from as usize].osm_id);
        }
        ours_nodes.push(graph.nodes[edge.to as usize].osm_id);
    }
    let set: std::collections::HashSet<i64> = theirs.iter().copied().collect();
    let ours_set: std::collections::HashSet<i64> = ours_nodes.iter().copied().collect();
    println!("our edges: {} (nodes {}), OSRM nodes {}", ours.edges.len(), ours_nodes.len(), theirs.len());
    println!("our junction nodes not on OSRM's route:");
    for e in &ours.edges {
        let edge = &graph.edges[*e as usize];
        let (f, t) = (graph.nodes[edge.from as usize].osm_id, graph.nodes[edge.to as usize].osm_id);
        if !set.contains(&f) || !set.contains(&t) {
            println!("  way {} {f}->{t}: {:.0} m {:.1} s ({:.0} km/h)", edge.way, edge.length_m, edge.seconds, edge.length_m as f64 / edge.seconds as f64 * 3.6);
        }
    }
    for (i, e) in ours.edges.iter().enumerate().step_by(10) {
        let n = &graph.nodes[graph.edges[*e as usize].from as usize];
        println!("  at edge {i}: {},{} (lon,lat)", n.lon, n.lat);
    }
    println!("OSRM nodes not on ours: {}", theirs.iter().filter(|n| !ours_set.contains(n)).count());
    Ok(())
}
