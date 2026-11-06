//! What the router does for `geko plan`, `fetch` and `build`: bring the country packs of the scope,
//! download its OSM extracts, read each into a fragment with the profile of its country, join them
//! into the road graph and work out its hierarchy, its part of the bundle with the graph. Each stage
//! leaves a stamp beside its folder and is skipped when neither its code nor its inputs changed
//! (PROJECT.md §8 of the host), so a new extract repeats the reading of that extract alone, and a
//! graph that comes out the same repeats no hierarchy.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use geko_artifact::registry::Outcome;
use geko_fetch::{HttpDownloader, HttpDownloaderConfig, RefreshPolicy};
use geko_module_sdk::bundle::LicenseNotice;
use geko_module_sdk::country::CountryCode;
use geko_module_sdk::download::{Checksum, DownloadRequest, Downloader};
use geko_module_sdk::incremental::{self, Key};
use geko_module_sdk::module::{BundlePart, Built, DataPolicy, Refresh};
use geko_module_sdk::scope::Scope;
use geko_module_sdk::stages::StageLog;
use geko_module_sdk::{GekoError, Result};

use crate::cch::Cch;
use crate::config::Config;
use crate::live;
use crate::extracts::{self, Catalog};
use crate::graph::{Builder, GeoBox, Graph, Roads};
use crate::history::{self, Bundled, Holidays, Profile, Sums};
use crate::pack::{Packs, Source};
use crate::profile::Vehicle;
use crate::scc;
use crate::sensors;
use crate::serve::MIN_COMPONENT;
use crate::snap::Snapper;
use crate::turn::{self, TurnProfile};
use crate::{graph_io, osm};

/// The versions of the code of the stages: raising one repeats its stage.
const FRAGMENT_CODE: &str = "1";
const GRAPH_CODE: &str = "5";
const HIERARCHY_CODE: &str = "1";
const TRAFFIC_CODE: &str = "1";

/// Where the graph goes in the bundle.
pub const GRAPH_PART: &str = "router/graph";

pub const GRAPH_FILE: &str = "graph.bin";

/// Where the order and topology of the hierarchy of queries go in the bundle.
pub const HIERARCHY_PART: &str = "router/hierarchy";

pub const HIERARCHY_FILE: &str = "cch.bin";

/// Where the sensors matched and the historical profiles go in the bundle, with traffic on.
pub const TRAFFIC_PART: &str = "router/traffic";

const FRAGMENT_FILE: &str = "fragment.bin";

pub struct Setup {
    pub config: Config,
    pub scope: Scope,
    pub data_dir: PathBuf,
    pub data: DataPolicy,
    pub packs: Packs,
}

impl Setup {
    pub fn work(&self) -> PathBuf {
        self.data_dir.join("router")
    }

    fn graph_dir(&self) -> PathBuf {
        self.work().join("graph")
    }

    fn hierarchy_dir(&self) -> PathBuf {
        self.work().join("hierarchy")
    }

    fn traffic_dir(&self) -> PathBuf {
        self.work().join("traffic")
    }

    /// Where the feeds and files of the sources of traffic are kept.
    fn sources_dir(&self) -> PathBuf {
        self.data_dir.join("raw").join("router")
    }

    fn fragments_dir(&self) -> PathBuf {
        self.work().join("fragments")
    }

    /// A source of a pack with what `[sources.<name>]` changes in it.
    pub fn source(&self, source: &Source) -> std::result::Result<Source, String> {
        match self.config.sources.get(&source.name) {
            Some(changes) => source.with(changes).map_err(|why| format!("[sources.{}]: {why}", source.name)),
            None => Ok(source.clone()),
        }
    }

