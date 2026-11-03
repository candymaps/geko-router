//! The routing module (docs/ROUTER_PLAN.md of the host). It downloads the OSM extracts of the scope,
//! builds the road graph of cars and leaves it as its part of the bundle (`pipeline`). Serving routes
//! on it is the next step: until then it answers 503.

mod config;
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
pub mod turn;

use std::time::Duration;

use geko_module_sdk::Result;
use geko_module_sdk::module::{self, Built, BundleView, Context, Description, Method, Module, ModuleName, Request, Response, Route};

use crate::pipeline::Setup;

pub const NAME: &str = "router";

pub fn open(context: Context) -> Result<Box<dyn Module>> {
    let config = config::Config::parse(&context.config, &context.config_file)?;
    Ok(Box::new(RouterModule { setup: Setup { config, scope: context.scope, data_dir: context.data_dir, data: context.data }, server: serve::Server::default() }))
}

pub struct RouterModule {
    setup: Setup,
    server: serve::Server,
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
            attribution: Some(serde_json::json!({
                "derived_database": {
                    "license": "ODbL 1.0",
                    "notice": "The road graph is a Derived Database of OpenStreetMap: © OpenStreetMap contributors.",
                },
            })),
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
        self.server.load(bundle)
    }

    fn handle(&self, request: Request) -> Response {
        self.server.handle(&request)
    }

    fn metrics(&self, out: &mut String) {
        self.server.metrics(out);
    }
}
