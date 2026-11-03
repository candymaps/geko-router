//! The traffic sensors of the city of Madrid (Ayuntamiento de Madrid, CC BY 4.0): about five thousand
//! measuring points of its roads, refreshed every five minutes in one XML file. Each says a level of
//! service from 0 (fluid) to 3 (congested) and in which direction it measures, in its description
//! ("… N-S (…)": from north to south). They say no speed, so the level is turned into one.
//!
//! A sensor is matched to the edge of its street that goes its way, once per graph, the first time
//! the feed has it (`Matcher`: a failing sensor is left out of it until it works); a sensor whose
//! direction cannot be read, or that is not near a street, is left out: slowing the wrong
//! carriageway is worse than not knowing.

use std::collections::{HashMap, HashSet};

use crate::graph::Graph;
use crate::snap::Snapper;
use crate::traffic::FREE_FLOW;
use crate::utm;

/// How far a sensor may be from the street it measures, in metres.
const MATCH_M: f64 = 40.0;
/// How far the direction of the street may be from that of the sensor, in degrees.
const MAX_TURN_DEG: f64 = 70.0;

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

/// The sensors of the feed that are working (`<error>N</error>`) and have a position and a level.
pub fn parse(xml: &str) -> Vec<Reading> {
    xml.split("<pm>")
        .skip(1)
        .filter_map(|block| {
            if tag(block, "error")? != "N" {
                return None;
            }
            let id = tag(block, "idelem")?.parse().ok()?;
            let level = tag(block, "nivelServicio")?.parse::<u8>().ok().filter(|l| *l <= 3)?;
            let (x, y) = (number(tag(block, "st_x")?)?, number(tag(block, "st_y")?)?);
            let (lat, lon) = utm::to_lat_lon(x, y);
            Some(Reading { id, description: tag(block, "descripcion").unwrap_or_default().to_owned(), lat, lon, level })
        })
        .collect()
}

/// The direction a sensor measures, in degrees clockwise from north, from the `A-B` of its description
/// (travelling from A to B, so towards B), or `None`.
pub fn heading(description: &str) -> Option<f64> {
    let towards = |name: &str| {
        Some(match name {
            "N" => 0.0,
            "NE" => 45.0,
            "E" => 90.0,
            "SE" => 135.0,
            "S" => 180.0,
            "SO" => 225.0,
            "O" => 270.0,
            "NO" => 315.0,
            _ => return None,
        })
    };
    description.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphabetic());
        let (from, to) = word.split_once('-')?;
        towards(from)?;
        towards(to)
    })
}

/// How fast the cars go at a level of service, in thousandths of free flow. The feed says no speed;
/// these are provisional, to be calibrated against measured times.
pub fn speed(level: u8) -> u16 {
    match level {
        0 => FREE_FLOW,
        1 => 650,
        2 => 400,
        _ => 200,
    }
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
pub fn match_sensors(graph: &Graph, snapper: &Snapper, readings: &[Reading]) -> (HashMap<u32, u32>, MatchStats) {
    let (mut matched, mut stats) = (HashMap::new(), MatchStats::default());
    for reading in readings {
        let Some(wanted) = heading(&reading.description) else {
            stats.no_direction += 1;
            continue;
        };
        let snaps = snapper.snap(graph, reading.lat, reading.lon, MATCH_M);
        if snaps.is_empty() {
            stats.no_street += 1;
            continue;
        }
        let best = snaps.iter().filter_map(|s| Some((angle_between(edge_heading(graph, s.edge)?, wanted), s.edge))).min_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((turn, edge)) if turn <= MAX_TURN_DEG => {
                matched.insert(reading.id, edge);
                stats.matched += 1;
            }
            _ => stats.wrong_direction += 1,
        }
    }
    (matched, stats)
}

/// The sensors matched so far. Those failing are not in a reading, so a sensor can first appear in
/// any of them: each is looked at the first time it is seen, and never again.
#[derive(Debug, Default)]
pub struct Matcher {
    matched: HashMap<u32, u32>,
    seen: HashSet<u32>,
}

impl Matcher {
    /// Matches the sensors of `readings` not seen before, and says how that went if there were any.
    pub fn add(&mut self, graph: &Graph, snapper: &Snapper, readings: &[Reading]) -> Option<MatchStats> {
        let new: Vec<Reading> = readings.iter().filter(|r| self.seen.insert(r.id)).cloned().collect();
        if new.is_empty() {
            return None;
        }
        let (matched, stats) = match_sensors(graph, snapper, &new);
        self.matched.extend(matched);
        Some(stats)
    }

