//! The car profile: which ways a car can drive on, in which direction and how fast. The speeds
//! by road class are data of the country pack (`data/road-classes.toml`), so the profile is the same
//! code for every country. A ferry that takes cars is a way too, at the time it says it lasts.

use std::collections::HashMap;

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

use crate::osm::{Tags, tag};

/// How fast a ferry goes when it does not say how long it lasts: what OSRM's `car.lua` takes, so that
/// the two can be compared. A ferry goes faster than this, but a crossing is rarely the best choice
/// and a route that takes one with no timetable should not look short.
const FERRY_KMH: f32 = 5.0;

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
}

fn no_reduction() -> f32 {
    1.0
}

#[derive(Debug, Clone)]
pub struct Car {
    free_flow_kmh: HashMap<String, f32>,
    maxspeed_factor: f32,
}

impl Car {
    /// From the contents of `data/road-classes.toml` of a pack. Its classes are the `highway` values a
    /// car can drive on.
    pub fn from_road_classes(toml: &str) -> Result<Self> {
        let classes: RoadClasses = toml::from_str(toml).map_err(|e| GekoError::Data(format!("road-classes.toml: {e}")))?;
        if let Some((class, speed)) = classes.free_flow_kmh.iter().find(|(_, s)| !(s.is_finite() && **s > 0.0)) {
            return Err(GekoError::Data(format!("road-classes.toml: the speed of `{class}` is {speed}, not a positive number")));
        }
        if !(classes.maxspeed_factor.is_finite() && classes.maxspeed_factor > 0.0 && classes.maxspeed_factor <= 1.0) {
            return Err(GekoError::Data(format!("road-classes.toml: maxspeed_factor is {}, not a number above 0 and up to 1", classes.maxspeed_factor)));
        }
        Ok(Self { free_flow_kmh: classes.free_flow_kmh, maxspeed_factor: classes.maxspeed_factor })
    }

    /// `None` when a car cannot use the way.
    pub fn way(&self, tags: &Tags) -> Option<Way> {
        if tag(tags, "route") == Some("ferry") {
            return allowed(tags).then(|| Way { speed_kmh: FERRY_KMH, direction: direction(tags, ""), ferry: true, duration_s: tag(tags, "duration").and_then(duration_s) });
        }
        let highway = tag(tags, "highway")?;
        let class_speed = *self.free_flow_kmh.get(highway)?;
        if tag(tags, "area") == Some("yes") || !allowed(tags) {
            return None;
        }
        let speed_kmh = tag(tags, "maxspeed").and_then(maxspeed_kmh).map_or(class_speed, |limit| limit * self.maxspeed_factor);
        Some(Way { speed_kmh, direction: direction(tags, highway), ferry: false, duration_s: None })
    }
}

/// The most specific access tag decides: `motor_vehicle`, then `vehicle`, then `access`.
fn allowed(tags: &Tags) -> bool {
    let value = ["motor_vehicle", "motorcar", "vehicle", "access"].iter().find_map(|key| tag(tags, key));
    !matches!(value, Some("no" | "private" | "agricultural" | "forestry" | "emergency"))
}

fn direction(tags: &Tags, highway: &str) -> Direction {
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

    fn car() -> Car {
        Car::from_road_classes(CLASSES).unwrap()
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
        let car = Car::from_road_classes("maxspeed_factor = 0.8\n[free_flow_kmh]\nprimary = 65\n").unwrap();
        let speed = |extra: &[(&str, &str)]| {
            let mut all = vec![("highway", "primary")];
            all.extend_from_slice(extra);
            car.way(&tags(&all)).unwrap().speed_kmh
        };
        assert_eq!(speed(&[]), 65.0);
        assert!((speed(&[("maxspeed", "100")]) - 80.0).abs() < 1e-4);
        assert_eq!(speed(&[("maxspeed", "none")]), 65.0);
        for bad in ["0", "1.5", "-1"] {
            assert!(Car::from_road_classes(&format!("maxspeed_factor = {bad}\n[free_flow_kmh]\nprimary = 65\n")).is_err(), "{bad}");
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

    #[test]
    fn bad_road_classes() {
        assert!(Car::from_road_classes("[free_flow_kmh]\nresidential = 0\n").is_err());
        assert!(Car::from_road_classes("nonsense").is_err());
    }
}