    /// What can only be checked with the packs at hand: that the scope has them, that they have the
    /// profile, and that `[sources.<name>]` names a source of theirs and fields it has.
    fn check(&self) -> Result<()> {
        let mut known = std::collections::BTreeSet::new();
        for country in self.scope.countries() {
            let pack = self.packs.get(country)?;
            if !pack.profiles.contains(&self.config.profiles.default) {
                return Err(GekoError::Config(format!("geko-router.toml: [profiles] default: the pack of {country} has no profile {:?}", self.config.profiles.default)));
            }
            for source in pack.sources.values() {
                self.source(source).map_err(|why| GekoError::Config(format!("geko-router.toml: {why}")))?;
                known.insert(source.name.clone());
            }
        }
        if !self.scope.is_empty()
            && let Some(name) = self.config.sources.keys().find(|n| !known.contains(*n))
        {
            return Err(GekoError::Config(format!("geko-router.toml: [sources.{name}]: no country pack of the scope has that source")));
        }
        Ok(())
    }

    fn base(&self) -> String {
        match self.config.osm.mirror.as_str() {
            "auto" => extracts::DEFAULT_BASE.to_owned(),
            mirror => mirror.trim_end_matches('/').to_owned(),
        }
    }

    /// The OSM extracts go where the geocoder and the tiles keep them, so the three share them.
    fn osm_dir(&self) -> PathBuf {
        self.data_dir.join("raw").join("osm")
    }
}

/// With `refresh = "if-changed"`, the sources are asked for changes only when the last check is
/// older than `[data] max_age_days`, or when `refresh_now` (`geko build --refresh`).
pub fn downloader(setup: &Setup, refresh_now: bool) -> HttpDownloader {
    HttpDownloader::new(HttpDownloaderConfig {
        policy: match setup.data.refresh {
            Refresh::Never => RefreshPolicy::Never,
            Refresh::IfChanged if refresh_now || check_due(setup) => RefreshPolicy::IfChanged,
            Refresh::IfChanged => RefreshPolicy::Never,
            Refresh::Always => RefreshPolicy::Always,
        },
        offline: setup.data.offline,
        ..Default::default()
    })
}

