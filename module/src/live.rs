//! Live traffic, in the background: a thread that asks the source of the scope for the state of the
//! roads every few minutes and gives the router the speeds it says, until it is dropped. Nothing
//! here runs, and nothing is downloaded, unless `[traffic] enabled` says so and the scope has a
//! source (`wanted`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::madrid;
use crate::pack::Source;
use crate::pipeline::Setup;
use crate::serve::Loaded;
use crate::traffic::{FREE_FLOW, Traffic};

/// The only kind of source of live traffic there is, the sensors of Madrid: its reader is
/// `madrid.rs`, until the providers by `kind` (docs/ROUTER_PLAN.md, point 21).
const SENSORS: &str = "sensors-xml";

/// The sources to poll, from the packs of the scope with what `[sources.<name>]` changes in them:
/// none unless traffic is on, the data may be downloaded (not offline) and the scope holds what a
/// source measures.
pub fn wanted(setup: &Setup) -> Vec<Source> {
    let traffic = &setup.config.traffic;
    if !(traffic.enabled && traffic.live) || setup.data.offline {
        return Vec::new();
    }
    let mut sources = Vec::new();
    for country in setup.scope.countries() {
        let pack = match setup.packs.get(country) {
            Ok(pack) => pack,
            Err(e) => {
                tracing::warn!(%country, error = %e, "no traffic: the country pack cannot be read");
                continue;
            }
        };
        let partitions: Vec<_> = setup.scope.partitions().into_iter().filter(|p| p.country == country).collect();
        for source in pack.sources.values() {
            match setup.source(source) {
                Ok(source) if source.enabled && source.kind == SENSORS && partitions.iter().any(|p| pack.covers(&source, p.region.as_ref())) => sources.push(source),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "a source of traffic is left out"),
            }
        }
    }
    sources
}

/// What the terms of a source ask of a response that uses it: who it comes from, its license with a
/// link to it, and that what the router does with the data is not what the source published, nor
/// something it backs. The date of the data is in `traffic.source_updated` of each response.
pub fn attribution(source: &Source) -> serde_json::Value {
    serde_json::json!({
        "license": source.license.name,
        "license_url": source.license.url,
        "notice": source.license.attribution,
        "changes": "Modified: the speeds are estimated from these data, and are not values of the source, which neither takes part in this service nor endorses it.",
    })
}

/// Gets the text of the feed of a source.
pub type Fetch = Box<dyn FnMut() -> Result<String, String> + Send>;

pub fn fetch_url(url: String) -> Fetch {
    Box::new(move || geko_fetch::get_text(&url, Duration::from_secs(30), 16 << 20).map_err(|e| e.to_string()))
}

