//! The profiles: which ways a car, a truck, a bicycle or someone on foot can use, in which direction
//! and how fast. The speeds by road class are data of the country pack (`data/road-classes.toml`, and
//! `data/road-classes-<profile>.toml` for the others), so a profile is the same code for every
//! country. A ferry that takes them is a way too, at the time it says it lasts.

use std::collections::HashMap;

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

use crate::osm::{Tags, tag};

/// How fast a ferry goes when it does not say how long it lasts: what OSRM's `car.lua` takes, so that
/// the two can be compared. A ferry goes faster than this, but a crossing is rarely the best choice
/// and a route that takes one with no timetable should not look short.
const FERRY_KMH: f32 = 5.0;

/// Who goes along the roads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Car,
    Truck,
    Bike,
    Foot,
}

impl Mode {
    pub const NAMES: [&str; 4] = ["car", "truck", "bike", "foot"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "car" => Some(Self::Car),
            "truck" => Some(Self::Truck),
            "bike" => Some(Self::Bike),
            "foot" => Some(Self::Foot),
            _ => None,
        }
    }

    /// Whether the traffic of the sources says anything of them: they measure motor traffic, and a
    /// jam does not stop a bicycle.
    pub fn has_traffic(self) -> bool {
        matches!(self, Self::Car | Self::Truck)
    }

    fn access_keys(self) -> &'static [&'static str] {
        match self {
            Self::Car => &["motor_vehicle", "motorcar", "vehicle", "access"],
            Self::Truck => &["hgv", "goods", "motor_vehicle", "vehicle", "access"],
            Self::Bike => &["bicycle", "vehicle", "access"],
            Self::Foot => &["foot", "access"],
        }
    }

    /// Whether the speed limits of a road say anything of how fast they go.
    fn follows_maxspeed(self) -> bool {
        self.has_traffic()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Both,
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Way {
    pub speed_kmh: f32,
    pub direction: Direction,
    pub ferry: bool,
    /// How long all of it lasts, when it says so: then the speed does not count.
    pub duration_s: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct RoadClasses {
    free_flow_kmh: HashMap<String, f32>,
    /// What an explicit `maxspeed` is multiplied by: nobody drives at the limit all the way.
    #[serde(default = "no_reduction")]
    maxspeed_factor: f32,
    /// The fastest it goes, whatever the road allows.
    #[serde(default)]
    max_kmh: Option<f32>,
    /// What a truck measures: a way that limits less than this is not for it.
    #[serde(default)]
    height_m: Option<f32>,
    #[serde(default)]
    weight_t: Option<f32>,
}

fn no_reduction() -> f32 {
    1.0
}

#[derive(Debug, Clone)]
pub struct Vehicle {
    mode: Mode,
    free_flow_kmh: HashMap<String, f32>,
    maxspeed_factor: f32,
    max_kmh: f32,
    height_m: Option<f32>,
    weight_t: Option<f32>,
}

impl Vehicle {
    /// A car, from the contents of `data/road-classes.toml` of a pack.
    pub fn from_road_classes(toml: &str) -> Result<Self> {
        Self::of(Mode::Car, toml)
    }

    /// From the road classes of a pack for `mode`: the `highway` values it can use, and their speeds.
    pub fn of(mode: Mode, toml: &str) -> Result<Self> {
        let classes: RoadClasses = toml::from_str(toml).map_err(|e| GekoError::Data(format!("road-classes.toml: {e}")))?;
        if let Some((class, speed)) = classes.free_flow_kmh.iter().find(|(_, s)| !(s.is_finite() && **s > 0.0)) {
            return Err(GekoError::Data(format!("road-classes.toml: the speed of `{class}` is {speed}, not a positive number")));
        }
        if !(classes.maxspeed_factor.is_finite() && classes.maxspeed_factor > 0.0 && classes.maxspeed_factor <= 1.0) {
            return Err(GekoError::Data(format!("road-classes.toml: maxspeed_factor is {}, not a number above 0 and up to 1", classes.maxspeed_factor)));
        }
        Ok(Self { mode, free_flow_kmh: classes.free_flow_kmh, maxspeed_factor: classes.maxspeed_factor, max_kmh: classes.max_kmh.unwrap_or(f32::MAX), height_m: classes.height_m, weight_t: classes.weight_t })
    }

    /// Whether a way lets it through, by its access tags and, for a truck, its limits.
    fn allowed(&self, tags: &Tags) -> bool {
        let limit = |key: &str| tag(tags, key).and_then(metric);
        allowed(self.mode, tags) && !self.height_m.is_some_and(|m| limit("maxheight").is_some_and(|l| l < m)) && !self.weight_t.is_some_and(|t| limit("maxweight").is_some_and(|l| l < t))
    }

    /// `None` when it cannot use the way.
    pub fn way(&self, tags: &Tags) -> Option<Way> {
        if tag(tags, "route") == Some("ferry") {
            return self.allowed(tags).then(|| Way { speed_kmh: FERRY_KMH, direction: direction(self.mode, tags, ""), ferry: true, duration_s: tag(tags, "duration").and_then(duration_s) });
        }
        let highway = tag(tags, "highway")?;
        let class_speed = *self.free_flow_kmh.get(highway)?;
        if (tag(tags, "area") == Some("yes") && self.mode != Mode::Foot) || !self.allowed(tags) {
            return None;
        }
        let limit = if self.mode == Mode::Truck { tag(tags, "maxspeed:hgv").or(tag(tags, "maxspeed")) } else { tag(tags, "maxspeed") };
        let limit = limit.filter(|_| self.mode.follows_maxspeed()).and_then(maxspeed_kmh);
        let speed_kmh = limit.map_or(class_speed, |limit| limit * self.maxspeed_factor).min(self.max_kmh);
        Some(Way { speed_kmh, direction: direction(self.mode, tags, highway), ferry: false, duration_s: None })
    }

    /// Whether the turn restrictions of OSM bind it: they are for traffic, not for people on foot.
    pub fn obeys_restrictions(&self) -> bool {
        self.mode != Mode::Foot
    }
}

/// The number at the start of a `maxheight` or a `maxweight`, in metres or tonnes; `None` in any other
/// unit, which then limits nothing rather than something wrong.
fn metric(value: &str) -> Option<f32> {
    let value = value.trim();
    let digits = value.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(value.len());
    matches!(value[digits..].trim(), "" | "m" | "t").then(|| value[..digits].parse().ok()).flatten()
}

/// The most specific access tag of the mode decides, up to `access`.
fn allowed(mode: Mode, tags: &Tags) -> bool {
    let value = mode.access_keys().iter().find_map(|key| tag(tags, key));
    !matches!(value, Some("no" | "private" | "agricultural" | "forestry" | "emergency" | "use_sidepath"))
}

fn direction(mode: Mode, tags: &Tags, highway: &str) -> Direction {
    match mode {
        Mode::Foot if tag(tags, "oneway:foot") != Some("yes") => return Direction::Both,
        Mode::Bike if tag(tags, "oneway:bicycle") == Some("no") || tag(tags, "cycleway").is_some_and(|c| c.starts_with("opposite")) => return Direction::Both,
        _ => {}
    }
    match tag(tags, "oneway") {
        Some("yes" | "true" | "1") => Direction::Forward,
        Some("-1" | "reverse") => Direction::Backward,
        Some("no" | "false" | "0") => Direction::Both,
        _ if matches!(tag(tags, "junction"), Some("roundabout" | "circular")) => Direction::Forward,
        _ if matches!(highway, "motorway" | "motorway_link") => Direction::Forward,
        _ => Direction::Both,
    }
}

/// The `duration` of a ferry, in seconds: `90` (minutes), `1:30`, `1:30:00` or `PT1H30M`.
pub fn duration_s(value: &str) -> Option<f32> {
    let value = value.trim();
    let seconds = if let Some(iso) = value.strip_prefix("PT") {
        let (mut total, mut number) = (0.0_f32, String::new());
        for c in iso.chars() {
            if c.is_ascii_digit() || c == '.' {
                number.push(c);
                continue;
            }
            let unit = match c {
                'H' => 3600.0,
                'M' => 60.0,
                'S' => 1.0,
                _ => return None,
            };
            total += number.parse::<f32>().ok()? * unit;
            number.clear();
        }
        if !number.is_empty() {
            return None;
        }
        total
    } else {
        let parts: Vec<f32> = value.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        match parts[..] {
            [minutes] => minutes * 60.0,
            [hours, minutes] => hours * 3600.0 + minutes * 60.0,
            [hours, minutes, seconds] => hours * 3600.0 + minutes * 60.0 + seconds,
            _ => return None,
        }
    };
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// `50`, `50 km/h` or `30 mph`; anything else (`none`, `signals`, `ES:urban`) is not a number and
/// the class speed applies.
pub fn maxspeed_kmh(value: &str) -> Option<f32> {
    let value = value.trim();
    let (number, factor) = match value.strip_suffix("mph") {
        Some(rest) => (rest, 1.609_344),
        None => (value.strip_suffix("km/h").unwrap_or(value), 1.0),
    };
    let speed = number.trim().parse::<f32>().ok()? * factor;
    (speed.is_finite() && speed > 0.0).then_some(speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSES: &str = "[free_flow_kmh]\nmotorway = 120\nprimary = 90\nresidential = 30\n";

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn car() -> Vehicle {
        Vehicle::from_road_classes(CLASSES).unwrap()
    }

    #[test]
    fn duration_forms() {
        for (value, seconds) in [("90", 5400.0), ("1:30", 5400.0), (" 01:30:30 ", 5430.0), ("PT1H30M", 5400.0), ("PT45M", 2700.0), ("PT30S", 30.0)] {
            assert_eq!(duration_s(value), Some(seconds), "{value:?}");
        }
        for bad in ["", "soon", "0", "0:00", "1:2:3:4", "PT", "PT5", "PT5X", "-5"] {
            assert_eq!(duration_s(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_ferry_is_a_way_unless_cars_may_not_board() {
        let way = car().way(&tags(&[("route", "ferry"), ("duration", "2:00")])).unwrap();
        assert!(way.ferry);
        assert_eq!((way.duration_s, way.direction), (Some(7200.0), Direction::Both));
        let no_time = car().way(&tags(&[("route", "ferry"), ("oneway", "yes")])).unwrap();
        assert_eq!((no_time.duration_s, no_time.speed_kmh, no_time.direction), (None, FERRY_KMH, Direction::Forward));
        assert!(car().way(&tags(&[("route", "ferry"), ("motor_vehicle", "no")])).is_none());
        assert!(!car().way(&tags(&[("highway", "primary")])).unwrap().ferry);
    }

    #[test]
    fn maxspeed_forms() {
        assert_eq!(maxspeed_kmh("50"), Some(50.0));
        assert_eq!(maxspeed_kmh(" 50 km/h "), Some(50.0));
        assert!((maxspeed_kmh("30 mph").unwrap() - 48.28).abs() < 0.01);
        for bad in ["none", "signals", "ES:urban", "", "0", "-5"] {
            assert_eq!(maxspeed_kmh(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn class_speed_and_explicit_maxspeed() {
        assert_eq!(car().way(&tags(&[("highway", "primary")])).unwrap().speed_kmh, 90.0);
        assert_eq!(car().way(&tags(&[("highway", "primary"), ("maxspeed", "70")])).unwrap().speed_kmh, 70.0);
        assert_eq!(car().way(&tags(&[("highway", "primary"), ("maxspeed", "none")])).unwrap().speed_kmh, 90.0);
    }

    #[test]
    fn maxspeed_is_reduced_by_the_factor_of_the_pack() {
        let car = Vehicle::from_road_classes("maxspeed_factor = 0.8\n[free_flow_kmh]\nprimary = 65\n").unwrap();
        let speed = |extra: &[(&str, &str)]| {
            let mut all = vec![("highway", "primary")];
            all.extend_from_slice(extra);
            car.way(&tags(&all)).unwrap().speed_kmh
        };
        assert_eq!(speed(&[]), 65.0);
        assert!((speed(&[("maxspeed", "100")]) - 80.0).abs() < 1e-4);
        assert_eq!(speed(&[("maxspeed", "none")]), 65.0);
        for bad in ["0", "1.5", "-1"] {
            assert!(Vehicle::from_road_classes(&format!("maxspeed_factor = {bad}\n[free_flow_kmh]\nprimary = 65\n")).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_listed_classes_and_open_access() {
        assert!(car().way(&tags(&[("highway", "footway")])).is_none());
        assert!(car().way(&tags(&[("railway", "rail")])).is_none());
        assert!(car().way(&tags(&[("highway", "residential"), ("access", "private")])).is_none());
        assert!(car().way(&tags(&[("highway", "residential"), ("area", "yes")])).is_none());
        // The most specific tag wins: a private road open to motor vehicles.
        assert!(car().way(&tags(&[("highway", "residential"), ("access", "private"), ("motor_vehicle", "yes")])).is_some());
        assert!(car().way(&tags(&[("highway", "residential"), ("access", "yes"), ("motor_vehicle", "no")])).is_none());
        assert!(car().way(&tags(&[("highway", "residential"), ("access", "destination")])).is_some());
    }

    #[test]
    fn directions() {
        let direction = |extra: &[(&str, &str)], highway: &str| {
            let mut all = vec![("highway", highway)];
            all.extend_from_slice(extra);
            car().way(&tags(&all)).unwrap().direction
        };
        assert_eq!(direction(&[], "residential"), Direction::Both);
        assert_eq!(direction(&[("oneway", "yes")], "residential"), Direction::Forward);
        assert_eq!(direction(&[("oneway", "-1")], "residential"), Direction::Backward);
        assert_eq!(direction(&[("junction", "roundabout")], "residential"), Direction::Forward);
        assert_eq!(direction(&[], "motorway"), Direction::Forward);
        assert_eq!(direction(&[("oneway", "no")], "motorway"), Direction::Both);
    }

    fn of(mode: Mode, classes: &str) -> Vehicle {
        Vehicle::of(mode, classes).unwrap()
    }

    #[test]
    fn each_mode_reads_its_own_access() {
        let classes = "[free_flow_kmh]\nresidential = 10\nfootway = 5\n";
        let no_cars = tags(&[("highway", "residential"), ("motor_vehicle", "no")]);
        assert!(of(Mode::Car, classes).way(&no_cars).is_none());
        assert!(of(Mode::Bike, classes).way(&no_cars).is_some());
        assert!(of(Mode::Foot, classes).way(&no_cars).is_some());
        let no_feet = tags(&[("highway", "residential"), ("foot", "no")]);
        assert!(of(Mode::Foot, classes).way(&no_feet).is_none());
        assert!(of(Mode::Car, classes).way(&no_feet).is_some());
        let no_trucks = tags(&[("highway", "residential"), ("hgv", "no")]);
        assert!(of(Mode::Truck, classes).way(&no_trucks).is_none());
        assert!(of(Mode::Car, classes).way(&no_trucks).is_some());
    }

    #[test]
    fn one_way_binds_less_those_on_foot_and_on_a_bicycle() {
        let classes = "[free_flow_kmh]\nresidential = 10\n";
        let oneway = tags(&[("highway", "residential"), ("oneway", "yes")]);
        assert_eq!(of(Mode::Foot, classes).way(&oneway).unwrap().direction, Direction::Both);
        assert_eq!(of(Mode::Bike, classes).way(&oneway).unwrap().direction, Direction::Forward);
        let contraflow = tags(&[("highway", "residential"), ("oneway", "yes"), ("oneway:bicycle", "no")]);
        assert_eq!(of(Mode::Bike, classes).way(&contraflow).unwrap().direction, Direction::Both);
        assert_eq!(of(Mode::Car, classes).way(&contraflow).unwrap().direction, Direction::Forward);
    }

    #[test]
    fn a_truck_keeps_to_its_limits_and_a_bicycle_ignores_maxspeed() {
        let truck = of(Mode::Truck, "max_kmh = 80\nheight_m = 4\nweight_t = 18\n[free_flow_kmh]\nmotorway = 80\n");
        let fast = tags(&[("highway", "motorway"), ("maxspeed", "120")]);
        assert_eq!(truck.way(&fast).unwrap().speed_kmh, 80.0);
        let own = tags(&[("highway", "motorway"), ("maxspeed", "120"), ("maxspeed:hgv", "60")]);
        assert_eq!(truck.way(&own).unwrap().speed_kmh, 60.0);
        assert!(truck.way(&tags(&[("highway", "motorway"), ("maxheight", "3.5")])).is_none());
        assert!(truck.way(&tags(&[("highway", "motorway"), ("maxweight", "7.5 t")])).is_none());
        assert!(truck.way(&tags(&[("highway", "motorway"), ("maxheight", "4.5"), ("maxweight", "40")])).is_some());
        assert!(truck.way(&tags(&[("highway", "motorway"), ("maxheight", "12'6\"")])).is_some(), "not in metres: no limit");
        let bike = of(Mode::Bike, "[free_flow_kmh]\nresidential = 16\n");
        assert_eq!(bike.way(&tags(&[("highway", "residential"), ("maxspeed", "30")])).unwrap().speed_kmh, 16.0);
    }

    #[test]
    fn bad_road_classes() {
        assert!(Vehicle::from_road_classes("[free_flow_kmh]\nresidential = 0\n").is_err());
        assert!(Vehicle::from_road_classes("nonsense").is_err());
    }
}
