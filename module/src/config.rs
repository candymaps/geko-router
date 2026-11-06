//! `geko-router.toml`: the configuration of the router module, beside `geko.toml`. The host keeps the
//! scope, the data directory and how data are downloaded; this file, where the OSM extracts and the
//! country packs come from, which profile is built, what is done with traffic and how it is served.

use std::collections::BTreeMap;

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

use crate::pack;
use crate::profile::Mode;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub osm: OsmConfig,
    pub packs: PacksConfig,
    pub profiles: ProfilesConfig,
    pub traffic: TrafficConfig,
    /// What changes in a source of a country pack, by its name: any field of its file. Checked
    /// against the packs of the scope when they are at hand (`Setup::check`).
    pub sources: BTreeMap<String, toml::Table>,
    pub serve: ServeConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsmConfig {
    /// `auto`, Geofabrik, or the URL of a mirror with its layout: the same extracts as the
    /// geocoder's, which share their downloads.
    pub mirror: String,
}

impl Default for OsmConfig {
    fn default() -> Self {
        Self { mirror: "auto".into() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacksConfig {
    /// Where the country packs are published, without the country.
    pub registry: String,
}

impl Default for PacksConfig {
    fn default() -> Self {
        Self { registry: pack::DEFAULT_REGISTRY.to_owned() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilesConfig {
    /// The profile the graph is built for, one for each bundle. It changes what is generated.
    pub default: String,
}

impl Default for ProfilesConfig {
    fn default() -> Self {
        Self { default: "car".into() }
    }
}

/// Traffic. Off, the router downloads nothing for it and does nothing with it: routes are at free
/// flow. With `enabled` and `live`, it polls the sources of the country packs of the scope, in the
/// background, each as often as its pack says, and routes avoid what is slow.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrafficConfig {
    pub enabled: bool,
    pub live: bool,
}

impl Default for TrafficConfig {
    fn default() -> Self {
        Self { enabled: false, live: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Load the graph when a bundle arrives; off, with the first request that needs it, which then
    /// waits for it. Until then the process takes no memory for it.
    pub preload: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self { preload: true }
    }
}

impl Config {
    /// The profile of the graph, which `parse` checked.
    pub fn mode(&self) -> Mode {
        Mode::parse(&self.profiles.default).unwrap_or_default()
    }

    pub fn parse(text: &str, file: &str) -> Result<Self> {
        let file = if file.is_empty() { "geko-router.toml" } else { file };
        let config: Self = toml::from_str(text).map_err(|e| GekoError::Config(format!("{file}: {e}")))?;
        if Mode::parse(&config.profiles.default).is_none() {
            return Err(GekoError::Config(format!("{file}: [profiles] default = {:?}: the profiles are {}", config.profiles.default, Mode::NAMES.join(", "))));
        }
        if config.packs.registry.is_empty() {
            return Err(GekoError::Config(format!("{file}: [packs] registry is empty")));
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_errors() {
        let config = Config::parse("", "").unwrap();
        assert_eq!(config.osm.mirror, "auto");
        assert_eq!(config.packs.registry, pack::DEFAULT_REGISTRY);
        assert_eq!(config.profiles.default, "car");
        assert!(config.serve.preload && config.sources.is_empty());
        assert_eq!(Config::parse("[osm]\nmirror = \"https://m.example\"", "").unwrap().osm.mirror, "https://m.example");
        let error = Config::parse("[osm]\nmirrr = 1", "/etc/geko/geko-router.toml").unwrap_err().to_string();
        assert!(error.contains("/etc/geko/geko-router.toml") && error.contains("mirrr"), "{error}");
    }

    #[test]
    fn traffic_is_off_unless_asked_for() {
        let config = Config::parse("", "").unwrap();
        assert!(!config.traffic.enabled && config.traffic.live);
        let on = Config::parse("[traffic]\nenabled = true\nlive = false\n\n[sources.madrid-sensors]\nenabled = false", "").unwrap();
        assert!(on.traffic.enabled && !on.traffic.live);
        assert_eq!(on.sources["madrid-sensors"]["enabled"].as_bool(), Some(false));
    }

    #[test]
    fn the_options_of_the_plan() {
        let config = Config::parse("[packs]\nregistry = \"r.example/c\"\n[profiles]\ndefault = \"car\"\n[serve]\npreload = false", "").unwrap();
        assert!(config.packs.registry == "r.example/c" && !config.serve.preload);
    }

    #[test]
    fn bad_options_name_the_file_and_the_key() {
        for (text, word) in [("[traffic]\nenable = true", "enable"), ("[traffic]\nlive = 3", "live"), ("[profiles]\ndefault = \"plane\"", "default"), ("[packs]\nregistry = \"\"", "registry"), ("[packs]\nurl = \"x\"", "url"), ("[serve]\ncountries = []", "countries"), ("[traffic.madrid]\nenabled = true", "madrid")] {
            let error = Config::parse(text, "geko-router.toml").unwrap_err().to_string();
            assert!(error.contains("geko-router.toml") && error.contains(word), "{error}");
        }
    }
}
