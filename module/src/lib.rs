//! The routing module (docs/ROUTER_PLAN.md of the host). Step 1: the skeleton, which answers 503
//! until it has a graph to route on.

use geko_module_sdk::Result;
use geko_module_sdk::module::{self, Context, Description, Method, Module, ModuleName, Request, Response, Route};

pub const NAME: &str = "router";

pub fn open(_context: Context) -> Result<Box<dyn Module>> {
    Ok(Box::new(RouterModule))
}

pub struct RouterModule;

impl Module for RouterModule {
    fn describe(&self) -> Description {
        Description {
            name: ModuleName::new(NAME).expect("a valid name"),
            version: geko_module_sdk::module_version!().to_owned(),
            contract: module::CONTRACT,
            routes: vec![Route { method: Method::Get, path: "v1/status".into(), pool: false, public: false }],
            openapi: None,
            attribution: None,
            commands: vec![],
        }
    }

    fn handle(&self, request: Request) -> Response {
        match request.route.as_str() {
            "v1/status" => Response::error(503, "the router has no graph yet").with_header("Retry-After", "60"),
            route => Response::error(404, &format!("no route {route}")),
        }
    }
}
