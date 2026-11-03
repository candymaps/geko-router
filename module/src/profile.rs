//! The car profile: which ways a car can drive on, in which direction and how fast. The speeds
//! by road class are data of the country pack (`data/road-classes.toml`), so the profile is the same
//! code for every country.

use std::collections::HashMap;

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

use crate::osm::{Tags, tag};

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
}

#[derive(Debug, Deserialize)]
struct RoadClasses {
    free_flow_kmh: HashMap<String, f32>,
}

#[derive(Debug, Clone)]
pub struct Car {
    free_flow_kmh: HashMap<String, f32>,
}

impl Car {
    /// From the contents of `data/road-classes.toml` of a pack. Its classes are the `highway` values a
    /// car can drive on.
    pub fn from_road_classes(toml: &str) -> Result<Self> {
        let classes: RoadClasses = toml::from_str(toml).map_err(|e| GekoError::Data(format!("road-classes.toml: {e}")))?;
        if let Some((class, speed)) = classes.free_flow_kmh.iter().find(|(_, s)| !(s.is_finite() && **s > 0.0)) {
            return Err(GekoError::Data(format!("road-classes.toml: the speed of `{class}` is {speed}, not a positive number")));
        }
        Ok(Self { free_flow_kmh: classes.free_flow_kmh })
    }

    /// `None` when a car cannot use the way.
    pub fn way(&self, tags: &Tags) -> Option<Way> {
        let highway = tag(tags, "highway")?;
        let class_speed = *self.free_flow_kmh.get(highway)?;
        if tag(tags, "area") == Some("yes") || !allowed(tags) {
            return None;
        }
        let speed_kmh = tag(tags, "maxspeed").and_then(maxspeed_kmh).unwrap_or(class_speed);
        Some(Way { speed_kmh, direction: direction(tags, highway) })
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
