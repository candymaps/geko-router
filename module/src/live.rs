//! Traffic, in the background: a thread for each live source of the scope, that asks it for the state
//! of the roads as often as it allows, and a clock that follows the hours of the historical profile.
//! What they say goes to a `Hub`, which puts it together, gives the router the speeds of every edge,
//! and leaves it all when it is dropped. Nothing here runs, and nothing is downloaded, unless
//! `[traffic] enabled` says so and the scope has a source (`wanted`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::pack::Source;
use crate::pipeline::Setup;
use crate::serve::Loaded;
use crate::traffic::{CLOSED, FREE_FLOW, Traffic};
use crate::{history, sensors, situations};

/// The sources of the scope of `kinds`, with what `[sources.<name>]` changes in them.
fn of_kinds(setup: &Setup, kinds: &[&str]) -> Vec<Source> {
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
                Ok(source) if source.enabled && kinds.contains(&source.kind.as_str()) && partitions.iter().any(|p| pack.covers(&source, p.region.as_ref())) => sources.push(source),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "a source of traffic is left out"),
            }
        }
    }
    sources
}

/// The sources to poll, from the packs of the scope: none unless traffic is on, the data may be
/// downloaded (not offline) and the scope holds what a source measures.
pub fn wanted(setup: &Setup) -> Vec<Source> {
    let traffic = &setup.config.traffic;
    if !(traffic.enabled && traffic.live) || setup.data.offline {
        return Vec::new();
    }
    of_kinds(setup, &[sensors::KIND, situations::KIND])
}

/// The sources the build learns the usual speeds from, with traffic on.
pub fn wanted_history(setup: &Setup) -> Vec<Source> {
    if !setup.config.traffic.enabled {
        return Vec::new();
    }
    of_kinds(setup, &[history::KIND])
}

/// The sources of sensors the build matches: with traffic on, those of the scope, polled or not.
pub fn wanted_sensors(setup: &Setup) -> Vec<Source> {
    if !setup.config.traffic.enabled {
        return Vec::new();
    }
    of_kinds(setup, &[sensors::KIND])
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

/// The speed of each edge a feed speaks of, and the date of its data.
type Said = (Vec<(u32, u16)>, Option<String>);

/// What reads the feed of a source, by its kind.
enum Provider {
    Sensors(Box<sensors::Matcher>),
    Situations(situations::Matcher),
}

impl Provider {
    fn of(source: &Source, loaded: &Loaded) -> Result<Self, String> {
        match source.kind.as_str() {
            sensors::KIND => Ok(Self::Sensors(Box::new(sensors::Matcher::seeded(sensors::Spec::of(source)?, loaded.sensors(&source.name))))),
            situations::KIND => Ok(Self::Situations(situations::Matcher::new(situations::Spec::of(source)?))),
            kind => Err(format!("source {}: no provider of kind {kind:?}", source.name)),
        }
    }

    /// The speed of each edge the text speaks of, and the date of the data; `None` if it has nothing usable.
    fn read(&mut self, text: &str, loaded: &Loaded) -> Option<Said> {
        match self {
            Self::Sensors(matcher) => {
                let spec = matcher.spec().clone();
                let readings = sensors::parse(&spec, text);
                if readings.is_empty() {
                    return None;
                }
                if let Some(stats) = matcher.add(loaded.graph(), loaded.snapper(), &readings) {
                    tracing::info!(sensors = readings.len(), matched = stats.matched, no_direction = stats.no_direction, no_street = stats.no_street, wrong_direction = stats.wrong_direction, "sensors matched to streets");
                }
                Some((matcher.speeds(&readings), sensors::updated(&spec, text)))
            }
            Self::Situations(matcher) => {
                let updated = situations::updated(text)?;
                let found = situations::parse(matcher.spec(), text, Utc::now());
                Some((matcher.speeds(&found, loaded), Some(updated)))
            }
        }
    }
}

/// What every source says now, put together. A live source decides over what is usual, and among
/// live sources the slowest does; a closed road stays closed.
pub struct Hub {
    loaded: Arc<Loaded>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    live: BTreeMap<String, Vec<(u32, u16)>>,
    slots: Vec<u16>,
    applied: Option<Traffic>,
}

impl Hub {
    pub fn new(loaded: Arc<Loaded>) -> Self {
        Self { loaded, state: Mutex::default() }
    }

    /// What `name` says now, `None` if it knows nothing any more. A change is applied, and only then.
    pub fn publish(&self, name: &str, edges: Option<Vec<(u32, u16)>>, read: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match edges {
            Some(edges) => state.live.insert(name.to_owned(), edges),
            None => state.live.remove(name),
        };
        self.apply(&mut state, read);
    }

    /// Follows the hours of the historical profile.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.apply(&mut state, Instant::now());
    }

    fn apply(&self, state: &mut State, read: Instant) {
        state.slots = self.loaded.slots(Utc::now());
        let traffic = self.loaded.usual(&state.slots);
        let traffic = combine(traffic, &state.live);
        if state.applied.as_ref() == Some(&traffic) || (state.applied.is_none() && traffic.known().iter().all(|k| *k == crate::traffic::Known::Nothing)) {
            self.loaded.touch(read);
            return;
        }
        self.loaded.apply(&traffic, read);
        let speeds = (0..traffic.edges() as u32).map(|e| traffic.speed(e));
        let (slowed, closed) = speeds.fold((0, 0), |(s, c), speed| (s + (speed < FREE_FLOW) as usize, c + (speed == CLOSED) as usize));
        tracing::info!(slowed_edges = slowed, closed_edges = closed, sources = state.live.len(), "traffic applied");
        state.applied = Some(traffic);
    }
}

