//! The provider of the sources of kind `sensors-xml`: measuring points in one XML file, each with a
//! level of service and the direction it measures, in its description ("… N-S (…)": from north to
//! south), as the sensors of Madrid publish them. They say no speed, so the level is turned into one,
//! by the factors of the source. Everything that depends on the source is in its file (`Spec`).
//!
//! A sensor is matched to the edge of its street that goes its way, once per graph, the first time
//! the feed has it (`Matcher`: a failing sensor is left out of it until it works); a sensor whose
//! direction cannot be read, or that is not near a street, is left out: slowing the wrong
//! carriageway is worse than not knowing.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Deserialize;

use crate::graph::Graph;
use crate::pack::Source;
use crate::snap::Snapper;
use crate::traffic::FREE_FLOW;
use crate::utm;

/// The kind of source this file reads.
pub const KIND: &str = "sensors-xml";

/// The only projected system there is a conversion for (`utm`).
const CRS: &str = "EPSG:25830";

/// What a source of this kind says of its feed, in its file.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    element: String,
    id: String,
    description: String,
    x: String,
    y: String,
    timestamp: String,
    error: String,
    error_ok: String,
    service_level: String,
    /// In degrees clockwise from north, by the name the descriptions give each direction.
    directions: BTreeMap<String, f64>,
    /// The thousandths of free flow of each level of service, from 0.
    factors: Vec<u16>,
    /// How far a sensor may be from the street it measures, in metres.
    max_distance_m: f64,
    /// How far the direction of the street may be from that of the sensor, in degrees.
    max_turn_deg: f64,
}

#[derive(Deserialize)]
struct File {
    record: Record,
    measures: Measures,
    matching: Matching,
    service_level_factor: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
struct Record {
    element: String,
    id: String,
    description: String,
    x: String,
    y: String,
    crs: String,
    timestamp: String,
    error: String,
    error_ok: String,
    directions: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
struct Measures {
    service_level: String,
}

#[derive(Deserialize)]
struct Matching {
    max_distance_m: f64,
    max_turn_deg: f64,
}

impl Spec {
    pub fn of(source: &Source) -> Result<Self, String> {
        let file: File = toml::Value::Table(source.table.clone()).try_into().map_err(|e: toml::de::Error| format!("source {}: {}", source.name, e.message()))?;
        if file.record.crs != CRS {
            return Err(format!("source {}: no conversion from {}, only from {CRS}", source.name, file.record.crs));
        }
        let mut factors = Vec::new();
        for level in 0..file.service_level_factor.len() {
            let factor = file.service_level_factor.get(&level.to_string()).filter(|f| (0.0..=1.0).contains(*f));
            let factor = factor.ok_or_else(|| format!("source {}: [service_level_factor] goes from 0 up, each a share of free flow", source.name))?;
            factors.push((factor * FREE_FLOW as f64).round() as u16);
        }
        if factors.is_empty() {
            return Err(format!("source {}: [service_level_factor] is empty", source.name));
        }
        let r = file.record;
        Ok(Self { element: r.element, id: r.id, description: r.description, x: r.x, y: r.y, timestamp: r.timestamp, error: r.error, error_ok: r.error_ok, service_level: file.measures.service_level, directions: r.directions, factors, max_distance_m: file.matching.max_distance_m, max_turn_deg: file.matching.max_turn_deg })
    }

    /// How fast the cars go at a level of service, in thousandths of free flow.
    pub fn speed(&self, level: u8) -> u16 {
        self.factors[(level as usize).min(self.factors.len() - 1)]
    }