fn checked_path(setup: &Setup) -> PathBuf {
    setup.work().join(".checked")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn since_check(setup: &Setup) -> Option<u64> {
    let checked = fs::read_to_string(checked_path(setup)).ok()?.trim().parse::<u64>().ok()?;
    Some(now_unix().saturating_sub(checked))
}

fn check_due(setup: &Setup) -> bool {
    since_check(setup).is_none_or(|s| s >= setup.data.max_age_days.saturating_mul(86_400))
}

fn mark_checked(setup: &Setup) -> Result<()> {
    fs::create_dir_all(setup.work())?;
    fs::write(checked_path(setup), now_unix().to_string())?;
    Ok(())
}

/// The shortest time between two builds of `geko up`.
const MIN_WAIT: std::time::Duration = std::time::Duration::from_secs(3600);

/// How long until `geko up` builds again: when the sources are due for a check. Offline, or with
/// `refresh = "never"`, never.
pub fn next_build(setup: &Setup) -> Option<std::time::Duration> {
    if setup.data.offline || setup.data.refresh == Refresh::Never || setup.scope.is_empty() {
        return None;
    }
    let max = setup.data.max_age_days.saturating_mul(86_400);
    let until = std::time::Duration::from_secs(since_check(setup).map_or(0, |s| max.saturating_sub(s)));
    Some(until.max(MIN_WAIT))
}

fn osm_license() -> LicenseNotice {
    LicenseNotice {
        source: "osm".into(),
        name: "ODbL 1.0".into(),
        attribution: "© OpenStreetMap contributors".into(),
        url: Some("https://www.openstreetmap.org/copyright".into()),
    }
}

/// The licenses of what the router reads with this configuration: OpenStreetMap, and the sources
/// of traffic only if they are used.
pub fn licenses(setup: &Setup) -> Vec<LicenseNotice> {
    let mut list = vec![osm_license()];
    let mut sources = live::wanted(setup);
    sources.extend(live::wanted_history(setup));
    list.extend(sources.into_iter().map(|s| LicenseNotice { source: s.name, name: s.license.name, attribution: s.license.attribution, url: Some(s.license.url) }));
    list
}

const LAST_BUILD: &str = "last_build.router.json";

/// Writes the stages of this build for the dashboard: the report of the host is one file that every
/// module's build overwrites, so each module keeps its own.
fn write_last_build(data_dir: &Path, stages: &[geko_module_sdk::stages::StageRecord]) {
    let finished = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let report = serde_json::json!({ "finished_unix": finished, "stages": stages });
    if let Err(e) = std::fs::write(data_dir.join(LAST_BUILD), report.to_string()) {
        tracing::warn!(error = %e, "cannot write the stages of this build");
    }
}

/// The stages of the last build of this module, as `write_last_build` left them; `null` before one.
pub fn last_build(data_dir: &Path) -> serde_json::Value {
    std::fs::read(data_dir.join(LAST_BUILD)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Before `plan`, `fetch`, `build` and `purge`: the packs of the scope, downloaded or checked.
pub fn prepare(setup: &Setup) -> Result<String> {
    let mut out = String::new();
    for country in setup.scope.countries() {
        let what = match setup.packs.ensure(country, &setup.config.packs.registry, setup.data.offline)? {
            Outcome::Downloaded => "downloaded".to_owned(),
            Outcome::UpToDate => "up to date".to_owned(),
            Outcome::Local(why) => format!("local copy ({why})"),
        };
        writeln!(out, "router pack {:<4} version {}, {what}", country.lowercase(), setup.packs.get(country)?.version).unwrap();
    }
    setup.check()?;
    Ok(out)
}

/// Downloading needs the licenses of the sources accepted (`[data] accept_licenses`).
pub fn licenses_accepted(setup: &Setup) -> Result<()> {
    if setup.scope.is_empty() || setup.data.accept_licenses {
        return Ok(());
    }
    let list: Vec<String> = licenses(setup).iter().map(|l| format!("{} ({})", l.source, l.name)).collect();
    Err(GekoError::Config(format!("downloading requires accepting the licenses of the sources of the router: {}. Set [data] accept_licenses = true", list.join(", "))))
}

fn catalog(setup: &Setup, dl: &dyn Downloader) -> Result<Catalog> {
    let path = setup.osm_dir().join(extracts::INDEX_FILE);
    dl.fetch(&DownloadRequest { url: format!("{}/{}", setup.base(), extracts::INDEX_FILE), checksum: None }, &path)?;
    let countries = setup.scope.countries().map(|c| Ok((c.lowercase(), setup.packs.get(c)?.osm.clone()))).collect::<Result<_>>()?;
    Catalog::parse(&fs::read(&path)?, countries)
}

/// What is read of an extract: the country whose profile applies, and the boxes that hold the
/// regions of the scope it serves; `None`, all of it.
#[derive(Debug, Clone, PartialEq)]
struct Job {
    country: CountryCode,
    clip: Option<Vec<GeoBox>>,
}

/// The extracts of the scope by id, the order they are joined in. Regions of the same extract share
/// its reading, and a part of the scope that takes all of an extract takes it for all.
fn jobs(setup: &Setup, catalog: &Catalog) -> Result<BTreeMap<String, Job>> {
    let mut jobs: BTreeMap<String, Job> = BTreeMap::new();
    for partition in setup.scope.partitions() {
        let pack = setup.packs.get(partition.country)?;
        let clip = match &partition.region {
            Some(region) => {
                let boxes = pack.boxes_of(region)?;
                if boxes.is_none() {
                    tracing::warn!(%region, "the country pack has no box for the region: all of its extract is read");
                }
                boxes
            }
            None => None,
        };
        for id in catalog.partition_ids(&partition)? {
            match jobs.get_mut(&id) {
                Some(job) => match (&mut job.clip, &clip) {
                    (Some(boxes), Some(more)) => boxes.extend(more.iter().copied()),
                    _ => job.clip = None,
                },
                None => {
                    jobs.insert(id, Job { country: partition.country, clip: clip.clone() });
                }
            }
        }
    }
    Ok(jobs)
}

pub fn plan(setup: &Setup, dl: &dyn Downloader) -> String {
    let mut out = format!("Router: the road graph of the scope, for the profile {}, from OpenStreetMap\n", setup.config.profiles.default);
    if setup.scope.is_empty() {
        out.push_str("  nothing to build: the scope is empty\n");
        return out;
    }
    out.push_str("OSM extracts (shared with the geocoder and the tiles):\n");
    let list = catalog(setup, dl).and_then(|c| {
        let jobs = jobs(setup, &c)?;
        jobs.iter().map(|(id, job)| Ok((c.get(id)?.file_name(), c.get(id)?.url(&setup.base()), job.clip.is_some()))).collect::<Result<Vec<_>>>()
    });
    match list {
        Ok(list) => list.iter().for_each(|(file, url, clipped)| writeln!(out, "  {file:<40} {url}{}", if *clipped { " (cut to the regions of the scope)" } else { "" }).unwrap()),
        Err(e) => writeln!(out, "  cannot plan: {e}").unwrap(),
    }
    out.push_str("Licenses:\n");
    for l in licenses(setup) {
        writeln!(out, "  {:<14} {}: {}", l.source, l.name, l.attribution).unwrap();
    }
    if live::wanted(setup).is_empty() {
        out.push_str("Live traffic: off\n");
    } else {
        out.push_str("Live traffic: on, polled in the background by `geko serve`\n");
    }
    for source in live::wanted_sensors(setup) {
        writeln!(out, "  {:<14} its sensors are matched to the graph by `build`", source.name).unwrap();
    }
    for source in live::wanted_history(setup) {
        match history::Spec::of(&source) {
            Ok(spec) => writeln!(out, "  {:<14} the usual speeds are learned by `build`, from {} files", source.name, spec.files.len()).unwrap(),
            Err(e) => writeln!(out, "  {e}").unwrap(),
        }
    }
    if !setup.data.accept_licenses {
        out.push_str("Downloads are blocked until [data] accept_licenses = true (review the licenses above).\n");
    }
    out
}

/// What a fetch leaves: each extract by its id and path, with the identity the source gave it.
#[derive(Default)]
struct Fetched {
    report: String,
    extracts: Vec<(String, PathBuf, Job)>,
    identities: BTreeMap<PathBuf, String>,
}

pub fn fetch(setup: &Setup, dl: &dyn Downloader) -> Result<String> {
    fetched(setup, dl).map(|f| f.report)
}

fn fetched(setup: &Setup, dl: &dyn Downloader) -> Result<Fetched> {
    licenses_accepted(setup)?;
    let mut f = Fetched::default();
    if setup.scope.is_empty() {
        return Ok(f);
    }
    let catalog = catalog(setup, dl)?;
    let (mut downloaded, mut kept) = (0, 0);
    for (id, job) in jobs(setup, &catalog)? {
        let e = catalog.get(&id)?;
        let file = e.file_name();
        let url = e.url(&setup.base());
        let md5_path = setup.osm_dir().join(format!("{file}.md5"));
        dl.fetch(&DownloadRequest { url: format!("{url}.md5"), checksum: None }, &md5_path)?;
        let md5 = fs::read_to_string(&md5_path)?
            .split_whitespace()
            .next()
            .map(str::to_owned)
            .ok_or_else(|| GekoError::Data(format!("empty checksum file {}", md5_path.display())))?;
        let path = setup.osm_dir().join(&file);
        let outcome = dl
            .fetch(&DownloadRequest { url, checksum: Some(Checksum::Md5(md5)) }, &path)
            .map_err(|e| GekoError::Data(format!("{e} (if Geofabrik published a new extract meanwhile, run again)")))?;
        if outcome.downloaded { downloaded += 1 } else { kept += 1 }
        f.identities.insert(path.clone(), format!("sha256 {}", outcome.sha256));
        f.extracts.push((id, path, job));
    }
    if dl.checks_for_updates() {
        mark_checked(setup)?;
    }
    writeln!(f.report, "downloads of the router: {downloaded} downloaded, {kept} up to date").unwrap();
    Ok(f)
}

/// The stage of an extract: its usable roads, with the profile of its country, cut to the scope.
/// Returns the digest of what it left, and whether it was already there.
fn fragment_stage(setup: &Setup, fetched: &Fetched, (id, path, job): &(String, PathBuf, Job), force: bool) -> Result<(String, bool)> {
    let pack = setup.packs.get(job.country)?;
    let dir = setup.fragments_dir().join(id);
    let clip = job.clip.as_ref().map_or_else(|| "all".to_owned(), |boxes| format!("{boxes:?}"));
    let key = Key::new("fragment")
        .add("code", FRAGMENT_CODE)
        .add("profile", &setup.config.profiles.default)
        .add("road-classes", pack.classes(&setup.config.profiles.default))
        .add("clip", &clip)
        .inputs(std::slice::from_ref(path), &fetched.identities)?
        .finish();
    if let Some(stamp) = incremental::up_to_date(&dir, &key).filter(|_| !force) {
        return Ok((stamp.output, true));
    }
    fresh_dir(&dir)?;
    let mut builder = Builder::clipped(Vehicle::of(setup.config.mode(), pack.classes(&setup.config.profiles.default))?, job.clip.clone());
    osm::read_pbf(path, |e| builder.first_pass(&e))?;
    osm::read_pbf(path, |e| builder.second_pass(&e))?;
    let fragment = builder.fragment();
    graph_io::write_fragment(&fragment, &dir.join(FRAGMENT_FILE))?;
    let stamp = incremental::seal(&dir, key, &[], serde_json::json!({ "ways": fragment.ways(), "nodes": fragment.nodes() }))?;
    Ok((stamp.output, false))
}

/// An empty `dir` for a stage to write in, marked out of date until the stage seals it.
fn fresh_dir(dir: &Path) -> Result<()> {
    incremental::invalidate(dir)?;
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)?;
    Ok(())
}

/// Removes the fragments of the extracts the scope no longer uses.
fn prune_fragments(setup: &Setup, fetched: &Fetched) -> Result<()> {
    let Ok(entries) = fs::read_dir(setup.fragments_dir()) else { return Ok(()) };
    for entry in entries {
        let dir = entry?.path();
        if dir.is_dir() && !fetched.extracts.iter().any(|(id, ..)| dir.file_name().is_some_and(|n| n == id.as_str())) {
            incremental::invalidate(&dir)?;
            fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// A file of a source of traffic, downloaded to `raw/router/<source>/`, with its digest.
fn source_file(setup: &Setup, dl: &dyn Downloader, source: &Source, url: &str) -> Result<(PathBuf, String)> {
    let name = url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("feed");
    let path = setup.sources_dir().join(&source.name).join(name);
    let outcome = dl.fetch(&DownloadRequest { url: url.to_owned(), checksum: None }, &path)?;
    Ok((path, outcome.sha256))
}

/// The stage of traffic, with traffic on: the sensors of the feeds matched to the graph, once and not
/// while serving, and the usual speeds of those edges. `None` when no source of the scope needs it.
/// Returns what it left and whether it was already there.
fn traffic_stage(setup: &Setup, dl: &dyn Downloader, graph_digest: &str, graph_file: &Path, force: bool) -> Result<Option<(PathBuf, serde_json::Value, bool)>> {
    let sensor_sources = live::wanted_sensors(setup);
    let history_sources = live::wanted_history(setup);
    if sensor_sources.is_empty() && history_sources.is_empty() {
        return Ok(None);
    }
    let mut key = Key::new("traffic").add("code", TRAFFIC_CODE).add("graph", graph_digest);
    let mut feeds = Vec::new();
    for source in &sensor_sources {
        let (path, digest) = source_file(setup, dl, source, &source.url)?;
        key = key.add(&source.name, &digest).add(&format!("{}.spec", source.name), &format!("{:?}", source.table));
        feeds.push((source, path));
    }
    let mut files = Vec::new();
    for source in &history_sources {
        let spec = history::Spec::of(source).map_err(GekoError::Data)?;
        let mut paths = Vec::new();
        for url in &spec.files {
            let (path, digest) = source_file(setup, dl, source, url)?;
            key = key.add(url, &digest);
            paths.push(path);
        }
        key = key.add(&format!("{}.spec", source.name), &format!("{:?}", source.table));
        files.push((source, spec, paths));
    }
    for country in setup.scope.countries() {
        let pack = setup.packs.get(country)?;
        key = key.add(&format!("{country}.calendar"), &format!("{:?} {}", pack.calendar, pack.timezone));
    }
    let key = key.finish();
    let dir = setup.traffic_dir();
    if let Some(stamp) = incremental::up_to_date(&dir, &key).filter(|_| !force) {
        return Ok(Some((dir, stamp.info, true)));
    }
    fresh_dir(&dir)?;
    let graph = graph_io::read(graph_file)?;
    let turns = turn::build(&graph, &TurnProfile::of(setup.config.mode()));
    let snapper = Snapper::new(&graph, graph.without_ferries(scc::in_large_component(&turns, MIN_COMPONENT)));
    let mut bundled = Bundled::default();
    let mut matched_sensors = 0;
    for (source, path) in feeds {
        let spec = sensors::Spec::of(source).map_err(GekoError::Data)?;
        let readings = sensors::parse(&spec, &fs::read_to_string(&path)?);
        let (matched, stats) = sensors::match_sensors(&spec, &graph, &snapper, &readings);
        tracing::info!(source = %source.name, sensors = readings.len(), matched = stats.matched, no_direction = stats.no_direction, no_street = stats.no_street, wrong_direction = stats.wrong_direction, "sensors matched to streets");
        matched_sensors += matched.len();
        bundled.sensors.insert(source.name.clone(), matched);
    }
    let mut profiled_edges = 0;
    for (source, spec, paths) in files {
        let Some(sensor_source) = sensor_sources.iter().find(|s| s.name == spec.sensors) else {
            tracing::warn!(source = %source.name, sensors = %spec.sensors, "no usual speeds: its sensors are not in use");
            continue;
        };
        let levels = sensors::Spec::of(sensor_source).map_err(GekoError::Data)?;
        let matched = bundled.sensors.get(&spec.sensors).cloned().unwrap_or_default();
        let country = setup.scope.countries().find(|c| setup.packs.get(*c).is_ok_and(|p| p.sources.contains_key(&source.name)));
        let pack = setup.packs.get(country.ok_or_else(|| GekoError::Data(format!("no pack has the source {}", source.name)))?)?;
        let holidays = Holidays::of(&pack.calendar);
        let mut sums = Sums::default();
        for path in paths {
            let used = history::read_zip(&path, |csv| sums.add(&spec, &levels, &holidays, &matched, csv))?;
            tracing::info!(source = %source.name, file = %path.display(), measurements = used, "usual speeds read");
        }
        let profile = Profile::new(&pack.timezone, holidays, sums.profile(&matched))?;
        profiled_edges += profile.edges();
        bundled.profiles.insert(source.name.clone(), profile);
    }
    bundled.write(&dir)?;
    let info = serde_json::json!({ "sensors": matched_sensors, "profiled_edges": profiled_edges });
    let stamp = incremental::seal(&dir, key, &[], info)?;
    Ok(Some((dir, stamp.info, false)))
}

pub fn build(setup: &Setup, dl: &dyn Downloader, force: bool) -> Result<Built> {
    licenses_accepted(setup)?;
    if setup.scope.is_empty() {
        return Ok(Built::default());
    }
    let mut log = StageLog::new();
    let fetched = fetched(setup, dl)?;
    log.done("fetch", false);

    let mut graph_key = Key::new("graph").add("code", GRAPH_CODE);
    for extract in &fetched.extracts {
        let (output, reused) = fragment_stage(setup, &fetched, extract, force)?;
        graph_key = graph_key.add(&extract.0, &output);
        log.done(format!("fragment {}", extract.0), reused);
    }
    prune_fragments(setup, &fetched)?;

    let dir = setup.graph_dir();
    let key = graph_key.finish();
    let mut graph: Option<Graph> = None;
    let (stamp, graph_reused) = match incremental::up_to_date(&dir, &key).filter(|_| !force) {
        Some(stamp) => (stamp, true),
        None => {
            fresh_dir(&dir)?;
            let mut roads = Roads::default();
            for (id, ..) in &fetched.extracts {
                roads.add(graph_io::read_fragment(&setup.fragments_dir().join(id).join(FRAGMENT_FILE))?);
            }
            let joined = roads.finish();
            graph_io::write(&joined, &dir.join(GRAPH_FILE))?;
            let stamp = incremental::seal(&dir, key, &[], serde_json::json!({ "nodes": joined.nodes.len(), "edges": joined.edges.len() }))?;
            graph = Some(joined);
            (stamp, false)
        }
    };
    log.done("graph", graph_reused);
    let count = |stamp: &incremental::Stamp, key: &str| stamp.info[key].as_u64().unwrap_or(0) as usize;
    let (nodes, edges) = (count(&stamp, "nodes"), count(&stamp, "edges"));

    let hierarchy = setup.hierarchy_dir();
    let key = Key::new("hierarchy").add("code", HIERARCHY_CODE).add("profile", &setup.config.profiles.default).add("graph", &stamp.output).finish();
    let (arcs, hierarchy_reused) = match incremental::up_to_date(&hierarchy, &key).filter(|_| !force) {
        Some(stamp) => (count(&stamp, "arcs"), true),
        None => {
            let graph = match graph {
                Some(graph) => graph,
                None => graph_io::read(&dir.join(GRAPH_FILE))?,
            };
            fresh_dir(&hierarchy)?;
            let turns = turn::build(&graph, &TurnProfile::of(setup.config.mode()));
            Cch::topology(&turns).write(&turns, &hierarchy.join(HIERARCHY_FILE))?;
            incremental::seal(&hierarchy, key, &[], serde_json::json!({ "arcs": turns.arcs.len() }))?;
            (turns.arcs.len(), false)
        }
    };
    log.done("hierarchy", hierarchy_reused);

    let traffic = traffic_stage(setup, dl, &stamp.output, &dir.join(GRAPH_FILE), force)?;

    let mut report = fetched.report;
    writeln!(report, "graph: {nodes} nodes, {edges} edges, {arcs} turns{}", if graph_reused && hierarchy_reused { " (up to date)" } else { "" }).unwrap();
    let mut parts = vec![BundlePart { from: dir, to: GRAPH_PART.into() }, BundlePart { from: hierarchy, to: HIERARCHY_PART.into() }];
    // What license each part goes under: the graph is a Derived Database of OSM, and what the traffic
    // part holds comes from the sources of traffic, on edges of that graph.
    let mut part_licenses = serde_json::json!({ GRAPH_PART: [osm_license().name], HIERARCHY_PART: [osm_license().name] });
    if let Some((from, info, reused)) = traffic {
        log.done("traffic", reused);
        writeln!(report, "traffic: {} sensors matched, {} edges with usual speeds", info["sensors"], info["profiled_edges"]).unwrap();
        parts.push(BundlePart { from, to: TRAFFIC_PART.into() });
        let mut names: Vec<String> = live::wanted_sensors(setup).into_iter().chain(live::wanted_history(setup)).map(|s| s.license.name).collect();
        names.push(osm_license().name);
        names.sort();
        names.dedup();
        part_licenses[TRAFFIC_PART] = names.into();
    }
    let packs: serde_json::Map<String, serde_json::Value> = setup.scope.countries().map(|c| Ok((c.to_string(), setup.packs.get(c)?.version.into()))).collect::<Result<_>>()?;
    let stages = log.into_records();
    write_last_build(&setup.data_dir, &stages);
    Ok(Built {
        report,
        parts,
        meta: serde_json::json!({ "nodes": nodes, "edges": edges, "arcs": arcs, "packs": packs, "part_licenses": part_licenses }),
        licenses: licenses(setup),
        stages,
        report_extra: serde_json::Map::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_module_sdk::country::RegionCode;

    const INDEX: &str = r#"{ "features": [
        { "properties": { "id": "andalucia", "parent": "spain", "urls": { "pbf": "https://download.geofabrik.de/europe/spain/andalucia-latest.osm.pbf" } } },
        { "properties": { "id": "spain", "iso3166-1:alpha2": ["ES"], "urls": { "pbf": "https://download.geofabrik.de/europe/spain-latest.osm.pbf" } } },
        { "properties": { "id": "canary-islands", "urls": { "pbf": "https://download.geofabrik.de/africa/canary-islands-latest.osm.pbf" } } },
        { "properties": { "id": "luxembourg", "iso3166-1:alpha2": ["LU"], "urls": { "pbf": "https://download.geofabrik.de/europe/luxembourg-latest.osm.pbf" } } }
    ] }"#;

    fn setup(scope: &[(&str, &[&str])], config: &str) -> Setup {
        let mut s = Scope::default();
        for (country, regions) in scope {
            s.add(CountryCode::new(country).unwrap(), regions.iter().map(|r| RegionCode::new(r).unwrap()).collect()).unwrap();
        }
        let packs = Packs::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries"), true);
        Setup { config: Config::parse(config, "").unwrap(), scope: s, data_dir: "/nowhere".into(), data: DataPolicy::default(), packs }
    }

    fn jobs_of(setup: &Setup) -> Result<BTreeMap<String, Job>> {
        let countries = setup.scope.countries().map(|c| (c.lowercase(), setup.packs.get(c).unwrap().osm.clone())).collect();
        jobs(setup, &Catalog::parse(INDEX.as_bytes(), countries)?)
    }

    #[test]
    fn a_scope_of_provinces_reads_their_extract_cut_to_their_boxes() {
        let jobs = jobs_of(&setup(&[("ES", &["ES-SE", "ES-MA"])], "")).unwrap();
        assert_eq!(jobs.keys().collect::<Vec<_>>(), ["andalucia"]);
        assert_eq!(jobs["andalucia"].clip.as_ref().unwrap().len(), 2, "one reading for both");
    }

    #[test]
    fn a_community_is_the_boxes_of_its_provinces() {
        let jobs = jobs_of(&setup(&[("ES", &["ES-AN"])], "")).unwrap();
        assert_eq!(jobs["andalucia"].clip.as_ref().unwrap().len(), 8);
    }

    #[test]
    fn a_country_reads_all_of_its_extracts_each_with_its_country() {
        let jobs = jobs_of(&setup(&[("ES", &[]), ("LU", &[])], "")).unwrap();
        assert_eq!(jobs.keys().collect::<Vec<_>>(), ["canary-islands", "luxembourg", "spain"]);
        assert!(jobs.values().all(|j| j.clip.is_none()));
        assert_eq!(jobs["luxembourg"].country.as_str(), "LU");
        assert_eq!(jobs["spain"].country.as_str(), "ES");
    }

    #[test]
    fn a_region_the_pack_does_not_know_is_an_error() {
        assert!(jobs_of(&setup(&[("ES", &["ES-XX"])], "")).unwrap_err().to_string().contains("ES-XX"));
        assert!(jobs_of(&setup(&[("LU", &["LU-CA"])], "")).unwrap_err().to_string().contains("LU-CA"));
    }

    #[test]
    fn the_configuration_is_checked_against_the_packs() {
        assert!(setup(&[("ES", &[])], "[sources.madrid-sensors]\nenabled = false").check().is_ok());
        let unknown = setup(&[("LU", &[])], "[sources.madrid-sensors]\nenabled = false").check().unwrap_err().to_string();
        assert!(unknown.contains("geko-router.toml") && unknown.contains("madrid-sensors"), "{unknown}");
        let key = setup(&[("ES", &[])], "[sources.madrid-sensors]\nenable = false").check().unwrap_err().to_string();
        assert!(key.contains("geko-router.toml") && key.contains("[sources.madrid-sensors]") && key.contains("enable"), "{key}");
        assert!(setup(&[("FR", &[])], "").check().unwrap_err().to_string().contains("no country pack of the router for FR"));
    }

    #[test]
    fn the_licenses_are_those_of_the_pack() {
        let on = setup(&[("ES", &[])], "[traffic]\nenabled = true");
        let list = licenses(&on);
        assert_eq!(list.iter().map(|l| l.source.as_str()).collect::<Vec<_>>(), ["osm", "madrid-sensors", "madrid-history"]);
        assert_eq!(list[1].name, "CC BY 4.0");
        assert_eq!(licenses(&setup(&[("ES", &[])], "")).len(), 1, "without traffic, only OSM");
    }
}