/// `base` with what the live sources say on top of it.
fn combine(mut base: Traffic, live: &BTreeMap<String, Vec<(u32, u16)>>) -> Traffic {
    let mut edges: BTreeMap<u32, u16> = BTreeMap::new();
    for (edge, speed) in live.values().flatten() {
        edges.entry(*edge).and_modify(|s| *s = (*s).min(*speed)).or_insert(*speed);
    }
    edges.into_iter().for_each(|(edge, speed)| base.set(edge, speed));
    base
}

/// The running poll. Dropping it tells the threads to stop, which they do before their next round.
pub struct Poller {
    stop: Arc<AtomicBool>,
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What makes a source a thread.
pub struct Feed {
    pub source: Source,
    pub fetch: Fetch,
}

fn sleep_until(stop: &AtomicBool, wake: Instant) {
    while Instant::now() < wake && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200).min(wake.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
    }
}

fn poll(hub: &Hub, mut provider: Provider, feed: Feed, stop: &AtomicBool) {
    let Feed { source, mut fetch } = feed;
    let (interval, max_age) = (Duration::from_secs(source.refresh_secs), Duration::from_secs(source.ttl_secs));
    let mut last_ok: Option<Instant> = None;
    let mut speeds: Vec<(u32, u16)> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match fetch() {
            Ok(text) => match provider.read(&text, &hub.loaded) {
                Some((read, updated)) => {
                    speeds = read;
                    last_ok = Some(Instant::now());
                    hub.loaded.set_source_updated(&source.name, updated);
                }
                None => tracing::warn!(source = %source.name, "the feed has nothing usable"),
            },
            Err(e) => tracing::warn!(source = %source.name, error = %e, "cannot read the feed"),
        }
        // A reading that cannot be renewed in time is dropped: the road goes back to what is usual.
        let fresh = last_ok.is_some_and(|t| t.elapsed() <= max_age);
        hub.publish(&source.name, fresh.then(|| speeds.clone()), last_ok.unwrap_or_else(Instant::now));
        sleep_until(stop, Instant::now() + interval);
    }
}

/// Starts a thread for each of `feeds`, and the clock if the bundle has a profile. With neither there
/// is nothing to run.
pub fn start(loaded: Arc<Loaded>, feeds: Vec<Feed>, tick: Duration) -> Option<Poller> {
    let clock = loaded.has_profiles();
    if feeds.is_empty() && !clock {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let hub = Arc::new(Hub::new(Arc::clone(&loaded)));
    for feed in feeds {
        let provider = match Provider::of(&feed.source, &loaded) {
            Ok(provider) => provider,
            Err(e) => {
                tracing::error!(error = %e, "a source of traffic is left out");
                continue;
            }
        };
        let (hub, flag) = (Arc::clone(&hub), Arc::clone(&stop));
        let spawned = std::thread::Builder::new().name("geko-router-traffic".into()).spawn(move || poll(&hub, provider, feed, &flag));
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start a traffic thread");
        }
    }
    if clock {
        let flag = Arc::clone(&stop);
        let spawned = std::thread::Builder::new().name("geko-router-clock".into()).spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                hub.tick();
                sleep_until(&flag, Instant::now() + tick);
            }
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start the clock of traffic");
        }
    }
    Some(Poller { stop })
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
        let attribution = attribution(sources.iter().find(|s| s.name == "madrid-sensors").unwrap());
        assert_eq!(attribution["license"], "CC BY 4.0");
        assert_eq!(attribution["license_url"], "https://creativecommons.org/licenses/by/4.0/legalcode.es");
        assert!(attribution["notice"].as_str().unwrap().contains("Ayuntamiento de Madrid"));
        assert!(attribution["changes"].as_str().unwrap().starts_with("Modified"));
    }

    #[test]
    fn a_source_changes_in_the_configuration_and_the_incidents_are_off_unless_asked_for() {
        assert_eq!(names("[traffic]\nenabled = true", "ES", &[], false), ["madrid-sensors"]);
        let sources = wanted(&setup("[traffic]\nenabled = true\n[sources.madrid-sensors]\nrefresh_secs = 600\nttl_secs = 1200\n[sources.dgt-incidents]\nenabled = true", "ES", &[], false));
        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["dgt-incidents", "madrid-sensors"]);
        assert_eq!((sources[1].refresh_secs, sources[1].ttl_secs), (600, 1200));
    }

    #[test]
    fn the_history_is_of_a_build_with_traffic_on_and_of_the_land_it_measures() {
        let history = |config, regions: &[&str]| wanted_history(&setup(config, "ES", regions, false)).into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert!(history("", &[]).is_empty());
        assert_eq!(history("[traffic]\nenabled = true\nlive = false", &[]), ["madrid-history"]);
        assert!(history("[traffic]\nenabled = true", &["ES-IB"]).is_empty());
    }

    #[test]
    fn a_live_source_decides_over_what_is_usual_and_the_slowest_among_them() {
        let mut usual = Traffic::free(4);
        usual.set_historical(0, 500);
        usual.set_historical(1, 500);
        let live: BTreeMap<String, Vec<(u32, u16)>> = [("a".to_owned(), vec![(1, 900), (2, 700)]), ("b".to_owned(), vec![(2, 400), (3, CLOSED)])].into();
        let traffic = combine(usual, &live);
        assert_eq!((0..4).map(|e| traffic.speed(e)).collect::<Vec<_>>(), [500, 900, 400, CLOSED]);
        use crate::traffic::Known::*;
        assert_eq!(traffic.known(), [Historical, Live, Live, Live]);
    }
}
