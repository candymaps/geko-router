//! What the router does for `geko plan`, `fetch` and `build`: download the OSM extracts of the
//! scope, read them into the road graph and leave it as its part of the bundle. The stage leaves a
//! stamp beside its folder and is skipped when neither its code nor its inputs changed
//! (PROJECT.md §8 of the host).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use geko_fetch::{HttpDownloader, HttpDownloaderConfig, RefreshPolicy};
use geko_module_sdk::bundle::LicenseNotice;
use geko_module_sdk::download::{Checksum, DownloadRequest, Downloader};
use geko_module_sdk::incremental::{self, Key};
use geko_module_sdk::module::{BundlePart, Built, DataPolicy, Refresh};
use geko_module_sdk::scope::Scope;
use geko_module_sdk::stages::StageLog;
use geko_module_sdk::{GekoError, Result};

use crate::config::Config;
use crate::extracts::{self, Catalog};
use crate::graph::Builder;
use crate::profile::Car;
use crate::{graph_io, osm};

/// The version of the code of the graph stage: raising it repeats the stage.
const GRAPH_CODE: &str = "1";

/// The road classes a car drives on, until the country packs bring their own.
const ROAD_CLASSES: &str = include_str!("../data/road-classes.toml");

/// Where the graph goes in the bundle.
pub const GRAPH_PART: &str = "router/graph";

pub const GRAPH_FILE: &str = "graph.bin";

pub struct Setup {
    pub config: Config,
    pub scope: Scope,
    pub data_dir: PathBuf,
    pub data: DataPolicy,
}

impl Setup {
    pub fn work(&self) -> PathBuf {
        self.data_dir.join("router")
    }

    fn graph_dir(&self) -> PathBuf {
        self.work().join("graph")
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

/// Downloading needs the licenses of the sources accepted (`[data] accept_licenses`).
pub fn licenses_accepted(setup: &Setup) -> Result<()> {
    if setup.scope.is_empty() || setup.data.accept_licenses {
        return Ok(());
    }
    let l = osm_license();
    Err(GekoError::Config(format!("downloading requires accepting the licenses of the sources of the router: {} ({}). Set [data] accept_licenses = true", l.source, l.name)))
}

fn catalog(setup: &Setup, dl: &dyn Downloader) -> Result<Catalog> {
    let path = setup.osm_dir().join(extracts::INDEX_FILE);
    dl.fetch(&DownloadRequest { url: format!("{}/{}", setup.base(), extracts::INDEX_FILE), checksum: None }, &path)?;
    Catalog::parse(&fs::read(&path)?)
}

pub fn plan(setup: &Setup, dl: &dyn Downloader) -> String {
    let mut out = String::from("Router: the road graph of the scope, for cars, from OpenStreetMap\n");
    if setup.scope.is_empty() {
        out.push_str("  nothing to build: the scope is empty\n");
        return out;
    }
    out.push_str("OSM extracts (shared with the geocoder and the tiles):\n");
    match catalog(setup, dl).and_then(|c| Ok(c.for_scope(&setup.scope)?.into_iter().map(|e| (e.file_name(), e.url(&setup.base()))).collect::<Vec<_>>())) {
        Ok(list) => list.iter().for_each(|(file, url)| writeln!(out, "  {file:<40} {url}").unwrap()),
        Err(e) => writeln!(out, "  cannot plan: {e}").unwrap(),
    }
    let l = osm_license();
    writeln!(out, "Licenses:\n  {:<14} {}: {}", l.source, l.name, l.attribution).unwrap();
    if !setup.data.accept_licenses {
        out.push_str("Downloads are blocked until [data] accept_licenses = true (review the licenses above).\n");
    }
    out
}

/// What a fetch leaves: each extract by its path, with the identity the source gave it.
#[derive(Default)]
struct Fetched {
    report: String,
    paths: Vec<PathBuf>,
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
    for e in catalog.for_scope(&setup.scope)? {
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
        f.paths.push(path);
    }
    if dl.checks_for_updates() {
        mark_checked(setup)?;
    }
    writeln!(f.report, "downloads of the router: {downloaded} downloaded, {kept} up to date").unwrap();
    Ok(f)
}

/// The graph of the extracts, written to `path`; its nodes and edges.
fn build_graph(extracts: &[PathBuf], path: &Path) -> Result<(usize, usize)> {
    let mut builder = Builder::new(Car::from_road_classes(ROAD_CLASSES)?);
    for extract in extracts {
        osm::read_pbf(extract, |e| builder.first_pass(&e))?;
    }
    for extract in extracts {
        osm::read_pbf(extract, |e| builder.second_pass(&e))?;
    }
    let graph = builder.finish();
    graph_io::write(&graph, path)?;
    Ok((graph.nodes.len(), graph.edges.len()))
}

pub fn build(setup: &Setup, dl: &dyn Downloader, force: bool) -> Result<Built> {
    licenses_accepted(setup)?;
    if setup.scope.is_empty() {
        return Ok(Built::default());
    }
    let mut log = StageLog::new();
    let fetched = fetched(setup, dl)?;
    log.done("fetch", false);

    let dir = setup.graph_dir();
    let key = Key::new("graph").add("code", GRAPH_CODE).add("road-classes", ROAD_CLASSES).inputs(&fetched.paths, &fetched.identities)?.finish();
    let (nodes, edges, reused) = match incremental::up_to_date(&dir, &key).filter(|_| !force) {
        Some(stamp) => (stamp.info["nodes"].as_u64().unwrap_or(0) as usize, stamp.info["edges"].as_u64().unwrap_or(0) as usize, true),
        None => {
            incremental::invalidate(&dir)?;
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
            fs::create_dir_all(&dir)?;
            let (nodes, edges) = build_graph(&fetched.paths, &dir.join(GRAPH_FILE))?;
            incremental::seal(&dir, key, &[], serde_json::json!({ "nodes": nodes, "edges": edges }))?;
            (nodes, edges, false)
        }
    };
    log.done("graph", reused);

    let mut report = fetched.report;
    writeln!(report, "graph: {nodes} nodes, {edges} edges{}", if reused { " (up to date)" } else { "" }).unwrap();
    Ok(Built {
        report,
        parts: vec![BundlePart { from: dir, to: GRAPH_PART.into() }],
        meta: serde_json::json!({ "nodes": nodes, "edges": edges }),
        licenses: vec![osm_license()],
        stages: log.into_records(),
        report_extra: serde_json::Map::new(),
    })
}
