//! The same requests for two routing servers, one path per line, to load them with `wrk`
//! (ci/ab-osrm.sh):
//!
//!   cargo run --release -p geko-module-router --example urls -- <graph.bin> <osrm|geko> [pairs] [geometry]
//!
//! The pairs are vertices of random streets of the same large component of the graph, from a fixed
//! seed, so two servers are asked the same, line by line. `osrm` asks
//! `/route/v1/driving/lon,lat;lon,lat`, `geko` asks `/router/v1/route?from=lat,lon&to=lat,lon`; with
//! `geometry` both return the line of the route, and without it only the numbers.

use std::io::Write;

use geko_module_router::graph_io;
use geko_module_router::scc::{components, sizes};
use geko_module_router::turn::{self, TurnProfile};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: urls <graph.bin> <osrm|geko> [pairs] [geometry]";
    let graph = graph_io::read(std::path::Path::new(args.first().ok_or(usage)?))?;
    let kind = args.get(1).ok_or(usage)?.as_str();
    let count: usize = args.get(2).map_or(Ok(20000), |a| a.parse())?;
    let geometry = args.get(3).is_some_and(|a| a == "geometry");
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
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut written = 0;
    while written < count {
        let (x, y) = (candidates[(next() % candidates.len() as u64) as usize], candidates[(next() % candidates.len() as u64) as usize]);
        if x == y || component[x as usize] != component[y as usize] {
            continue;
        }
        let (a, b) = (vertex(x), vertex(y));
        match kind {
            "osrm" => writeln!(out, "/route/v1/driving/{},{};{},{}?overview={}&geometries=geojson", a[1], a[0], b[1], b[0], if geometry { "full" } else { "false" })?,
            _ => writeln!(out, "/router/v1/route?from={},{}&to={},{}&geometry={geometry}", a[0], a[1], b[0], b[1])?,
        }
        written += 1;
    }
    Ok(())
}
