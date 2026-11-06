//! The provider of the sources of kind `datex2-situations`: incidents of a road authority in DATEX II.
//! Each record says what happens and where, a stretch between two points or a point, and its source
//! says what that does to a road: it closes it, or leaves a share of its speed.
//!
//! A stretch is the fastest way between its ends, at free flow, and only if that way is not much
//! longer than the line between them: a closure in the wrong place is worse than not knowing of it.
//! Each record is placed once, by its id and version.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::pack::Source;
use crate::traffic::{CLOSED, FREE_FLOW};

/// The kind of source this file reads.
pub const KIND: &str = "datex2-situations";

/// How much longer than the line between its ends a stretch may be, and by how much at least.
const MAX_DETOUR: f64 = 1.5;
const MIN_SLACK_M: f64 = 500.0;
const MAX_STRETCH_M: f64 = 100_000.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    validity_status: String,
    /// By the name of a type of record, the speed each of its values leaves.
    effects: BTreeMap<String, BTreeMap<String, u16>>,
    pub max_distance_m: f64,
}

#[derive(Deserialize)]
struct File {
    validity_status: String,
    effects: BTreeMap<String, BTreeMap<String, Effect>>,
    matching: Matching,
}

#[derive(Deserialize)]
struct Matching {
    max_distance_m: f64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Effect {
    /// `"closed"`.
    Word(String),
    Slower { speed_factor: f64 },
}

impl Spec {
    pub fn of(source: &Source) -> Result<Self, String> {
        let file: File = toml::Value::Table(source.table.clone()).try_into().map_err(|e: toml::de::Error| format!("source {}: {}", source.name, e.message()))?;
        let mut effects = BTreeMap::new();
        for (kind, values) in file.effects {
            let mut speeds = BTreeMap::new();
            for (value, effect) in values {
                let speed = match effect {
                    Effect::Word(word) if word == "closed" => CLOSED,
                    Effect::Slower { speed_factor } if (0.0..=1.0).contains(&speed_factor) => (speed_factor * FREE_FLOW as f64).round() as u16,
                    _ => return Err(format!("source {}: [effects.{kind}] {value}: \"closed\", or a speed_factor from 0 to 1", source.name)),
                };
                speeds.insert(value, speed);
            }
            effects.insert(kind, speeds);
        }
        Ok(Self { validity_status: file.validity_status, effects, max_distance_m: file.matching.max_distance_m })
    }
}

/// What a record does, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Situation {
    /// Its id and version: a new version may be somewhere else.
    pub id: String,
    pub speed: u16,
    pub from: [f64; 2],
    /// The other end; a point has none.
    pub to: Option<[f64; 2]>,
    /// Both ways of the road.
    pub both: bool,
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let start = block.find(&format!("<{name}>"))? + name.len() + 2;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(block[start..end].trim())
}

fn attribute<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let start = head.find(&format!("{name}=\""))? + name.len() + 2;
    head[start..].split('"').next()
}

/// The first coordinates of `block`.
fn point(block: &str) -> Option<[f64; 2]> {
    Some([tag(block, "loc:latitude")?.parse().ok()?, tag(block, "loc:longitude")?.parse().ok()?])
}

/// The part of `block` from `<name ` up to its end.
fn element<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let start = block.find(&format!("<{name} "))?;
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(&block[start..end])
}

fn time(block: &str, name: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(tag(block, name)?).ok().map(|t| t.with_timezone(&Utc))
}

/// When the source published the feed, as it writes it.
pub fn updated(xml: &str) -> Option<String> {
    tag(xml, "com:publicationTime").map(str::to_owned)
}

/// The records of the feed that are in force at `now` and that the source gives an effect to.
pub fn parse(spec: &Spec, xml: &str, now: DateTime<Utc>) -> Vec<Situation> {
    xml.split("<sit:situationRecord ")
        .skip(1)
        .filter_map(|record| {
            if tag(record, "com:validityStatus")? != spec.validity_status {
                return None;
            }
            if time(record, "com:overallStartTime").is_some_and(|t| t > now) || time(record, "com:overallEndTime").is_some_and(|t| t < now) {
                return None;
            }
            let speed = spec.effects.iter().filter_map(|(kind, values)| values.get(tag(record, &format!("sit:{kind}Type"))?).copied()).min()?;
            let head = record.split('>').next()?;
            let id = format!("{}.{}", attribute(head, "id")?, attribute(head, "version").unwrap_or("0"));
            let both = !matches!(tag(record, "lse:tpegDirectionRoad"), Some("positive" | "negative"));
            match (element(record, "loc:from"), element(record, "loc:to")) {
                (Some(from), Some(to)) => Some(Situation { id, speed, from: point(from)?, to: Some(point(to)?), both }),
                _ => Some(Situation { id, speed, from: point(record)?, to: None, both }),
            }
        })
        .collect()
}

