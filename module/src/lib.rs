//! The routing module (docs/ROUTER_PLAN.md). With what the country packs of the scope say (`pack`),
//! it downloads their OSM extracts, builds the road graph of cars and leaves it as its part of the
//! bundle (`pipeline`), and answers routes, matrices and the nearest street on it (`serve`), with
//! live traffic if it is asked for (`live`).

mod config;
pub mod history;
pub mod isochrone;
pub mod live;
pub mod matching;
pub mod sensors;
mod extracts;
pub mod cch;
pub mod graph;
pub mod graph_io;
pub mod osm;
pub mod pack;
mod pipeline;
pub mod profile;
pub mod route;
pub mod scc;
pub mod situations;
mod serve;
pub mod snap;
mod stats;
pub mod steps;
pub mod traffic;
pub mod trip;
pub mod turn;
pub mod utm;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use geko_module_sdk::Result;
use geko_module_sdk::module::{self, Built, BundleView, Context, Description, Method, Module, ModuleName, Purge, Request, Response, Route};

use crate::pipeline::Setup;

pub const NAME: &str = "router";

pub fn open(context: Context) -> Result<Box<dyn Module>> {
    Ok(Box::new(RouterModule::new(context)?))
}

pub struct RouterModule {
    setup: Setup,
    server: serve::Server,
    /// The poll of live traffic, while there is one: dropping it stops it.
    poller: Mutex<Option<live::Poller>>,
    /// With `[serve] preload = false`: there is a bundle in `pending` that nothing asked for yet.
    deferred: AtomicBool,
    pending: Mutex<Option<PathBuf>>,
}

impl Module for RouterModule {
    fn describe(&self) -> Description {
        Description {
            name: ModuleName::new(NAME).expect("a valid name"),
            version: geko_module_sdk::module_version!().to_owned(),
            contract: module::CONTRACT,
            routes: vec![
                Route { method: Method::Get, path: "".into(), pool: false, public: true },
                Route { method: Method::Get, path: "openapi.yaml".into(), pool: false, public: false },
                Route { method: Method::Get, path: "v1/status".into(), pool: false, public: false },
                Route { method: Method::Get, path: "v1/stats".into(), pool: false, public: false },
                Route { method: Method::Get, path: "v1/route".into(), pool: true, public: false },
                Route { method: Method::Get, path: "v1/nearest".into(), pool: true, public: false },
                Route { method: Method::Get, path: "v1/table".into(), pool: true, public: false },
                Route { method: Method::Post, path: "v1/table".into(), pool: true, public: false },
                Route { method: Method::Get, path: "v1/isochrone".into(), pool: true, public: false },
                Route { method: Method::Get, path: "v1/match".into(), pool: true, public: false },
                Route { method: Method::Post, path: "v1/match".into(), pool: true, public: false },
                Route { method: Method::Get, path: "v1/trip".into(), pool: true, public: false },
                Route { method: Method::Post, path: "v1/trip".into(), pool: true, public: false },
            ],
            openapi: Some("openapi.yaml".into()),
            attribution: Some(self.attribution()),
            commands: vec![],
        }
    }

    fn prepare(&self) -> Result<String> {
        pipeline::prepare(&self.setup)
    }

    fn plan(&self) -> Result<String> {
        Ok(pipeline::plan(&self.setup, &pipeline::downloader(&self.setup, false)))
    }

    fn fetch(&self, refresh: bool) -> Result<String> {
        pipeline::fetch(&self.setup, &pipeline::downloader(&self.setup, refresh))
    }

    fn ready(&self) -> Result<()> {
        pipeline::licenses_accepted(&self.setup)
    }

    fn build(&self, force: bool, refresh: bool) -> Result<Built> {
        pipeline::build(&self.setup, &pipeline::downloader(&self.setup, refresh), force)
    }

    fn next_build(&self) -> Option<Duration> {
        pipeline::next_build(&self.setup)
    }

    fn purge(&self, what: Purge) -> Result<String> {
        let Purge::Packages { all } = what else { return Ok(String::new()) };
        let scope: Vec<_> = self.setup.scope.countries().collect();
        let mut out = String::new();
        for dir in self.setup.packs.purge((!all).then_some(scope.as_slice()))? {
            writeln!(out, "purged {}", dir.display()).unwrap();
        }
        Ok(out)
    }

    fn load(&self, bundle: &BundleView) -> Result<()> {
        self.load_from(&bundle.root)
    }

    fn handle(&self, request: Request) -> Response {
        if self.deferred.load(Ordering::Acquire) && needs_the_graph(&request.route) {
            self.load_deferred();
        }
        self.server.handle(&request)
    }

