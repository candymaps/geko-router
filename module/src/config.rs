//! `geko-router.toml`: the configuration of the router module, beside `geko.toml`. The host keeps the
//! scope, the data directory and how data are downloaded; this file, where the OSM extracts come
//! from and whether live traffic is used.

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub osm: OsmConfig,
    pub traffic: TrafficConfig,
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

/// Live traffic. Off, the router downloads nothing for it and does nothing with it: routes are at
/// free flow. On, it polls the sources of the countries of the scope that have one, in the
/// background, and routes avoid what is slow.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrafficConfig {
    pub enabled: bool,
    /// Seconds between two polls of a source. The sources do not change faster than every five
    /// minutes, so less than a minute is refused.
    pub interval_secs: u64,
    /// Seconds after which a reading that could not be renewed is dropped and its roads go back to
    /// free flow: an old jam is worse than none.
    pub max_age_secs: u64,
    pub madrid: MadridConfig,
}

impl Default for TrafficConfig {
    fn default() -> Self {
        Self { enabled: false, interval_secs: 300, max_age_secs: 1800, madrid: MadridConfig::default() }
    }
}

/// The traffic sensors of the city of Madrid (CC BY 4.0, Ayuntamiento de Madrid), used when the scope
/// covers Madrid.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MadridConfig {
    pub enabled: bool,
    pub url: String,
}

impl Default for MadridConfig {
    fn default() -> Self {
        Self { enabled: true, url: "https://informo.madrid.es/informo/tmadrid/pm.xml".into() }
    }
}

impl Config {
    pub fn parse(text: &str, file: &str) -> Result<Self> {
        let file = if file.is_empty() { "geko-router.toml" } else { file };
        let config: Self = toml::from_str(text).map_err(|e| GekoError::Config(format!("{file}: {e}")))?;
        let traffic = &config.traffic;
        if traffic.interval_secs < 60 {
            return Err(GekoError::Config(format!("{file}: [traffic] interval_secs = {} is below 60 seconds", traffic.interval_secs)));
        }
        if traffic.max_age_secs < traffic.interval_secs {
            return Err(GekoError::Config(format!("{file}: [traffic] max_age_secs = {} is below interval_secs = {}", traffic.max_age_secs, traffic.interval_secs)));
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_errors() {
        assert_eq!(Config::parse("", "").unwrap().osm.mirror, "auto");
        assert_eq!(Config::parse("[osm]\nmirror = \"https://m.example\"", "").unwrap().osm.mirror, "https://m.example");
        let error = Config::parse("[osm]\nmirrr = 1", "/etc/geko/geko-router.toml").unwrap_err().to_string();
        assert!(error.contains("/etc/geko/geko-router.toml") && error.contains("mirrr"), "{error}");
    }

    #[test]
    fn traffic_is_off_unless_asked_for() {
        let config = Config::parse("", "").unwrap();
        assert!(!config.traffic.enabled);
        assert_eq!((config.traffic.interval_secs, config.traffic.max_age_secs), (300, 1800));
        assert!(config.traffic.madrid.enabled);
        let on = Config::parse("[traffic]\nenabled = true\ninterval_secs = 120\n\n[traffic.madrid]\nenabled = false", "").unwrap();
        assert!(on.traffic.enabled && !on.traffic.madrid.enabled && on.traffic.interval_secs == 120);
    }

    #[test]
    fn bad_traffic_options_name_the_file_and_the_key() {
        for (text, word) in [("[traffic]\ninterval_secs = 30", "interval_secs"), ("[traffic]\nmax_age_secs = 100", "max_age_secs"), ("[traffic]\nenable = true", "enable"), ("[traffic.madrid]\nurl = 3", "url")] {
            let error = Config::parse(text, "geko-router.toml").unwrap_err().to_string();
            assert!(error.contains("geko-router.toml") && error.contains(word), "{error}");
        }
    }
}