/// A running poll. Dropping it tells the thread to stop, which it does before its next one.
pub struct Poller {
    stop: Arc<AtomicBool>,
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Polls `fetch` every `interval` and applies what it says to `loaded`. A reading that cannot be
/// renewed within `max_age` is dropped, and the roads go back to free flow.
pub fn start(loaded: Arc<Loaded>, interval: Duration, max_age: Duration, mut fetch: Fetch) -> Poller {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let spawned = std::thread::Builder::new().name("geko-router-traffic".into()).spawn(move || {
        let mut matcher = madrid::Matcher::default();
        let mut speeds: Vec<(u32, u16)> = Vec::new();
        let mut last_ok: Option<Instant> = None;
        let mut applied: Vec<(u32, u16)> = Vec::new();
        while !flag.load(Ordering::Relaxed) {
            match fetch() {
                Ok(text) => {
                    let readings = madrid::parse(&text);
                    if readings.is_empty() {
                        tracing::warn!("the traffic feed has no usable sensors");
                    } else {
                        if let Some(stats) = matcher.add(loaded.graph(), loaded.snapper(), &readings) {
                            tracing::info!(sensors = readings.len(), matched = stats.matched, no_direction = stats.no_direction, no_street = stats.no_street, wrong_direction = stats.wrong_direction, "sensors matched to streets");
                        }
                        speeds = matcher.speeds(&readings);
                        last_ok = Some(Instant::now());
                        loaded.set_source_updated(madrid::updated(&text));
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot read the traffic feed"),
            }
            let fresh = last_ok.is_some_and(|t| t.elapsed() <= max_age);
            // Every edge measured, the clear ones too: they are what `traffic.coverage` says is known.
            let current: &[(u32, u16)] = if fresh { &speeds } else { &[] };
            if current != applied.as_slice() {
                let mut traffic = Traffic::free(loaded.edges());
                current.iter().for_each(|(edge, speed)| traffic.set(*edge, *speed));
                loaded.apply(&traffic, last_ok.unwrap_or_else(Instant::now));
                tracing::info!(measured_edges = current.len(), slowed_edges = current.iter().filter(|(_, s)| *s < FREE_FLOW).count(), "traffic applied");
                applied = current.to_vec();
            } else if let Some(read) = last_ok {
                loaded.touch(read);
            }
            let wake = Instant::now() + interval;
            while Instant::now() < wake && !flag.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200).min(interval));
            }
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "cannot start the traffic thread");
    }
    Poller { stop }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pack::Packs;
    use geko_module_sdk::country::{CountryCode, RegionCode};
    use geko_module_sdk::module::DataPolicy;
    use geko_module_sdk::scope::Scope;

    fn setup(config: &str, country: &str, regions: &[&str], offline: bool) -> Setup {
        let mut scope = Scope::default();
        scope.add(CountryCode::new(country).unwrap(), regions.iter().map(|r| RegionCode::new(r).unwrap()).collect()).unwrap();
        let packs = Packs::at(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries"), true);
        Setup { config: Config::parse(config, "").unwrap(), scope, data_dir: "/nowhere".into(), data: DataPolicy { offline, ..DataPolicy::default() }, packs }
    }

    fn names(config: &str, country: &str, regions: &[&str], offline: bool) -> Vec<String> {
        wanted(&setup(config, country, regions, offline)).into_iter().map(|s| s.name).collect()
    }

    #[test]
    fn nothing_is_polled_unless_traffic_is_on_and_the_scope_has_a_source() {
        let on = "[traffic]\nenabled = true";
        let madrid = ["madrid-sensors"];
        assert!(names("", "ES", &[], false).is_empty(), "off by default");
        assert_eq!(names(on, "ES", &[], false), madrid);
        assert_eq!(names(on, "ES", &["ES-MD"], false), madrid);
        assert_eq!(names(on, "ES", &["ES-M"], false), madrid, "the province, the same land");
        assert!(names(on, "ES", &["ES-IB"], false).is_empty(), "Madrid is not in the scope");
        assert!(names(on, "LU", &[], false).is_empty(), "no source in Luxembourg");
        assert!(names(on, "ES", &[], true).is_empty(), "offline");
        assert!(names("[traffic]\nenabled = true\nlive = false", "ES", &[], false).is_empty());
        assert!(names("[traffic]\nenabled = true\n[sources.madrid-sensors]\nenabled = false", "ES", &[], false).is_empty());
    }

    #[test]
    fn a_source_in_use_owes_its_license_and_says_its_data_are_changed() {
        let sources = wanted(&setup("[traffic]\nenabled = true", "ES", &[], false));
        let attribution = attribution(&sources[0]);
        assert_eq!(attribution["license"], "CC BY 4.0");
        assert_eq!(attribution["license_url"], "https://creativecommons.org/licenses/by/4.0/legalcode.es");
        assert!(attribution["notice"].as_str().unwrap().contains("Ayuntamiento de Madrid"));
        assert!(attribution["changes"].as_str().unwrap().starts_with("Modified"));
    }

    #[test]
    fn a_source_changes_in_the_configuration_and_the_incidents_stay_off() {
        let sources = wanted(&setup("[traffic]\nenabled = true\n[sources.madrid-sensors]\nrefresh_secs = 600\nttl_secs = 1200\n[sources.dgt-incidents]\nenabled = true", "ES", &[], false));
        assert_eq!(sources.len(), 1, "no provider reads the incidents yet");
        assert_eq!((sources[0].refresh_secs, sources[0].ttl_secs), (600, 1200));
    }
}