    pub fn levels(&self) -> usize {
        self.factors.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub id: u32,
    pub description: String,
    pub lat: f64,
    pub lon: f64,
    /// 0 fluid, 1 slow, 2 holdups, 3 congested.
    pub level: u8,
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let start = block.find(&format!("<{name}>"))? + name.len() + 2;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

/// A number as the feed writes it, with a decimal comma.
fn number(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse().ok()
}

/// The sensors of the feed that are working and have a position and a level.
pub fn parse(spec: &Spec, xml: &str) -> Vec<Reading> {
    xml.split(&format!("<{}>", spec.element))
        .skip(1)
        .filter_map(|block| {
            if tag(block, &spec.error)? != spec.error_ok {
                return None;
            }
            let id = tag(block, &spec.id)?.parse().ok()?;
            let level = tag(block, &spec.service_level)?.parse::<u8>().ok().filter(|l| (*l as usize) < spec.levels())?;
            let (x, y) = (number(tag(block, &spec.x)?)?, number(tag(block, &spec.y)?)?);
            let (lat, lon) = utm::to_lat_lon(x, y);
            Some(Reading { id, description: tag(block, &spec.description).unwrap_or_default().to_owned(), lat, lon, level })
        })
        .collect()
}

/// When the source last updated the feed, as it writes it (`03/11/2026 17:55:02`, local time): the
/// terms of Madrid ask to say it and not to alter it.
pub fn updated(spec: &Spec, xml: &str) -> Option<String> {
    tag(xml, &spec.timestamp).filter(|date| !date.is_empty()).map(str::to_owned)
}

/// The direction a sensor measures, in degrees clockwise from north, from the `A-B` of its description
/// (travelling from A to B, so towards B), or `None`.
pub fn heading(spec: &Spec, description: &str) -> Option<f64> {
    description.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphabetic());
        let (from, to) = word.split_once('-')?;
        spec.directions.get(from)?;
        spec.directions.get(to).copied()
    })
}

/// The direction an edge goes, in degrees clockwise from north: from its first point to its last.
fn edge_heading(graph: &Graph, edge: u32) -> Option<f64> {
    let mut points = graph.shape(&graph.edges[edge as usize]);
    let first = points.next()?;
    let last = points.last()?;
    let dlat = last[0] - first[0];
    let dlon = (last[1] - first[1]) * first[0].to_radians().cos();
    (dlat != 0.0 || dlon != 0.0).then(|| dlon.atan2(dlat).to_degrees().rem_euclid(360.0))
}

fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MatchStats {
    pub matched: usize,
    pub no_direction: usize,
    pub no_street: usize,
    pub wrong_direction: usize,
}

/// The edge each sensor measures, by the id of the sensor.
pub fn match_sensors(spec: &Spec, graph: &Graph, snapper: &Snapper, readings: &[Reading]) -> (HashMap<u32, u32>, MatchStats) {
    let (mut matched, mut stats) = (HashMap::new(), MatchStats::default());
    for reading in readings {
        let Some(wanted) = heading(spec, &reading.description) else {
            stats.no_direction += 1;
            continue;
        };
        let snaps = snapper.snap(graph, reading.lat, reading.lon, spec.max_distance_m);
        if snaps.is_empty() {
            stats.no_street += 1;
            continue;
        }
        let best = snaps.iter().filter_map(|s| Some((angle_between(edge_heading(graph, s.edge)?, wanted), s.edge))).min_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((turn, edge)) if turn <= spec.max_turn_deg => {
                matched.insert(reading.id, edge);
                stats.matched += 1;
            }
            _ => stats.wrong_direction += 1,
        }
    }
    (matched, stats)
}

/// The sensors matched so far: those of the bundle, matched when it was built, and those that came
/// later. Those failing are not in a reading, so a sensor can first appear in any of them: each is
/// looked at the first time it is seen, and never again.
#[derive(Debug)]
pub struct Matcher {
    spec: Spec,
    matched: HashMap<u32, u32>,
    seen: HashSet<u32>,
}

impl Matcher {
    pub fn new(spec: Spec) -> Self {
        Self::seeded(spec, HashMap::new())
    }

    /// With the sensors that the bundle says are matched.
    pub fn seeded(spec: Spec, matched: HashMap<u32, u32>) -> Self {
        let seen = matched.keys().copied().collect();
        Self { spec, matched, seen }
    }

    /// Matches the sensors of `readings` not seen before, and says how that went if there were any.
    pub fn add(&mut self, graph: &Graph, snapper: &Snapper, readings: &[Reading]) -> Option<MatchStats> {
        let new: Vec<Reading> = readings.iter().filter(|r| self.seen.insert(r.id)).cloned().collect();
        if new.is_empty() {
            return None;
        }
        let (matched, stats) = match_sensors(&self.spec, graph, snapper, &new);
        self.matched.extend(matched);
        Some(stats)
    }

    pub fn speeds(&self, readings: &[Reading]) -> Vec<(u32, u16)> {
        speeds(&self.spec, &self.matched, readings)
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }
}

