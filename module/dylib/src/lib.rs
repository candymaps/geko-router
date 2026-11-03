//! The router module as a library that geko loads (docs/MODULES_PLAN.md of the host, `DylibModule`).

use geko_module_sdk::Result;
use geko_module_sdk::module::{Context, Module};

fn open(context: Context) -> Result<Box<dyn Module>> {
    // The library has its own copy of `tracing`, which the host's subscriber does not see.
    use tracing_subscriber::prelude::*;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .finish()
        .with(tracing_subscriber::filter::filter_fn(geko_module_sdk::progress::shown))
        .try_init();
    geko_module_router::open(context)
}

geko_module_sdk::export_module!(open);
