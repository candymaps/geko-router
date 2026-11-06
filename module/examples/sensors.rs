//! How the sensors of a feed match the graph, sensor by sensor, to check the matching against what the
//! sensors say of themselves (docs/ROUTER_PLAN.md, point 19):
//!
//!   sensors <graph.bin> <folder of the country pack> <source> <feed.xml>
//!
//! A sensor names its street in its description, and the edge it is matched to has its own name from
//! OSM: they should agree. The sensors whose street is not in the name of their edge are listed.

use std::path::Path;

use geko_module_router::graph_io;
use geko_module_router::pack::CountryPack;
use geko_module_router::scc::in_large_component;
use geko_module_router::sensors::{self, Spec};
use geko_module_router::snap::Snapper;
use geko_module_router::turn::{self, TurnProfile};

/// In capitals, without accents, the words of more than three letters.
fn words(text: &str) -> Vec<String> {
    let plain: String = text.to_uppercase().chars().map(|c| match c {
        'Á' => 'A',
        'É' => 'E',
        'Í' => 'I',
        'Ó' => 'O',
        'Ú' | 'Ü' => 'U',
        c if c.is_alphanumeric() => c,
        _ => ' ',
    }).collect();
    plain.split_whitespace().filter(|w| w.len() > 3).map(str::to_owned).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [graph, pack, source, feed] = args.as_slice() else {
        return Err("usage: sensors <graph.bin> <folder of the country pack> <source> <feed.xml>".into());
    };
    let graph = graph_io::read(Path::new(graph))?;
    let pack = CountryPack::load(Path::new(pack))?;
    let spec = Spec::of(pack.sources.get(source).ok_or("no such source")?)?;
    let turns = turn::build(&graph, &TurnProfile::default());
    let snapper = Snapper::new(&graph, graph.without_ferries(in_large_component(&turns, 1000)));
    let readings = sensors::parse(&spec, &std::fs::read_to_string(feed)?);
    let (matched, stats) = sensors::match_sensors(&spec, &graph, &snapper, &readings);
    println!("{} sensors: {stats:?}", readings.len());
    let (mut named, mut agree, mut unnamed, mut shown) = (0, 0, 0, 0);
    for reading in &readings {
        let Some(edge) = matched.get(&reading.id) else { continue };
        let edge = &graph.edges[*edge as usize];
        let Some(name) = graph.names.get(edge.name as usize) else {
            unnamed += 1;
            continue;
        };
        named += 1;
        let (name_words, own) = (words(name), words(&reading.description));
        if name_words.iter().any(|w| own.contains(w)) {
            agree += 1;
        } else if shown < 25 {
            shown += 1;
            println!("  {:<60} on {name}", reading.description);
        }
    }
    println!("{named} on a named street, {unnamed} on one without a name; {agree} say the same street ({:.1} %)", agree as f64 * 100.0 / named.max(1) as f64);
    Ok(())
}