/// Whether a way of `length_m` is the stretch between two points `line_m` apart.
fn plausible(length_m: f64, line_m: f64) -> bool {
    length_m <= MAX_STRETCH_M && length_m <= line_m * MAX_DETOUR + MIN_SLACK_M
}

fn metres(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dlat = (a[0] - b[0]) * 111_195.0;
    let dlon = (a[1] - b[1]) * 111_195.0 * a[0].to_radians().cos();
    (dlat * dlat + dlon * dlon).sqrt()
}

/// What the graph can say of a place, in the service.
pub trait Places {
    /// The edges of the fastest way from a point to another at free flow, and its length in metres.
    fn way(&self, from: [f64; 2], to: [f64; 2], max_m: f64) -> Option<(Vec<u32>, f64)>;
    /// The edges of the road nearest to a point, both ways.
    fn road(&self, at: [f64; 2], max_m: f64) -> Vec<u32>;
}

/// The edges of each record seen, which are not looked for again.
#[derive(Debug)]
pub struct Matcher {
    spec: Spec,
    placed: HashMap<String, Vec<u32>>,
}

impl Matcher {
    pub fn new(spec: Spec) -> Self {
        Self { spec, placed: HashMap::new() }
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    fn place(max_m: f64, situation: &Situation, places: &dyn Places) -> Vec<u32> {
        let Some(to) = situation.to else { return places.road(situation.from, max_m) };
        let line = metres(situation.from, to);
        let way = |a, b| places.way(a, b, max_m).filter(|(_, length)| plausible(*length, line)).map(|(edges, _)| edges).unwrap_or_default();
        let mut edges = way(situation.from, to);
        if situation.both {
            edges.extend(way(to, situation.from));
        }
        edges
    }

    /// The speed each edge has by `situations`, the slowest where two meet. Those no longer in the
    /// feed are forgotten.
    pub fn speeds(&mut self, situations: &[Situation], places: &dyn Places) -> Vec<(u32, u16)> {
        let max_m = self.spec.max_distance_m;
        let mut placed = HashMap::with_capacity(situations.len());
        let mut by_edge: HashMap<u32, u16> = HashMap::new();
        for situation in situations {
            let edges = self.placed.remove(&situation.id).unwrap_or_else(|| Self::place(max_m, situation, places));
            for edge in &edges {
                by_edge.entry(*edge).and_modify(|s| *s = (*s).min(situation.speed)).or_insert(situation.speed);
            }
            placed.insert(situation.id.clone(), edges);
        }
        self.placed = placed;
        let mut list: Vec<(u32, u16)> = by_edge.into_iter().collect();
        list.sort_unstable();
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::CountryPack;
    use chrono::TimeZone;
    use std::cell::Cell;

    fn spec() -> Spec {
        let pack = CountryPack::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/es")).unwrap();
        Spec::of(&pack.sources["dgt-incidents"]).unwrap()
    }

    fn record(id: u32, kind: &str, value: &str, direction: &str, extra: &str) -> String {
        format!(
            r#"<sit:situationRecord xsi:type="sit:X" id="{id}" version="2"><sit:validity><com:validityStatus>active</com:validityStatus><com:validityTimeSpecification><com:overallStartTime>2026-01-01T08:00:00.000+01:00</com:overallStartTime>{extra}</com:validityTimeSpecification></sit:validity>
            <sit:{kind}Type>{value}</sit:{kind}Type>
            <loc:tpegLinearLocation><loc:to xsi:type="loc:P"><loc:pointCoordinates><loc:latitude>40.0</loc:latitude><loc:longitude>-3.0</loc:longitude></loc:pointCoordinates></loc:to><loc:from xsi:type="loc:P"><loc:pointCoordinates><loc:latitude>40.1</loc:latitude><loc:longitude>-3.1</loc:longitude></loc:pointCoordinates></loc:from><lse:tpegDirectionRoad>{direction}</lse:tpegDirectionRoad></loc:tpegLinearLocation></sit:situationRecord>"#
        )
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 8, 0, 0).unwrap()
    }

    #[test]
    fn a_record_is_read_with_its_effect_and_its_ends() {
        let spec = spec();
        let feed = record(1, "roadOrCarriagewayOrLaneManagement", "roadClosed", "positive", "") + &record(2, "abnormalTraffic", "slowTraffic", "both", "");
        let found = parse(&spec, &feed, now());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], Situation { id: "1.2".into(), speed: CLOSED, from: [40.1, -3.1], to: Some([40.0, -3.0]), both: false });
        assert_eq!((found[1].speed, found[1].both), (500, true));
    }

    #[test]
    fn what_has_no_effect_or_is_not_in_force_is_left_out() {
        let spec = spec();
        let unknown = record(1, "roadOrCarriagewayOrLaneManagement", "somethingElse", "both", "");
        let over = record(2, "abnormalTraffic", "slowTraffic", "both", "<com:overallEndTime>2026-02-01T08:00:00.000+01:00</com:overallEndTime>");
        let later = record(3, "abnormalTraffic", "slowTraffic", "both", "").replace("2026-01-01", "2027-01-01");
        assert!(parse(&spec, &(unknown + &over + &later), now()).is_empty());
    }

    #[test]
    fn a_point_has_only_one_end() {
        let spec = spec();
        let feed = r#"<sit:situationRecord xsi:type="sit:X" id="9"><com:validityStatus>active</com:validityStatus><sit:roadOrCarriagewayOrLaneManagementType>roadClosed</sit:roadOrCarriagewayOrLaneManagementType><loc:point xsi:type="loc:P"><loc:latitude>41.5</loc:latitude><loc:longitude>-0.5</loc:longitude></loc:point></sit:situationRecord>"#;
        let found = parse(&spec, feed, now());
        assert_eq!((found[0].from, found[0].to, found[0].both), ([41.5, -0.5], None, true));
    }

    #[derive(Default)]
    struct Fake {
        length_m: f64,
        asked: Cell<u32>,
    }

    impl Places for Fake {
        fn way(&self, from: [f64; 2], _: [f64; 2], _: f64) -> Option<(Vec<u32>, f64)> {
            self.asked.set(self.asked.get() + 1);
            Some((vec![if from[0] > 40.05 { 1 } else { 2 }], self.length_m))
        }
        fn road(&self, _: [f64; 2], _: f64) -> Vec<u32> {
            vec![3]
        }
    }

    fn situation(id: &str, speed: u16, to: Option<[f64; 2]>, both: bool) -> Situation {
        Situation { id: id.into(), speed, from: [40.1, -3.1], to, both }
    }

    #[test]
    fn a_stretch_goes_its_way_and_the_other_only_if_it_says_so() {
        let places = Fake { length_m: 14_000.0, ..Default::default() };
        let mut matcher = Matcher::new(spec());
        let end = Some([40.0, -3.0]);
        assert_eq!(matcher.speeds(&[situation("a", CLOSED, end, false)], &places), [(1, CLOSED)]);
        assert_eq!(matcher.speeds(&[situation("b", CLOSED, end, true)], &places), [(1, CLOSED), (2, CLOSED)]);
        assert_eq!(matcher.speeds(&[situation("c", 500, None, true)], &places), [(3, 500)]);
    }

    #[test]
    fn a_way_much_longer_than_the_stretch_is_not_it() {
        let places = Fake { length_m: 60_000.0, ..Default::default() };
        assert!(Matcher::new(spec()).speeds(&[situation("a", CLOSED, Some([40.0, -3.0]), false)], &places).is_empty());
    }

    #[test]
    fn a_record_is_placed_once_and_the_slowest_decides() {
        let places = Fake { length_m: 14_000.0, ..Default::default() };
        let mut matcher = Matcher::new(spec());
        let list = [situation("a", 800, Some([40.0, -3.0]), false), situation("b", 400, Some([40.0, -3.0]), false)];
        assert_eq!(matcher.speeds(&list, &places), [(1, 400)]);
        assert_eq!(places.asked.get(), 2);
        matcher.speeds(&list, &places);
        assert_eq!(places.asked.get(), 2, "not again");
    }
}
