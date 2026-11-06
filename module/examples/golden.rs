//! The golden routes of a country pack: what the router answers on frozen data, so that only a change
//! in the code or in the pack can change it (`ci/golden.sh`).
//!
//!   cargo run --release -p geko-module-router --example golden -- <pack folder> <data folder> [--write] [--generate <n>]
//!
//! `golden/sets.toml` of the pack names each set: its extracts in the data folder and, if it has one,
//! a frozen reading of a feed of traffic. `golden/<set>.jsonl` has a line per route: where it starts
//! and ends, what it took (`duration_s`, `distance_m`, and with a feed `traffic_duration_s`) and, for
//! those written by hand, a range of what it should measure (`expect_km`, an estimate), which says
//! the numbers make sense and not only that they are those of yesterday. A route that is faster with
//! traffic than without it is an error too.
//!
//! `--write` keeps the routes and writes down what they take now; `--generate <n>` adds that many
//! between random streets, from a fixed seed, to a set that has fewer. A change is reviewed in its diff.

use std::collections::BTreeMap;
use std::path::Path;

use geko_module_router::cch::{Cch, Scratch};
use geko_module_router::graph::{Builder, Graph, Roads};
use geko_module_router::pack::CountryPack;
use geko_module_router::profile::Car;
use geko_module_router::route::route_cch;
use geko_module_router::scc::in_large_component;
use geko_module_router::snap::{Snap, Snapper};
use geko_module_router::traffic::Traffic;
use geko_module_router::turn::{self, TurnGraph, TurnProfile};
use geko_module_router::{osm, sensors};
use serde::{Deserialize, Serialize};

/// Rounded to this in the files, so a difference below it is not one.
const TOLERANCE: f64 = 0.1;
const SNAP_M: f64 = 1000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    extracts: Vec<String>,
    #[serde(default)]
    feed: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Golden {
    name: String,
    from: [f64; 2],
    to: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expect_km: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    distance_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    traffic_duration_s: Option<f64>,
}

struct Network {
    turns: TurnGraph,
    cch: Cch,
}

impl Network {
    fn route(&self, graph: &Graph, from: &[Snap], to: &[Snap]) -> Option<(f64, f64)> {
        let mut scratch = Scratch::new(&self.cch);
        route_cch(graph, &self.turns, &self.cch, &mut scratch, from, to).map(|r| (r.duration_s, r.distance_m))
    }
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The graph of the extracts as a build leaves it: a fragment of each, joined.
fn graph(pack: &CountryPack, data: &Path, set: &Set) -> Result<Graph, Box<dyn std::error::Error>> {
    let mut roads = Roads::default();
    for extract in &set.extracts {
        let path = data.join(extract);
        let mut builder = Builder::new(Car::from_road_classes(&pack.road_classes)?);
        osm::read_pbf(&path, |e| builder.first_pass(&e))?;
        osm::read_pbf(&path, |e| builder.second_pass(&e))?;
        roads.add(builder.fragment());
    }
    Ok(roads.finish())
}

/// `n` routes between the middle of random streets, from a fixed seed.
fn generate(graph: &Graph, allowed: &[bool], n: usize, from_index: usize) -> Vec<Golden> {
    let candidates: Vec<usize> = (0..graph.edges.len()).filter(|e| allowed[*e]).collect();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut point = || {
        let edge = &graph.edges[candidates[(next() % candidates.len() as u64) as usize]];
        let shape: Vec<[f64; 2]> = graph.shape(edge).collect();
        let [lat, lon] = shape[shape.len() / 2];
        [(lat * 1e6).round() / 1e6, (lon * 1e6).round() / 1e6]
    };
    (0..n).map(|i| Golden { name: format!("random-{}", from_index + i), from: point(), to: point(), expect_km: None, distance_m: None, duration_s: None, traffic_duration_s: None }).collect()
}

fn check_set(pack: &CountryPack, name: &str, set: &Set, dir: &Path, data: &Path, write: bool, wanted: usize) -> Result<usize, Box<dyn std::error::Error>> {
    let graph = graph(pack, data, set)?;
    let turns = turn::build(&graph, &TurnProfile::default());
    let allowed = graph.without_ferries(in_large_component(&turns, 1000));
    let snapper = Snapper::new(&graph, allowed.clone());
    let mut cch = Cch::topology(&turns);
    cch.customize(&turns);

    let slow = match &set.feed {
        Some(feed) => {
            let spec = sensors::Spec::of(pack.sources.values().find(|s| s.kind == sensors::KIND).ok_or("the pack has no sensors")?)?;
            let readings = sensors::parse(&spec, &std::fs::read_to_string(data.join(feed))?);
            let mut matcher = sensors::Matcher::new(spec);
            matcher.add(&graph, &snapper, &readings);
            let mut traffic = Traffic::free(graph.edges.len());
            matcher.speeds(&readings).iter().for_each(|(edge, speed)| traffic.set(*edge, *speed));
            let turns = turns.with_traffic(&traffic);
            let mut cch = cch.share_topology();
            cch.customize(&turns);
            Some(Network { turns, cch })
        }
        None => None,
    };
    let free = Network { turns, cch };

    let path = dir.join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path)?;
    let mut lines: Vec<Golden> = text.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<Result<_, _>>()?;
    if lines.len() < wanted {
        let more = wanted - lines.len();
        lines.extend(generate(&graph, &allowed, more, lines.len()));
    }