    fn metrics(&self, out: &mut String) {
        self.server.metrics(out);
    }
}

/// The routes that run on the pool, which can wait for a load: `v1/status` says there is no graph.
fn needs_the_graph(route: &str) -> bool {
    route.starts_with("v1/") && route != "v1/status" && route != "v1/stats"
}

/// Every option of `geko-router.toml` with the value it has and the one it would have without a
/// file, and when a change of it takes effect.
fn settings(config: &config::Config) -> serde_json::Value {
    let d = config::Config::default();
    let setting = |key: &str, value: serde_json::Value, default: serde_json::Value, apply: &str| serde_json::json!({ "key": key, "value": value, "default": default, "apply": apply });
    serde_json::Value::Array(vec![
        setting("osm.mirror", config.osm.mirror.as_str().into(), d.osm.mirror.as_str().into(), "rebuild"),
        setting("packs.registry", config.packs.registry.as_str().into(), d.packs.registry.as_str().into(), "rebuild"),
        setting("profiles.default", config.profiles.default.as_str().into(), d.profiles.default.as_str().into(), "rebuild"),
        setting("traffic.enabled", config.traffic.enabled.into(), d.traffic.enabled.into(), "rebuild"),
        setting("traffic.live", config.traffic.live.into(), d.traffic.live.into(), "restart"),
        setting("sources", serde_json::json!(config.sources), serde_json::json!(d.sources), "rebuild"),
        setting("serve.preload", config.serve.preload.into(), d.serve.preload.into(), "restart"),
    ])
}

impl RouterModule {
    fn new(context: Context) -> Result<Self> {
        let config = config::Config::parse(&context.config, &context.config_file)?;
        let packs = pack::Packs::new(&context.data_dir);
        let mode = config.mode();
        let settings = settings(&config);
        let setup = Setup { config, scope: context.scope, data_dir: context.data_dir, data: context.data, packs };
        let own_sources = pipeline::licenses(&setup).into_iter().map(|l| l.source).collect();
        let server = serve::Server::new(mode).with_status(settings, setup.data_dir.clone(), own_sources);
        Ok(Self { setup, server, poller: Mutex::new(None), deferred: AtomicBool::new(false), pending: Mutex::new(None) })
    }

    fn load_from(&self, root: &Path) -> Result<()> {
        if self.setup.config.serve.preload {
            return self.load_now(root);
        }
        // Left for later, but a part it cannot read is rejected now: geko goes on with the bundle
        // it had, which it could not do from the first request.
        graph_io::check(&root.join(pipeline::GRAPH_PART).join(pipeline::GRAPH_FILE))?;
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(root.to_owned());
        self.deferred.store(true, Ordering::Release);
        Ok(())
    }

    fn load_now(&self, root: &Path) -> Result<()> {
        self.server.load(root)?;
        self.restart_traffic();
        Ok(())
    }

    /// The load that `[serve] preload = false` left for now. The requests that arrive meanwhile wait
    /// for it, and it is tried again if it fails.
    fn load_deferred(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(root) = pending.take() else { return };
        match self.load_now(&root) {
            Ok(()) => self.deferred.store(false, Ordering::Release),
            Err(e) => {
                tracing::error!(error = %e, "cannot load the bundle");
                *pending = Some(root);
            }
        }
    }

    /// What a response of the router owes to its sources: OpenStreetMap always, and the traffic data
    /// only when they are in use.
    fn attribution(&self) -> serde_json::Value {
        let mut attribution = serde_json::json!({
            "derived_database": {
                "license": "ODbL 1.0",
                "notice": "The road graph is a Derived Database of OpenStreetMap: © OpenStreetMap contributors.",
            },
        });
        for source in live::wanted(&self.setup).into_iter().chain(live::wanted_history(&self.setup)) {
            attribution[format!("traffic_{}", source.name)] = live::attribution(&source);
        }
        attribution
    }

