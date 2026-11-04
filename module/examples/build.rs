//! Builds the part of the router without the host, for measuring the build:
//!
//!   cargo run --release -p geko-module-router --example build -- <data dir> <country>[:<region>,…]… [--force] [--frozen]
//!
//! It opens the module as the host does, with the licenses accepted and `geko-router.toml` from
//! `GEKO_ROUTER_CONFIG` if set, and prints what `build` reports. `--frozen` keeps every download
//! there is (`refresh = "never"`) and fetches only what is missing. The country packs are those of
//! `GEKO_ROUTER_PACKS`, or those of the registry.

use geko_module_sdk::country::{CountryCode, RegionCode};
use geko_module_sdk::module::{Context, DataPolicy, Refresh};
use geko_module_sdk::scope::Scope;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force = args.iter().any(|a| a == "--force");
    let frozen = args.iter().any(|a| a == "--frozen");
    let mut args = args.iter().filter(|a| !a.starts_with("--"));
    let data_dir = args.next().ok_or("usage: build <data dir> <country>[:<region>,…]… [--force] [--frozen]")?;
    let mut scope = Scope::default();
    for entry in args {
        let (country, regions) = entry.split_once(':').unwrap_or((entry, ""));
        let regions = regions.split(',').filter(|r| !r.is_empty()).map(RegionCode::new).collect::<Result<_, _>>()?;
        scope.add(CountryCode::new(country)?, regions)?;
    }
    let config_file = std::env::var("GEKO_ROUTER_CONFIG").unwrap_or_default();
    let config = if config_file.is_empty() { String::new() } else { std::fs::read_to_string(&config_file)? };
    let refresh = if frozen { Refresh::Never } else { DataPolicy::default().refresh };
    let data = DataPolicy { accept_licenses: true, keep_downloads: true, refresh, ..DataPolicy::default() };
    let module = geko_module_router::open(Context { config, config_file, scope, data_dir: data_dir.into(), data })?;
    let started = std::time::Instant::now();
    print!("{}", module.prepare()?);
    let built = module.build(force, false)?;
    print!("{}", built.report);
    for s in &built.stages {
        println!("  {:<12} {:>8} ms  {:>6} MB{}", s.stage, s.elapsed_ms, s.peak_mb.unwrap_or(0), if s.reused { "  (reused)" } else { "" });
    }
    println!("meta: {}", built.meta);
    println!("total: {} ms", started.elapsed().as_millis());
    Ok(())
}