    let mut failures = 0;
    let mut fail = |line: &Golden, why: String| {
        println!("  {}: {why}", line.name);
        failures += 1;
    };
    let mut found = Vec::new();
    for line in &lines {
        let (from, to) = (snapper.snap(&graph, line.from[0], line.from[1], SNAP_M), snapper.snap(&graph, line.to[0], line.to[1], SNAP_M));
        let Some((duration_s, distance_m)) = free.route(&graph, &from, &to) else {
            fail(line, "no route".into());
            continue;
        };
        let mut now = line.clone();
        now.duration_s = Some(round(duration_s));
        now.distance_m = Some(round(distance_m));
        if let Some([min, max]) = line.expect_km
            && !(min..=max).contains(&(distance_m / 1000.0))
        {
            fail(line, format!("{:.1} km, and it should be from {min} to {max}", distance_m / 1000.0));
        }
        if let Some(network) = &slow {
            match network.route(&graph, &from, &to) {
                Some((slowed, _)) if slowed + TOLERANCE < duration_s => fail(line, format!("{slowed:.1} s with traffic, faster than the {duration_s:.1} s without it")),
                Some((slowed, _)) => now.traffic_duration_s = Some(round(slowed)),
                None => fail(line, "no route with traffic".into()),
            }
        }
        if !write {
            for (field, old, new) in [("duration_s", line.duration_s, now.duration_s), ("distance_m", line.distance_m, now.distance_m), ("traffic_duration_s", line.traffic_duration_s, now.traffic_duration_s)] {
                if let (Some(old), Some(new)) = (old, new)
                    && (old - new).abs() > TOLERANCE
                {
                    fail(line, format!("{field} was {old} and is {new}"));
                } else if old.is_none() != new.is_none() {
                    fail(line, format!("{field} was {old:?} and is {new:?}"));
                }
            }
        }
        found.push(now);
    }
    if write {
        let mut out = String::new();
        for line in &found {
            out.push_str(&serde_json::to_string(line)?);
            out.push('\n');
        }
        std::fs::write(&path, out)?;
    }
    println!("{} {name}: {} routes, {} nodes, {} edges, {} differences{}", pack.country, lines.len(), graph.nodes.len(), graph.edges.len(), failures, if slow.is_some() { ", with traffic" } else { "" });
    Ok(failures)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: golden <pack folder> <data folder> [--write] [--generate <n>]";
    let (pack_dir, data) = (Path::new(args.first().ok_or(usage)?), Path::new(args.get(1).ok_or(usage)?));
    let write = args.iter().any(|a| a == "--write");
    let wanted: usize = match args.iter().position(|a| a == "--generate") {
        Some(i) => args.get(i + 1).ok_or(usage)?.parse()?,
        None => 0,
    };
    let pack = CountryPack::load(pack_dir)?;
    let golden = pack_dir.join("golden");
    let sets: BTreeMap<String, Set> = toml::from_str(&std::fs::read_to_string(golden.join("sets.toml"))?)?;
    let mut failures = 0;
    for (name, set) in &sets {
        failures += check_set(&pack, name, set, &golden, data, write, wanted)?;
    }
    if failures > 0 {
        return Err(format!("{failures} differences in the golden routes of {}", pack.country).into());
    }
    Ok(())
}
