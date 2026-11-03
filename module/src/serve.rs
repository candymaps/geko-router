//! What the router answers under `/router/`: it loads the graph of the bundle, prepares what a
//! route needs (turns, components, the index of streets) and answers `v1/route` and `v1/status`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use geko_module_sdk::Result;
use geko_module_sdk::module::{BundleView, Request, Response};

use crate::cch::{Cch, Scratch};
use crate::graph::Graph;
use crate::pipeline::{GRAPH_FILE, GRAPH_PART, HIERARCHY_FILE};
use crate::route;
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

/// What answers at a moment: the weights of the turn graph and the hierarchy customized for them.
/// Traffic makes a new one and swaps it in, so that requests in flight finish on the one they began with.
struct Weighted {
    turns: Arc<TurnGraph>,
    cch: Cch,
    traffic: Option<TrafficInfo>,
}

/// What a route says of the traffic it was found with.
#[derive(Clone, Copy)]
pub struct TrafficInfo {
    /// Edges slower than free flow.
    pub slowed_edges: usize,
    pub updated: Instant,
}

pub struct Loaded {
    graph: Graph,
    /// The turn graph at free flow: what traffic is applied to.
    free: Arc<TurnGraph>,
    snapper: Snapper,
    live: RwLock<Arc<Weighted>>,
    /// Query state is as large as the graph: requests take one and give it back, not make one.
    scratches: Mutex<Vec<Scratch>>,
}

impl Loaded {
    fn new(graph: Graph, free: TurnGraph, snapper: Snapper, cch: Cch) -> Self {
        let free = Arc::new(free);
        let turns = Arc::clone(&free);
        Self { graph, free, snapper, live: RwLock::new(Arc::new(Weighted { turns, cch, traffic: None })), scratches: Mutex::new(Vec::new()) }
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
        let slowed_edges = (0..traffic.edges() as u32).filter(|e| traffic.speed(*e) < FREE_FLOW).count();
        let info = (slowed_edges > 0).then_some(TrafficInfo { slowed_edges, updated });
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Weighted { turns, cch, traffic: info });
    }

    fn route(&self, weighted: &Weighted, from: &[Snap], to: &[Snap]) -> Option<route::Route> {
        let mut scratch = self.scratches.lock().unwrap_or_else(|e| e.into_inner()).pop().unwrap_or_else(|| Scratch::new(&weighted.cch));
        let found = route::route_cch(&self.graph, &weighted.turns, &weighted.cch, &mut scratch, from, to);
        self.scratches.lock().unwrap_or_else(|e| e.into_inner()).push(scratch);
        found
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
    let (lat, lon) = (lat.trim().parse::<f64>().ok()?, lon.trim().parse::<f64>().ok()?);
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}


/// What a response says of traffic: `null` when none was applied (it is off, or nothing is slow).
fn traffic_json(info: Option<TrafficInfo>) -> serde_json::Value {
    info.map_or(serde_json::Value::Null, |i| serde_json::json!({ "slowed_edges": i.slowed_edges, "age_s": i.updated.elapsed().as_secs() }))
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
        let snapper = Snapper::new(&graph, scc::in_large_component(&turns, min_component));
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
        let Some(loaded) = self.current() else {
            return Response::error(503, "the router has no graph yet").with_header("Retry-After", "60");
        };
        match request.route.as_str() {
            "v1/status" => json(200, serde_json::json!({ "nodes": loaded.graph.nodes.len(), "edges": loaded.graph.edges.len(), "turns": loaded.free.arcs.len(), "traffic": traffic_json(loaded.weighted().traffic) })),
            "v1/route" => self.route(&loaded, request),
            route => Response::error(404, &format!("no route {route}")),
        }
    }

    fn bad(&self, message: &str) -> Response {
        self.bad_request.fetch_add(1, Ordering::Relaxed);
        Response::error(400, message)
    }

    fn route(&self, loaded: &Loaded, request: &Request) -> Response {
        let query = |name: &str| request.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        let point = |name: &str| match query(name) {
            None => Err(format!("`{name}` is required, as `lat,lon`")),
            Some(v) => coordinate(v).ok_or_else(|| format!("`{name}` is not a coordinate `lat,lon`: {v:?}")),
        };
        let (from, to) = match (point("from"), point("to")) {
            (Ok(from), Ok(to)) => (from, to),
            (Err(e), _) | (_, Err(e)) => return self.bad(&e),
        };
        let snap_m = match query("snap_m").map(str::parse::<f64>) {
            None => DEFAULT_SNAP_M,
            Some(Ok(m)) if m > 0.0 && m <= MAX_SNAP_M => m,
            Some(_) => return self.bad(&format!("`snap_m` is a number of metres from 0 to {MAX_SNAP_M}")),
        };
        let geometry = !matches!(query("geometry"), Some("false" | "0"));

        let started = Instant::now();
        let snap = |(lat, lon): (f64, f64)| loaded.snapper.snap(&loaded.graph, lat, lon, snap_m);
        let (snapped_from, snapped_to) = (snap(from), snap(to));
        let describe = |snaps: &[Snap]| snaps.first().map(|s| serde_json::json!({ "lat": s.point[0], "lon": s.point[1], "distance_m": s.distance_m }));
        let (Some(origin), Some(destination)) = (describe(&snapped_from), describe(&snapped_to)) else {
            self.no_route.fetch_add(1, Ordering::Relaxed);
            let which = if snapped_from.is_empty() { "from" } else { "to" };
            return json(422, serde_json::json!({ "error": format!("no street within {snap_m} m of `{which}`") }));
        };
        let weighted = loaded.weighted();
        let Some(found) = loaded.route(&weighted, &snapped_from, &snapped_to) else {
            self.no_route.fetch_add(1, Ordering::Relaxed);
            return json(404, serde_json::json!({ "error": "no route between the points" }));
        };
        self.ok.fetch_add(1, Ordering::Relaxed);
        let mut body = serde_json::json!({
            "duration_s": found.duration_s,
            "distance_m": found.distance_m,
            "snapped": { "from": origin, "to": destination },
            "compute_us": started.elapsed().as_micros() as u64,
            "traffic": traffic_json(weighted.traffic),
        });
        if geometry {
            body["geometry"] = serde_json::json!({ "type": "LineString", "coordinates": found.geometry.iter().map(|p| [p[1], p[0]]).collect::<Vec<_>>() });
        }
        json(200, body)
    }

    pub fn metrics(&self, out: &mut String) {
        use std::fmt::Write as _;
        out.push_str("# HELP geko_router_requests_total Route requests by result.\n# TYPE geko_router_requests_total counter\n");
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
        let request = Request { route: "v1/route".into(), query: query.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(), ..Default::default() };
        let response = server.handle(&request);
        (response.status, serde_json::from_slice(&response.body).unwrap())
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
