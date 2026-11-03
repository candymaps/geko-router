//! The routing module (docs/ROUTER_PLAN.md of the host). It downloads the OSM extracts of the scope,
//! builds the road graph of cars and leaves it as its part of the bundle (`pipeline`). Serving routes
//! on it is the next step: until then it answers 503.

mod config;
pub mod live;
pub mod madrid;
mod extracts;
pub mod cch;
pub mod graph;
pub mod graph_io;
pub mod osm;
mod pipeline;
pub mod profile;
pub mod route;
pub mod scc;
mod serve;
pub mod snap;
pub mod traffic;
pub mod turn;
pub mod utm;

use std::sync::Mutex;
use std::time::Duration;

use geko_module_sdk::Result;
use geko_module_sdk::module::{self, Built, BundleView, Context, Description, Method, Module, ModuleName, Request, Response, Route};

use crate::pipeline::Setup;

pub const NAME: &str = "router";

pub fn open(context: Context) -> Result<Box<dyn Module>> {
    let config = config::Config::parse(&context.config, &context.config_file)?;
    Ok(Box::new(RouterModule { setup: Setup { config, scope: context.scope, data_dir: context.data_dir, data: context.data }, server: serve::Server::default(), poller: Mutex::new(None) }))
}

pub struct RouterModule {
    setup: Setup,
    server: serve::Server,
    /// The poll of live traffic, while there is one: dropping it stops it.
    poller: Mutex<Option<live::Poller>>,
}

impl Module for RouterModule {
    fn describe(&self) -> Description {
        Description {
            name: ModuleName::new(NAME).expect("a valid name"),
            version: geko_module_sdk::module_version!().to_owned(),
            contract: module::CONTRACT,
            routes: vec![
                Route { method: Method::Get, path: "v1/status".into(), pool: false, public: false },
                Route { method: Method::Get, path: "v1/route".into(), pool: true, public: false },
            ],
            openapi: None,
            attribution: Some(self.attribution()),
            commands: vec![],
        }
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

    fn load(&self, bundle: &BundleView) -> Result<()> {
        self.server.load(bundle)?;
        self.restart_traffic();
        Ok(())
    }

    fn handle(&self, request: Request) -> Response {
        self.server.handle(&request)
    }

    fn metrics(&self, out: &mut String) {
        self.server.metrics(out);
    }
}

impl RouterModule {
    /// What a response of the router owes to its sources: OpenStreetMap always, and the traffic data
    /// only when they are in use.
    fn attribution(&self) -> serde_json::Value {
        let mut attribution = serde_json::json!({
            "derived_database": {
                "license": "ODbL 1.0",
                "notice": "The road graph is a Derived Database of OpenStreetMap: © OpenStreetMap contributors.",
            },
        });
        if live::wanted(&self.setup.config.traffic, &self.setup.scope, self.setup.data.offline).contains(&live::Source::Madrid) {
            attribution["traffic_madrid"] = serde_json::json!({
                "license": "CC BY 4.0",
                "notice": "Live traffic of the city of Madrid: © Ayuntamiento de Madrid (datos abiertos), CC BY 4.0.",
            });
        }
        attribution
    }

    /// Starts polling the traffic sources of the scope for the graph that was just loaded, and stops
    /// the poll of the previous one. With traffic off, offline or without a source in the scope, it
    /// only stops: nothing is downloaded and nothing runs.
    fn restart_traffic(&self) {
        let traffic = &self.setup.config.traffic;
        let mut poller = self.poller.lock().unwrap_or_else(|e| e.into_inner());
        *poller = None;
        let Some(loaded) = self.server.current() else { return };
        for source in live::wanted(traffic, &self.setup.scope, self.setup.data.offline) {
            match source {
                live::Source::Madrid => {
                    let (interval, max_age) = (Duration::from_secs(traffic.interval_secs), Duration::from_secs(traffic.max_age_secs));
                    *poller = Some(live::start(loaded.clone(), interval, max_age, live::fetch_url(traffic.madrid.url.clone())));
                }
            }
        }
    }
}