/// The speed of each edge that a sensor measures, from the sensors in `matched` that `readings` has:
/// the slowest where two sensors share an edge.
pub fn speeds(spec: &Spec, matched: &HashMap<u32, u32>, readings: &[Reading]) -> Vec<(u32, u16)> {
    let mut by_edge: HashMap<u32, u16> = HashMap::new();
    for reading in readings {
        if let Some(edge) = matched.get(&reading.id) {
            let speed = spec.speed(reading.level);
            by_edge.entry(*edge).and_modify(|s| *s = (*s).min(speed)).or_insert(speed);
        }
    }
    let mut list: Vec<(u32, u16)> = by_edge.into_iter().collect();
    list.sort_unstable();
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Builder;
    use crate::osm::Element;
    use crate::pack::CountryPack;
    use crate::profile::Car;

    fn spec() -> Spec {
        let pack = CountryPack::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/es")).unwrap();
        Spec::of(&pack.sources["madrid-sensors"]).unwrap()
    }

    const FEED: &str = "\u{feff}<pms>\n<fecha_hora>03/11/2026 17:55:02</fecha_hora>\n\
        <pm><idelem>3409</idelem><descripcion>SEPULVEDA N-S (CEBREROS - C. FCO.J.JIMENEZ)</descripcion><nivelServicio>2</nivelServicio><error>N</error><st_x>436004,103195086</st_x><st_y>4472588,47356732</st_y></pm>\n\
        <pm><idelem>1</idelem><descripcion>BROKEN E-O</descripcion><nivelServicio>3</nivelServicio><error>S</error><st_x>436004,1</st_x><st_y>4472588,4</st_y></pm>\n\
        <pm><idelem>2</idelem><descripcion>NO LEVEL</descripcion><nivelServicio>9</nivelServicio><error>N</error><st_x>436004,1</st_x><st_y>4472588,4</st_y></pm>\n\
        <pm><idelem>3</idelem><descripcion>NO POSITION</descripcion><nivelServicio>1</nivelServicio><error>N</error></pm>\n\
        </pms>";

    #[test]
    fn the_date_of_the_feed_is_kept_as_it_comes() {
        let spec = spec();
        assert_eq!(updated(&spec, FEED).as_deref(), Some("03/11/2026 17:55:02"));
        assert_eq!(updated(&spec, "<pms></pms>"), None);
        assert_eq!(updated(&spec, "<pms><fecha_hora> </fecha_hora></pms>"), None);
    }

    #[test]
    fn the_feed_is_read_with_its_decimal_commas_and_the_broken_sensors_left_out() {
        let spec = spec();
        let readings = parse(&spec, FEED);
        assert_eq!(readings.len(), 1);
        let r = &readings[0];
        assert_eq!((r.id, r.level), (3409, 2));
        assert!((40.38..40.42).contains(&r.lat) && (-3.78..-3.72).contains(&r.lon), "{},{}", r.lat, r.lon);
        assert!(parse(&spec, "").is_empty() && parse(&spec, "<pms></pms>").is_empty());
    }

    #[test]
    fn the_direction_is_where_the_sensor_looks() {
        let spec = spec();
        assert_eq!(heading(&spec, "SEPULVEDA N-S (CEBREROS - C. FCO.J.JIMENEZ)"), Some(180.0));
        assert_eq!(heading(&spec, "CJAL. FCO. J. JIMENEZ nº 126 E-O (BERLANAS-SEPULVEDA)"), Some(270.0));
        assert_eq!(heading(&spec, "PASEO CASTELLANA S-N (A - B)"), Some(0.0));
        assert_eq!(heading(&spec, "AVDA DE AMERICA (NE-SO)"), Some(225.0));
        assert_eq!(heading(&spec, "CALLE SIN SENTIDO (DE UNA - A OTRA)"), None);
        assert_eq!(heading(&spec, "X-Y"), None);
        assert_eq!(heading(&spec, ""), None);
    }

    #[test]
    fn a_worse_level_is_slower() {
        let spec = spec();
        let speeds: Vec<u16> = (0..=3).map(|l| spec.speed(l)).collect();
        assert_eq!(speeds[0], FREE_FLOW);
        assert!(speeds.windows(2).all(|w| w[0] > w[1]));
    }

    /// A street going east (way 10, nodes 1 -> 2) and one going north (way 11, 3 -> 4), one-way.
    fn graph() -> Graph {
        let node = |id, lat, lon| Element::Node { id, lat, lon, tags: Vec::new() };
        let way = |id, refs: &[i64]| Element::Way { id, refs: refs.to_vec(), tags: vec![("highway".into(), "residential".into()), ("oneway".into(), "yes".into())] };
        let elements = [node(1, 40.4, -3.7), node(2, 40.4, -3.698), node(3, 40.41, -3.7), node(4, 40.412, -3.7), way(10, &[1, 2]), way(11, &[3, 4])];
        let mut builder = Builder::new(Car::from_road_classes("[free_flow_kmh]\nresidential = 36\n").unwrap());
        elements.iter().for_each(|e| builder.first_pass(e));
        elements.iter().for_each(|e| builder.second_pass(e));
        builder.finish()
    }

    fn reading(id: u32, description: &str, lat: f64, lon: f64, level: u8) -> Reading {
        Reading { id, description: description.into(), lat, lon, level }
    }

    #[test]
    fn a_sensor_is_matched_to_the_street_that_goes_its_way_or_to_none() {
        let graph = graph();
        let snapper = Snapper::new(&graph, vec![true; graph.edges.len()]);
        let readings = [
            reading(1, "CALLE ESTE E-O", 40.4, -3.699, 2),
            reading(2, "CALLE ESTE O-E", 40.4, -3.699, 2),
            reading(3, "CALLE NORTE S-N", 40.411, -3.7, 3),
            reading(4, "CALLE NORTE N-S", 40.411, -3.7, 3),
            reading(5, "CALLE SIN FLECHA", 40.4, -3.699, 1),
            reading(6, "CALLE LEJOS O-E", 40.5, -3.5, 1),
        ];
        let (matched, stats) = match_sensors(&spec(), &graph, &snapper, &readings);
        // Sensor 2 (towards the east) is on way 10, sensor 3 (towards the north) on way 11. The streets
        // are one-way, so those that look the other way find no edge going theirs.
        let way_of = |id: u32| graph.edges[matched[&id] as usize].way;
        assert_eq!((way_of(2), way_of(3)), (10, 11));
        assert!(!matched.contains_key(&1) && !matched.contains_key(&4));
        assert_eq!(stats, MatchStats { matched: 2, no_direction: 1, no_street: 1, wrong_direction: 2 });
    }

    #[test]
    fn a_sensor_that_comes_back_is_matched_then_and_only_once() {
        let graph = graph();
        let snapper = Snapper::new(&graph, vec![true; graph.edges.len()]);
        let working = reading(1, "CALLE ESTE O-E", 40.4, -3.699, 2);
        let failing_at_first = reading(2, "CALLE NORTE S-N", 40.411, -3.7, 3);
        let mut matcher = Matcher::new(spec());
        assert_eq!(matcher.add(&graph, &snapper, std::slice::from_ref(&working)).map(|s| s.matched), Some(1));
        let both = [working, failing_at_first];
        assert_eq!(matcher.speeds(&both).len(), 1, "not matched yet");
        assert_eq!(matcher.add(&graph, &snapper, &both).map(|s| s.matched), Some(1), "only the new one");
        assert_eq!(matcher.speeds(&both).len(), 2);
        assert_eq!(matcher.add(&graph, &snapper, &both), None, "nothing new");
    }

    #[test]
    fn a_sensor_of_the_bundle_is_not_matched_again() {
        let graph = graph();
        let snapper = Snapper::new(&graph, vec![true; graph.edges.len()]);
        let known = reading(1, "CALLE ESTE O-E", 40.4, -3.699, 2);
        let mut matcher = Matcher::seeded(spec(), [(1, 0)].into());
        assert_eq!(matcher.add(&graph, &snapper, std::slice::from_ref(&known)), None);
        assert_eq!(matcher.speeds(std::slice::from_ref(&known)).len(), 1);
    }

    #[test]
    fn a_source_with_a_system_of_coordinates_there_is_no_conversion_from_is_refused() {
        let pack = CountryPack::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/es")).unwrap();
        let changes: toml::Table = toml::from_str("[record]\ncrs = \"EPSG:4326\"").unwrap();
        let error = Spec::of(&pack.sources["madrid-sensors"].with(&changes).unwrap()).unwrap_err();
        assert!(error.contains("EPSG:4326"), "{error}");
    }

    #[test]
    fn the_slowest_sensor_of_an_edge_decides() {
        let matched: HashMap<u32, u32> = [(1, 7), (2, 7), (3, 8)].into();
        let readings = [reading(1, "", 0.0, 0.0, 1), reading(2, "", 0.0, 0.0, 3), reading(3, "", 0.0, 0.0, 0), reading(4, "", 0.0, 0.0, 3)];
        let spec = spec();
        assert_eq!(speeds(&spec, &matched, &readings), vec![(7, spec.speed(3)), (8, FREE_FLOW)]);
    }
}