    pub fn speeds(&self, readings: &[Reading]) -> Vec<(u32, u16)> {
        speeds(&self.matched, readings)
    }
}

/// The speed of each edge that a sensor measures, from the sensors in `matched` that `readings` has:
/// the slowest where two sensors share an edge.
pub fn speeds(matched: &HashMap<u32, u32>, readings: &[Reading]) -> Vec<(u32, u16)> {
    let mut by_edge: HashMap<u32, u16> = HashMap::new();
    for reading in readings {
        if let Some(edge) = matched.get(&reading.id) {
            let speed = speed(reading.level);
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
    use crate::profile::Car;

    const FEED: &str = "\u{feff}<pms>\n<fecha_hora>03/11/2026 17:55:02</fecha_hora>\n\
        <pm><idelem>3409</idelem><descripcion>SEPULVEDA N-S (CEBREROS - C. FCO.J.JIMENEZ)</descripcion><nivelServicio>2</nivelServicio><error>N</error><st_x>436004,103195086</st_x><st_y>4472588,47356732</st_y></pm>\n\
        <pm><idelem>1</idelem><descripcion>BROKEN E-O</descripcion><nivelServicio>3</nivelServicio><error>S</error><st_x>436004,1</st_x><st_y>4472588,4</st_y></pm>\n\
        <pm><idelem>2</idelem><descripcion>NO LEVEL</descripcion><nivelServicio>9</nivelServicio><error>N</error><st_x>436004,1</st_x><st_y>4472588,4</st_y></pm>\n\
        <pm><idelem>3</idelem><descripcion>NO POSITION</descripcion><nivelServicio>1</nivelServicio><error>N</error></pm>\n\
        </pms>";

    #[test]
    fn the_feed_is_read_with_its_decimal_commas_and_the_broken_sensors_left_out() {
        let readings = parse(FEED);
        assert_eq!(readings.len(), 1);
        let r = &readings[0];
        assert_eq!((r.id, r.level), (3409, 2));
        assert!((40.38..40.42).contains(&r.lat) && (-3.78..-3.72).contains(&r.lon), "{},{}", r.lat, r.lon);
        assert!(parse("").is_empty() && parse("<pms></pms>").is_empty());
    }

    #[test]
    fn the_direction_is_where_the_sensor_looks() {
        assert_eq!(heading("SEPULVEDA N-S (CEBREROS - C. FCO.J.JIMENEZ)"), Some(180.0));
        assert_eq!(heading("CJAL. FCO. J. JIMENEZ nº 126 E-O (BERLANAS-SEPULVEDA)"), Some(270.0));
        assert_eq!(heading("PASEO CASTELLANA S-N (A - B)"), Some(0.0));
        assert_eq!(heading("AVDA DE AMERICA (NE-SO)"), Some(225.0));
        assert_eq!(heading("CALLE SIN SENTIDO (DE UNA - A OTRA)"), None);
        assert_eq!(heading("X-Y"), None);
        assert_eq!(heading(""), None);
    }

    #[test]
    fn a_worse_level_is_slower() {
        let speeds: Vec<u16> = (0..=3).map(speed).collect();
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
        let (matched, stats) = match_sensors(&graph, &snapper, &readings);
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
        let mut matcher = Matcher::default();
        assert_eq!(matcher.add(&graph, &snapper, std::slice::from_ref(&working)).map(|s| s.matched), Some(1));
        let both = [working, failing_at_first];
        assert_eq!(matcher.speeds(&both).len(), 1, "not matched yet");
        assert_eq!(matcher.add(&graph, &snapper, &both).map(|s| s.matched), Some(1), "only the new one");
        assert_eq!(matcher.speeds(&both).len(), 2);
        assert_eq!(matcher.add(&graph, &snapper, &both), None, "nothing new");
    }

    #[test]
    fn the_slowest_sensor_of_an_edge_decides() {
        let matched: HashMap<u32, u32> = [(1, 7), (2, 7), (3, 8)].into();
        let readings = [reading(1, "", 0.0, 0.0, 1), reading(2, "", 0.0, 0.0, 3), reading(3, "", 0.0, 0.0, 0), reading(4, "", 0.0, 0.0, 3)];
        assert_eq!(speeds(&matched, &readings), vec![(7, speed(3)), (8, FREE_FLOW)]);
    }
}