    /// Starts polling the traffic sources of the scope for the graph that was just loaded, and stops
    /// the poll of the previous one. With traffic off, offline or without a source in the scope, it
    /// only stops: nothing is downloaded and nothing runs.
    fn restart_traffic(&self) {
        let mut poller = self.poller.lock().unwrap_or_else(|e| e.into_inner());
        *poller = None;
        let Some(loaded) = self.server.current() else { return };
        let feeds = live::wanted(&self.setup).into_iter().map(|source| {
            let fetch = live::fetch_url(source.url.clone());
            live::Feed { source, fetch }
        });
        *poller = live::start(loaded, feeds.collect(), Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_module_sdk::module::DataPolicy;
    use geko_module_sdk::scope::Scope;

    fn module(config: &str, data_dir: &Path) -> RouterModule {
        RouterModule::new(Context { config: config.into(), config_file: String::new(), scope: Scope::default(), data_dir: data_dir.into(), data: DataPolicy::default() }).unwrap()
    }

    /// A bundle with a graph of one road.
    fn bundle(root: &Path) {
        use crate::graph::Builder;
        use crate::osm::Element;
        let mut builder = Builder::new(crate::profile::Vehicle::from_road_classes("[free_flow_kmh]\nprimary = 90\n").unwrap());
        let road = [
            Element::Node { id: 1, lat: 0.0, lon: 0.0, tags: vec![] },
            Element::Node { id: 2, lat: 0.0, lon: 0.01, tags: vec![] },
            Element::Way { id: 9, refs: vec![1, 2], tags: vec![("highway".into(), "primary".into())] },
        ];
        road.iter().for_each(|e| builder.first_pass(e));
        road.iter().for_each(|e| builder.second_pass(e));
        let dir = root.join(pipeline::GRAPH_PART);
        std::fs::create_dir_all(&dir).unwrap();
        graph_io::write(&builder.finish(), &dir.join(pipeline::GRAPH_FILE)).unwrap();
    }

    fn get(route: &str) -> Request {
        Request { route: route.into(), query: vec![("at".into(), "0,0".into())], ..Default::default() }
    }

    #[test]
    fn preload_loads_at_once_and_without_it_the_first_request_that_needs_the_graph_does() {
        let tmp = tempfile::tempdir().unwrap();
        bundle(tmp.path());

        let ready = |module: &RouterModule| {
            let response = module.handle(get("v1/status"));
            assert_eq!(response.status, 200);
            serde_json::from_slice::<serde_json::Value>(&response.body).unwrap()["ready"].as_bool().unwrap()
        };

        let eager = module("", tmp.path());
        eager.load_from(tmp.path()).unwrap();
        assert!(ready(&eager));

        let lazy = module("[serve]\npreload = false", tmp.path());
        lazy.load_from(tmp.path()).unwrap();
        assert!(!ready(&lazy), "not yet");
        assert_eq!(lazy.handle(get("")).status, 200, "the demo needs no graph");
        assert_ne!(lazy.handle(get("v1/nearest")).status, 503);
        assert!(ready(&lazy));
    }

    #[test]
    fn the_status_says_how_it_is_set_up_and_the_stats_what_it_answered_also_without_a_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let module = module("[serve]\npreload = false", tmp.path());
        let body = |route: &str| serde_json::from_slice::<serde_json::Value>(&module.handle(get(route)).body).unwrap();
        let status = body("v1/status");
        assert_eq!((&status["bundle"], &status["nodes"], &status["last_build"]), (&serde_json::Value::Null, &serde_json::Value::Null, &serde_json::Value::Null));
        let preload = status["settings"].as_array().unwrap().iter().find(|s| s["key"] == "serve.preload").unwrap();
        assert_eq!((preload["value"].as_bool(), preload["default"].as_bool(), preload["apply"].as_str()), (Some(false), Some(true), Some("restart")));
        assert_eq!(module.handle(get("v1/nearest")).status, 503);
        let nearest = &body("v1/stats")["routes"][1];
        assert_eq!((nearest["route"].as_str(), nearest["requests"].as_u64(), nearest["errors"].as_u64()), (Some("v1/nearest"), Some(1), Some(1)));
    }

    #[test]
    fn a_graph_of_another_format_is_rejected_at_load_also_without_preload() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(pipeline::GRAPH_PART);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(pipeline::GRAPH_FILE), b"GEKORG00 and more").unwrap();
        for config in ["", "[serve]\npreload = false"] {
            let err = module(config, tmp.path()).load_from(tmp.path()).unwrap_err().to_string();
            assert!(err.contains("not a graph file"), "{err}");
        }
    }

    #[test]
    fn a_load_that_fails_is_tried_again() {
        let tmp = tempfile::tempdir().unwrap();
        let lazy = module("[serve]\npreload = false", tmp.path());
        lazy.load_from(tmp.path()).unwrap();
        assert_eq!(lazy.handle(get("v1/nearest")).status, 503, "no bundle there");
        bundle(tmp.path());
        assert_ne!(lazy.handle(get("v1/nearest")).status, 503);
    }

    #[test]
    fn the_routes_that_wait_are_those_of_the_pool() {
        let tmp = tempfile::tempdir().unwrap();
        for route in module("", tmp.path()).describe().routes {
            assert_eq!(route.pool, needs_the_graph(&route.path), "{}", route.path);
        }
    }
}
