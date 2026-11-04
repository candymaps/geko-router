//! What the router answers under `/router/`: it loads the graph of the bundle, prepares what a
//! route needs (turns, components, the index of streets) and answers `v1/route`, `v1/table`,
//! `v1/nearest` and `v1/status`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use geko_module_sdk::Result;
use geko_module_sdk::module::{BundleView, Request, Response};

use crate::cch::{Cch, Scratch};
use crate::graph::Graph;
use crate::pipeline::{GRAPH_FILE, GRAPH_PART, HIERARCHY_FILE};
use crate::route::{self, durations_cch};
use crate::steps;
use crate::scc;
use crate::snap::{Snap, Snapper};
use crate::traffic::{FREE_FLOW, Traffic};
use crate::turn::{self, TurnGraph, TurnProfile};
use crate::graph_io;

/// A component of fewer edges than this is an island: a car park, a private road.
const MIN_COMPONENT: u32 = 1000;
/// How far from the road a point may be and still snap to it, unless the request says otherwise.
const DEFAULT_SNAP_M: f64 = 1000.0;
const MAX_SNAP_M: f64 = 50_000.0;
const MAX_ALTERNATIVES: usize = 3;
/// The reference of the API, and the page that tries it.
pub const OPENAPI: &str = include_str!("../../openapi.yaml");
const DEMO: &str = include_str!("demo.html");
/// The most cells of a matrix, and the most points on a side of it: a request must not hold a thread
/// for long, and the answer has a number for each cell.
const MAX_TABLE_CELLS: usize = 1_000_000;
const MAX_TABLE_POINTS: usize = 2_000;

/// What answers at a moment: the weights of the turn graph and the hierarchy customized for them.
/// Traffic makes a new one and swaps it in, so that requests in flight finish on the one they began with.
struct Weighted {
    turns: Arc<TurnGraph>,
    cch: Cch,
    /// Edges slower than free flow, or `None` if no source says anything: no data is not no jams.
    slowed_edges: Option<usize>,
    /// For each edge, whether a source says anything of it; empty when none does.
    measured: Vec<bool>,
}

pub struct Loaded {
    graph: Graph,
    /// The turn graph at free flow: what traffic is applied to.
    free: Arc<TurnGraph>,
    snapper: Snapper,
    live: RwLock<Arc<Weighted>>,
    /// Query state is as large as the graph: requests take one and give it back, not make one.
    scratches: Mutex<Vec<Scratch>>,
    /// When the sources last answered, which can be after the last change of the weights.
    updated: Mutex<Instant>,
    /// `[min lat, min lon, max lat, max lon]` of the nodes, found the first time it is asked.
    bounds: std::sync::OnceLock<[f64; 4]>,
}

impl Loaded {
    fn new(graph: Graph, free: TurnGraph, snapper: Snapper, cch: Cch) -> Self {
        let free = Arc::new(free);
        let turns = Arc::clone(&free);
        Self { graph, free, snapper, live: RwLock::new(Arc::new(Weighted { turns, cch, slowed_edges: None, measured: Vec::new() })), scratches: Mutex::new(Vec::new()), updated: Mutex::new(Instant::now()), bounds: std::sync::OnceLock::new() }
    }

