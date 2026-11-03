//! Live traffic, in the background: a thread that asks the source of the scope for the state of the
//! roads every few minutes and gives the router the speeds it says, until it is dropped. Nothing
//! here runs, and nothing is downloaded, unless `[traffic] enabled` says so and the scope has a
//! source (`wanted`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use geko_module_sdk::scope::Scope;

use crate::config::TrafficConfig;
use crate::madrid;
use crate::serve::Loaded;
use crate::traffic::{FREE_FLOW, Traffic};

/// The sources of live traffic there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Madrid,
}

/// The sources to poll: none unless traffic is on, the data may be downloaded (not offline) and the
/// scope covers what a source measures.
pub fn wanted(config: &TrafficConfig, scope: &Scope, offline: bool) -> Vec<Source> {
    let mut sources = Vec::new();
    if config.enabled && !offline && config.madrid.enabled && covers_madrid(scope) {
        sources.push(Source::Madrid);
    }
    sources
}

fn covers_madrid(scope: &Scope) -> bool {
    // The community and the province are the same land, under two codes (countries.toml).
    scope.partitions().iter().any(|p| p.country.to_string() == "ES" && p.region.as_ref().is_none_or(|r| matches!(r.to_string().as_str(), "ES-MD" | "ES-M")))
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
    use geko_module_sdk::country::{CountryCode, RegionCode};

    fn scope(country: &str, regions: &[&str]) -> Scope {
        let mut scope = Scope::default();
        scope.add(CountryCode::new(country).unwrap(), regions.iter().map(|r| RegionCode::new(r).unwrap()).collect()).unwrap();
        scope
    }

    fn traffic(text: &str) -> TrafficConfig {
        Config::parse(text, "").unwrap().traffic
    }

    #[test]
    fn nothing_is_polled_unless_traffic_is_on_and_the_scope_has_a_source() {
        let on = traffic("[traffic]\nenabled = true");
        assert!(wanted(&traffic(""), &scope("ES", &[]), false).is_empty(), "off by default");
        assert_eq!(wanted(&on, &scope("ES", &[]), false), vec![Source::Madrid]);
        assert_eq!(wanted(&on, &scope("ES", &["ES-MD"]), false), vec![Source::Madrid]);
        assert_eq!(wanted(&on, &scope("ES", &["ES-M"]), false), vec![Source::Madrid], "the province, the same land");
        assert!(wanted(&on, &scope("ES", &["ES-IB"]), false).is_empty(), "Madrid is not in the scope");
        assert!(wanted(&on, &scope("LU", &[]), false).is_empty(), "no source in Luxembourg");
        assert!(wanted(&on, &scope("ES", &[]), true).is_empty(), "offline");
        assert!(wanted(&traffic("[traffic]\nenabled = true\n[traffic.madrid]\nenabled = false"), &scope("ES", &[]), false).is_empty());
    }
}