    fn bounds(&self) -> [f64; 4] {
        *self.bounds.get_or_init(|| {
            self.graph.nodes.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, n| [b[0].min(n.lat), b[1].min(n.lon), b[2].max(n.lat), b[3].max(n.lon)])
        })
    }

    fn weighted(&self) -> Arc<Weighted> {
        self.live.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn edges(&self) -> usize {
        self.graph.edges.len()
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn snapper(&self) -> &Snapper {
        &self.snapper
    }

    /// Answers from now on with the graph at the speeds of `traffic`; with none, at free flow again.
    /// The hierarchy that answers is not touched while the next one is customized.
    pub fn apply(&self, traffic: &Traffic, updated: Instant) {
        let current = self.weighted();
        let turns = Arc::new(self.free.with_traffic(traffic));
        let mut cch = current.cch.share_topology();
        cch.customize(&turns);
        let slowed = (0..traffic.edges() as u32).filter(|e| traffic.speed(*e) < FREE_FLOW).count();
        let measured = traffic.measured().contains(&true);
        let weighted = Weighted { turns, cch, slowed_edges: measured.then_some(slowed), measured: if measured { traffic.measured().to_vec() } else { Vec::new() } };
        self.touch(updated);
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(weighted);
    }

    /// The sources answered at `updated`, and what they said changes nothing.
    pub fn touch(&self, updated: Instant) {
        *self.updated.lock().unwrap_or_else(|e| e.into_inner()) = updated;
    }

    /// What a response says of traffic: `null` when no source says anything (it is off, or the data
    /// are too old); with nothing slow, `slowed_edges` is 0.
    fn traffic(&self, weighted: &Weighted) -> serde_json::Value {
        let age_s = self.updated.lock().unwrap_or_else(|e| e.into_inner()).elapsed().as_secs();
        weighted.slowed_edges.map_or(serde_json::Value::Null, |slowed_edges| serde_json::json!({ "slowed_edges": slowed_edges, "age_s": age_s }))
    }

    /// Runs `query` with the state of a query of the pool.
    fn with_scratch<T>(&self, weighted: &Weighted, query: impl FnOnce(&mut Scratch) -> T) -> T {
        let mut scratch = self.scratches.lock().unwrap_or_else(|e| e.into_inner()).pop().unwrap_or_else(|| Scratch::new(&weighted.cch));
        let found = query(&mut scratch);
        self.scratches.lock().unwrap_or_else(|e| e.into_inner()).push(scratch);
        found
    }

    /// The fastest route and up to `alternatives` others.
    fn routes(&self, weighted: &Weighted, from: &[Snap], to: &[Snap], alternatives: usize) -> Vec<route::Route> {
        self.with_scratch(weighted, |scratch| route::alternatives_cch(&self.graph, &weighted.turns, &weighted.cch, scratch, from, to, alternatives))
    }

    /// What a response says of a route: its time, its length and, if asked, its line and its steps.
    fn route_json(&self, weighted: &Weighted, route: &route::Route, geometry: bool, steps: bool) -> serde_json::Value {
        let mut body = serde_json::json!({ "duration_s": route.duration_s, "distance_m": route.distance_m });
        if geometry {
            body["geometry"] = serde_json::json!({ "type": "LineString", "coordinates": route.geometry.iter().map(|p| [p[1], p[0]]).collect::<Vec<_>>() });
        }
        if steps {
            body["steps"] = steps::steps(&self.graph, &weighted.turns, route).iter().map(step_json).collect();
        }
        body
    }
}

#[derive(Default)]
pub struct Server {
    loaded: RwLock<Option<Arc<Loaded>>>,
    ok: AtomicU64,
    no_route: AtomicU64,
    bad_request: AtomicU64,
}

fn coordinate(value: &str) -> Option<(f64, f64)> {
    let (lat, lon) = value.split_once(',')?;
    on_earth(lat.trim().parse::<f64>().ok()?, lon.trim().parse::<f64>().ok()?)
}

fn on_earth(lat: f64, lon: f64) -> Option<(f64, f64)> {
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

fn query<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// How far from the road a point may be, as `snap_m` says.
fn snap_limit(request: &Request) -> std::result::Result<f64, String> {
    match query(request, "snap_m").map(str::parse::<f64>) {
        None => Ok(DEFAULT_SNAP_M),
        Some(Ok(m)) if m > 0.0 && m <= MAX_SNAP_M => Ok(m),
        Some(_) => Err(format!("`snap_m` is a number of metres from 0 to {MAX_SNAP_M}")),
    }
}

type Coordinates = Vec<(f64, f64)>;

/// The points of a matrix and how far from the road they may be: from the query (`from` and `to`
/// repeated, for a few points) or from a JSON body (`{"from": [[lat, lon], ...], "to": [...]}`, for
/// as many as a matrix allows, which would not fit in a URL).
fn table_request(request: &Request) -> std::result::Result<(Coordinates, Coordinates, f64), String> {
    if request.body.is_empty() {
        return Ok((points(request, "from")?, points(request, "to")?, snap_limit(request)?));
    }
    let body: serde_json::Value = serde_json::from_slice(&request.body).map_err(|e| format!("the body is not JSON: {e}"))?;
    let list = |name: &str| -> std::result::Result<Vec<(f64, f64)>, String> {
        let items = body[name].as_array().filter(|a| !a.is_empty()).ok_or_else(|| format!("`{name}` is required, as a list of `[lat, lon]`"))?;
        let pair = |item: &serde_json::Value| match item.as_array().map(Vec::as_slice) {
            Some([lat, lon]) => on_earth(lat.as_f64()?, lon.as_f64()?),
            _ => None,
        };
        items.iter().map(pair).collect::<Option<_>>().ok_or_else(|| format!("`{name}` has something that is not a coordinate `[lat, lon]`"))
    };
    let snap_m = match body.get("snap_m") {
        None | Some(serde_json::Value::Null) => DEFAULT_SNAP_M,
        Some(v) => v.as_f64().filter(|m| *m > 0.0 && *m <= MAX_SNAP_M).ok_or_else(|| format!("`snap_m` is a number of metres from 0 to {MAX_SNAP_M}"))?,
    };
    Ok((list("from")?, list("to")?, snap_m))
}

/// The coordinates of a parameter that can be repeated, at least one.
fn points(request: &Request, name: &str) -> std::result::Result<Vec<(f64, f64)>, String> {
    let values: Vec<&str> = request.query.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect();
    if values.is_empty() {
        return Err(format!("`{name}` is required, as `lat,lon`, once for each point"));
    }
    values.into_iter().map(|v| coordinate(v).ok_or_else(|| format!("`{name}` is not a coordinate `lat,lon`: {v:?}"))).collect()
}

fn snapped_json(snaps: &[Snap]) -> Option<serde_json::Value> {
    snaps.first().map(|s| serde_json::json!({ "lat": s.point[0], "lon": s.point[1], "distance_m": s.distance_m }))
}

fn step_json(step: &steps::Step) -> serde_json::Value {
    let mut body = serde_json::json!({
        "maneuver": step.maneuver,
        "distance_m": step.distance_m,
        "duration_s": step.duration_s,
        "location": [step.location[1], step.location[0]],
    });
    if let Some(modifier) = step.modifier {
        body["modifier"] = modifier.into();
    }
    if let Some(name) = &step.name {
        body["name"] = name.as_str().into();
    }
    body
}

/// The `alternatives` of a request: how many other routes it asks for.
fn alternatives(request: &Request) -> std::result::Result<usize, String> {
    match query(request, "alternatives") {
        None => Ok(0),
        Some(v) => v.parse::<usize>().ok().filter(|n| *n <= MAX_ALTERNATIVES).ok_or_else(|| format!("`alternatives` is a number from 0 to {MAX_ALTERNATIVES}: {v:?}")),
    }
}

/// Which share of a route, by length, a source says something of. The first and last edges count
/// whole: on a route of any length that is noise, and it is a share, not a measurement.
fn coverage(graph: &Graph, edges: &[u32], measured: &[bool]) -> serde_json::Value {
    let (mut live, mut total) = (0.0_f64, 0.0_f64);
    for edge in edges {
        let length = graph.edges[*edge as usize].length_m as f64;
        total += length;
        if measured.get(*edge as usize).copied().unwrap_or(false) {
            live += length;
        }
    }
    let live = if total > 0.0 { (live / total * 1000.0).round() / 1000.0 } else { 0.0 };
    serde_json::json!({ "live": live, "free_flow": ((1.0 - live) * 1000.0).round() / 1000.0 })
}

fn json(status: u16, value: serde_json::Value) -> Response {
    Response::new(status, "application/json", value.to_string())
}

impl Server {
    pub fn load(&self, bundle: &BundleView) -> Result<()> {
        let dir = bundle.root.join(GRAPH_PART);
        let graph = graph_io::read(&dir.join(GRAPH_FILE))?;
        self.install(graph, MIN_COMPONENT, Some(&dir.join(HIERARCHY_FILE)));
        Ok(())
    }

    /// Prepares what a route needs from `graph` and starts answering with it; requests in flight finish
    /// on the previous one. The topology of the hierarchy is read from `hierarchy`, or worked out again
    /// if the file is missing or is not of this graph.
    fn install(&self, graph: Graph, min_component: u32, hierarchy: Option<&std::path::Path>) {
        let turns = turn::build(&graph, &TurnProfile::default());
        let snapper = Snapper::new(&graph, graph.without_ferries(scc::in_large_component(&turns, min_component)));
        let mut cch = match hierarchy.map(|path| Cch::read(path, &turns)) {
            Some(Ok(cch)) => cch,
            other => {
                if let Some(Err(e)) = other {
                    tracing::warn!(error = %e, "working out the hierarchy again");
                }
                Cch::topology(&turns)
            }
        };
        cch.customize(&turns);
        *self.loaded.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Loaded::new(graph, turns, snapper, cch)));
    }

    /// What answers now, which traffic is applied to.
    pub fn current(&self) -> Option<Arc<Loaded>> {
        self.loaded.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn handle(&self, request: &Request) -> Response {
        match request.route.as_str() {
            "" => return Response::new(200, "text/html; charset=utf-8", DEMO),
            "openapi.yaml" => return Response::new(200, "application/yaml", OPENAPI),
            _ => {}
        }
        let Some(loaded) = self.current() else {
            return Response::error(503, "the router has no graph yet").with_header("Retry-After", "60");
        };
        match request.route.as_str() {
            "v1/status" => json(200, serde_json::json!({ "nodes": loaded.graph.nodes.len(), "edges": loaded.graph.edges.len(), "turns": loaded.free.arcs.len(), "bounds": loaded.bounds(), "traffic": loaded.traffic(&loaded.weighted()) })),
            "v1/route" => self.route(&loaded, request),
            "v1/table" => self.table(&loaded, request),
            "v1/nearest" => self.nearest(&loaded, request),
            route => Response::error(404, &format!("no route {route}")),
        }
    }

    fn bad(&self, message: &str) -> Response {
        self.bad_request.fetch_add(1, Ordering::Relaxed);
        Response::error(400, message)
    }

    fn route(&self, loaded: &Loaded, request: &Request) -> Response {
        let point = |name: &str| match query(request, name) {
            None => Err(format!("`{name}` is required, as `lat,lon`")),
            Some(v) => coordinate(v).ok_or_else(|| format!("`{name}` is not a coordinate `lat,lon`: {v:?}")),
        };
        let (from, to) = match (point("from"), point("to")) {
            (Ok(from), Ok(to)) => (from, to),
            (Err(e), _) | (_, Err(e)) => return self.bad(&e),
        };
        let snap_m = match snap_limit(request) {
            Ok(m) => m,
            Err(e) => return self.bad(&e),
        };
        let geometry = !matches!(query(request, "geometry"), Some("false" | "0"));
        let with_steps = matches!(query(request, "steps"), Some("true" | "1"));
        let wanted = match alternatives(request) {
            Ok(n) => n,
            Err(e) => return self.bad(&e),
        };

        let started = Instant::now();
        let snap = |(lat, lon): (f64, f64)| loaded.snapper.snap(&loaded.graph, lat, lon, snap_m);
        let (snapped_from, snapped_to) = (snap(from), snap(to));
        let (Some(origin), Some(destination)) = (snapped_json(&snapped_from), snapped_json(&snapped_to)) else {
            self.no_route.fetch_add(1, Ordering::Relaxed);
            let which = if snapped_from.is_empty() { "from" } else { "to" };
            return json(422, serde_json::json!({ "error": format!("no street within {snap_m} m of `{which}`") }));
        };
        let weighted = loaded.weighted();
        let mut found = loaded.routes(&weighted, &snapped_from, &snapped_to, wanted);
        if found.is_empty() {
            self.no_route.fetch_add(1, Ordering::Relaxed);
            return json(404, serde_json::json!({ "error": "no route between the points" }));
        }
        let others = found.split_off(1);
        let found = found.remove(0);
        self.ok.fetch_add(1, Ordering::Relaxed);
        // Always an object: a route says what is known of it, also when that is nothing.
        let mut traffic = loaded.traffic(&weighted);
        if traffic.is_null() {
            traffic = serde_json::json!({});
        }
        traffic["coverage"] = coverage(&loaded.graph, &found.edges, &weighted.measured);
        let mut body = loaded.route_json(&weighted, &found, geometry, with_steps);
        body["snapped"] = serde_json::json!({ "from": origin, "to": destination });
        body["traffic"] = traffic;
        if wanted > 0 {
            body["alternatives"] = others
                .iter()
                .map(|other| {
                    let mut alternative = loaded.route_json(&weighted, other, geometry, with_steps);
                    alternative["coverage"] = coverage(&loaded.graph, &other.edges, &weighted.measured);
                    alternative
                })
                .collect();
        }
        body["compute_us"] = (started.elapsed().as_micros() as u64).into();
        json(200, body)
    }

    /// The durations from every `from` to every `to`, in seconds: a row for each `from`, `null` where
    /// a point has no street near or there is no route.
    fn table(&self, loaded: &Loaded, request: &Request) -> Response {
        let (from, to, snap_m) = match table_request(request) {
            Ok(parsed) => parsed,
            Err(e) => return self.bad(&e),
        };
        if from.len() > MAX_TABLE_POINTS || to.len() > MAX_TABLE_POINTS {
            return self.bad(&format!("{} x {} has more than the {MAX_TABLE_POINTS} points a side of a table can have", from.len(), to.len()));
        }
        if from.len() * to.len() > MAX_TABLE_CELLS {
            return self.bad(&format!("{} x {} is more than the {MAX_TABLE_CELLS} cells of a table", from.len(), to.len()));
        }
        let started = Instant::now();
        let snap = |points: &[(f64, f64)]| points.iter().map(|(lat, lon)| loaded.snapper.snap(&loaded.graph, *lat, *lon, snap_m)).collect::<Vec<_>>();
        let (from, to) = (snap(&from), snap(&to));
        let weighted = loaded.weighted();
        let durations = loaded.with_scratch(&weighted, |scratch| durations_cch(&weighted.turns, &weighted.cch, scratch, &from, &to));
        self.ok.fetch_add(1, Ordering::Relaxed);
        let snapped = |list: &[Vec<Snap>]| list.iter().map(|s| snapped_json(s)).collect::<Vec<_>>();
        json(200, serde_json::json!({
            "durations_s": durations,
            "snapped": { "from": snapped(&from), "to": snapped(&to) },
            "compute_us": started.elapsed().as_micros() as u64,
            "traffic": loaded.traffic(&weighted),
        }))
    }

    /// The street closest to `at`, where a route from that point would start.
    fn nearest(&self, loaded: &Loaded, request: &Request) -> Response {
        let (lat, lon) = match query(request, "at") {
            None => return self.bad("`at` is required, as `lat,lon`"),
            Some(v) => match coordinate(v) {
                Some(point) => point,
                None => return self.bad(&format!("`at` is not a coordinate `lat,lon`: {v:?}")),
            },
        };
        let snap_m = match snap_limit(request) {
            Ok(m) => m,
            Err(e) => return self.bad(&e),
        };
        let snaps = loaded.snapper.snap(&loaded.graph, lat, lon, snap_m);
        let Some(snapped) = snapped_json(&snaps) else {
            self.no_route.fetch_add(1, Ordering::Relaxed);
            return json(422, serde_json::json!({ "error": format!("no street within {snap_m} m of `at`") }));
        };
        self.ok.fetch_add(1, Ordering::Relaxed);
        json(200, serde_json::json!({ "snapped": snapped, "way": loaded.graph.edges[snaps[0].edge as usize].way }))
    }

    pub fn metrics(&self, out: &mut String) {
        use std::fmt::Write as _;
        out.push_str("# HELP geko_router_requests_total Requests of routes, tables and nearest streets by result.\n# TYPE geko_router_requests_total counter\n");
        for (result, counter) in [("ok", &self.ok), ("no_route", &self.no_route), ("bad_request", &self.bad_request)] {
            let _ = writeln!(out, "geko_router_requests_total{{result=\"{result}\"}} {}", counter.load(Ordering::Relaxed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates() {
        assert_eq!(coordinate("39.57,2.65"), Some((39.57, 2.65)));
        assert_eq!(coordinate(" 39.57 , -2.65 "), Some((39.57, -2.65)));
        for bad in ["", "39.57", "a,b", "91,0", "0,181", "NaN,0", "1,2,3"] {
            assert_eq!(coordinate(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn without_a_graph_it_is_a_503() {
        let response = Server::default().handle(&Request { route: "v1/route".into(), ..Default::default() });
        assert_eq!(response.status, 503);
    }

    /// The crossing of the other tests: 111 m arms at 10 m/s around (0, 0).
    fn server() -> Server {
        use crate::graph::Builder;
        use crate::osm::Element;
        use crate::profile::Car;
        let node = |id, lat, lon| Element::Node { id, lat, lon, tags: Vec::new() };
        let way = |id, refs: &[i64]| Element::Way { id, refs: refs.to_vec(), tags: vec![("highway".into(), "residential".into())] };
        let elements = [
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.0),
            node(4, -0.001, 0.0),
            node(5, 0.0, 0.0),
            way(10, &[1, 5]),
            way(11, &[5, 2]),
            way(12, &[4, 5]),
            way(13, &[5, 3]),
        ];
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        let server = Server::default();
        server.install(builder.finish(), 1, None);
        server
    }

    fn get(server: &Server, query: &[(&str, &str)]) -> (u16, serde_json::Value) {
        ask(server, "v1/route", query)
    }

    fn ask(server: &Server, route: &str, query: &[(&str, &str)]) -> (u16, serde_json::Value) {
        let request = Request { route: route.into(), query: query.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(), ..Default::default() };
        let response = server.handle(&request);
        (response.status, serde_json::from_slice(&response.body).unwrap())
    }

    #[test]
    fn without_a_source_a_route_says_all_of_it_is_free_flow() {
        let server = server();
        let (_, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0")]);
        assert_eq!(body["traffic"], serde_json::json!({ "coverage": { "live": 0.0, "free_flow": 1.0 } }));
        assert_eq!(ask(&server, "v1/status", &[]).1["traffic"], serde_json::Value::Null);
    }

    #[test]
    fn a_clear_road_with_a_sensor_is_not_a_road_without_data() {
        let server = server();
        let loaded = server.current().unwrap();
        let mut traffic = Traffic::free(loaded.edges());
        (0..loaded.edges() as u32).for_each(|e| traffic.set(e, FREE_FLOW));
        loaded.apply(&traffic, Instant::now());
        let (_, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0")]);
        assert_eq!(body["traffic"]["slowed_edges"], 0);
        assert_eq!(body["traffic"]["coverage"], serde_json::json!({ "live": 1.0, "free_flow": 0.0 }));
        // The data got too old: back to knowing nothing.
        loaded.apply(&Traffic::free(loaded.edges()), Instant::now());
        let (_, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0")]);
        assert_eq!(body["traffic"], serde_json::json!({ "coverage": { "live": 0.0, "free_flow": 1.0 } }));
    }

    #[test]
    fn only_the_measured_part_of_a_route_is_live() {
        let graph_edges = [(100.0, true), (300.0, false)];
        let graph = Graph { edges: graph_edges.iter().map(|(m, _)| crate::graph::Edge { from: 0, to: 0, way: 0, length_m: *m, seconds: 0.0, shape_start: 0, shape_len: 0, reversed: false, ferry: false, name: crate::graph::NO_NAME }).collect(), ..Default::default() };
        let measured: Vec<bool> = graph_edges.iter().map(|(_, m)| *m).collect();
        assert_eq!(coverage(&graph, &[0, 1], &measured), serde_json::json!({ "live": 0.25, "free_flow": 0.75 }));
        assert_eq!(coverage(&graph, &[0, 1], &[]), serde_json::json!({ "live": 0.0, "free_flow": 1.0 }));
        assert_eq!(coverage(&graph, &[], &measured), serde_json::json!({ "live": 0.0, "free_flow": 1.0 }));
    }

    #[test]
    fn a_table_says_what_the_routes_say() {
        let server = server();
        let (a, b, far) = ("0,-0.0005", "0.0005,0", "40,3");
        let (status, body) = ask(&server, "v1/table", &[("from", a), ("from", far), ("to", b), ("to", a)]);
        assert_eq!(status, 200, "{body}");
        let (_, route) = get(&server, &[("from", a), ("to", b)]);
        assert_eq!(body["durations_s"][0][0], route["duration_s"]);
        assert_eq!(body["durations_s"][1], serde_json::json!([null, null]), "no street near the second row");
        assert!(body["snapped"]["from"][0]["distance_m"].as_f64().unwrap() < 0.01);
        assert_eq!(body["snapped"]["from"][1], serde_json::Value::Null);
    }

    #[test]
    fn a_table_too_large_or_without_points_is_refused() {
        let server = server();
        assert_eq!(ask(&server, "v1/table", &[("to", "0,0")]).0, 400);
        assert_eq!(ask(&server, "v1/table", &[("from", "0,0"), ("to", "x")]).0, 400);
        let many: Vec<(&str, &str)> = std::iter::repeat_n(("from", "0,0"), 1001).chain(std::iter::repeat_n(("to", "0,0"), 1000)).collect();
        let (status, body) = ask(&server, "v1/table", &many);
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("1001 x 1000 is more than"), "{body}");
        let long: Vec<(&str, &str)> = std::iter::repeat_n(("from", "0,0"), 2001).chain([("to", "0,0")]).collect();
        let (status, body) = ask(&server, "v1/table", &long);
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("2001 x 1 has more than"), "{body}");
    }

    fn post(server: &Server, route: &str, body: &str) -> (u16, serde_json::Value) {
        let request = Request { route: route.into(), body: body.as_bytes().to_vec(), content_type: Some("application/json".into()), ..Default::default() };
        let response = server.handle(&request);
        (response.status, serde_json::from_slice(&response.body).unwrap())
    }

    #[test]
    fn a_table_from_a_body_is_the_same_as_from_the_query() {
        let server = server();
        let (a, b, far) = ("0,-0.0005", "0.0005,0", "40,3");
        let (_, from_query) = ask(&server, "v1/table", &[("from", a), ("from", far), ("to", b), ("to", a)]);
        let (status, from_body) = post(&server, "v1/table", r#"{"from": [[0, -0.0005], [40, 3]], "to": [[0.0005, 0], [0, -0.0005]]}"#);
        assert_eq!(status, 200, "{from_body}");
        assert_eq!(from_body["durations_s"], from_query["durations_s"]);
        assert_eq!(post(&server, "v1/table", r#"{"from": [[0, 0]], "to": [[0, 0]], "snap_m": 50}"#).0, 200);
    }

    #[test]
    fn a_body_that_is_not_a_table_is_refused() {
        let server = server();
        for (body, word) in [
            ("nonsense", "not JSON"),
            (r#"{"to": [[0, 0]]}"#, "from"),
            (r#"{"from": [], "to": [[0, 0]]}"#, "from"),
            (r#"{"from": [[0, 0]], "to": [[0, "x"]]}"#, "to"),
            (r#"{"from": [[0, 0, 1]], "to": [[0, 0]]}"#, "from"),
            (r#"{"from": [[95, 0]], "to": [[0, 0]]}"#, "from"),
            (r#"{"from": [[0, 0]], "to": [[0, 0]], "snap_m": -1}"#, "snap_m"),
        ] {
            let (status, got) = post(&server, "v1/table", body);
            assert_eq!(status, 400, "{body}: {got}");
            assert!(got["error"].as_str().unwrap().contains(word), "{body}: {got}");
        }
    }

    #[test]
    fn the_nearest_street() {
        let server = server();
        let (status, body) = ask(&server, "v1/nearest", &[("at", "0.0001,-0.0005")]);
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["way"], 10);
        assert!((body["snapped"]["distance_m"].as_f64().unwrap() - 11.1).abs() < 0.2, "{body}");
        assert_eq!(ask(&server, "v1/nearest", &[]).0, 400);
        assert_eq!(ask(&server, "v1/nearest", &[("at", "x")]).0, 400);
        assert_eq!(ask(&server, "v1/nearest", &[("at", "40,3")]).0, 422);
    }

    #[test]
    fn a_route_with_its_geometry() {
        let server = server();
        let (status, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0")]);
        assert_eq!(status, 200, "{body}");
        assert!((body["duration_s"].as_f64().unwrap() - 17.1).abs() < 0.15, "{body}");
        assert_eq!(body["geometry"]["type"], "LineString");
        // GeoJSON order: longitude first.
        assert_eq!(body["geometry"]["coordinates"][0], serde_json::json!([-0.0005, 0.0]));
        assert!(body["snapped"]["from"]["distance_m"].as_f64().unwrap() < 0.01);
        let (_, short) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0"), ("geometry", "false")]);
        assert!(short.get("geometry").is_none());
    }

    #[test]
    fn the_page_and_the_reference_need_no_graph() {
        let server = Server::default();
        for (route, kind, start) in [("", "text/html; charset=utf-8", "<!doctype html>"), ("openapi.yaml", "application/yaml", "openapi: 3.1")] {
            let response = server.handle(&Request { route: route.into(), ..Default::default() });
            assert_eq!((response.status, response.content_type.as_str()), (200, kind), "{route:?}");
            assert!(String::from_utf8(response.body).unwrap().starts_with(start), "{route:?}");
        }
        for path in ["/:", "/v1/status:", "/v1/route:", "/v1/nearest:", "/v1/table:"] {
            assert!(OPENAPI.contains(&format!("\n  {path}\n")), "{path}");
        }
        assert!(!DEMO.contains("http://") && !DEMO.contains("https://"), "the page makes no request outside the server");
    }

    #[test]
    fn a_route_with_steps_and_the_status_with_its_bounds() {
        let server = server();
        let (status, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0"), ("steps", "true"), ("alternatives", "2")]);
        assert_eq!(status, 200, "{body}");
        let steps = body["steps"].as_array().unwrap();
        assert_eq!((steps[0]["maneuver"].clone(), steps[steps.len() - 1]["maneuver"].clone()), ("depart".into(), "arrive".into()));
        let total: f64 = steps.iter().map(|s| s["distance_m"].as_f64().unwrap()).sum();
        assert!((total - body["distance_m"].as_f64().unwrap()).abs() < 1.0, "{body}");
        assert!(body["alternatives"].is_array());
        let (_, plain) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0")]);
        assert!(plain.get("steps").is_none() && plain.get("alternatives").is_none());
        let (_, status) = ask(&server, "v1/status", &[]);
        assert_eq!(status["bounds"], serde_json::json!([-0.001, -0.001, 0.001, 0.001]));
    }

    #[test]
    fn too_many_alternatives_are_refused() {
        let server = server();
        for value in ["4", "-1", "x"] {
            let (status, body) = get(&server, &[("from", "0,-0.0005"), ("to", "0.0005,0"), ("alternatives", value)]);
            assert_eq!(status, 400, "{value}: {body}");
            assert!(body["error"].as_str().unwrap().contains("alternatives"), "{body}");
        }
    }

    #[test]
    fn the_errors_say_what_is_wrong() {
        let server = server();
        for (query, status, word) in [
            (vec![("to", "0,0")], 400, "from"),
            (vec![("from", "0,0"), ("to", "x")], 400, "to"),
            (vec![("from", "0,0"), ("to", "0,0"), ("snap_m", "-3")], 400, "snap_m"),
            (vec![("from", "40,3"), ("to", "0,0")], 422, "from"),
            (vec![("from", "0,0"), ("to", "40,3")], 422, "to"),
        ] {
            let (got, body) = get(&server, &query);
            assert_eq!(got, status, "{query:?}: {body}");
            assert!(body["error"].as_str().unwrap().contains(word), "{query:?}: {body}");
        }
        let mut metrics = String::new();
        server.metrics(&mut metrics);
        assert!(metrics.contains("geko_router_requests_total{result=\"bad_request\"} 3"), "{metrics}");
    }
}
