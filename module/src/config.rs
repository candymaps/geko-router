//! `geko-router.toml`: the configuration of the router module, beside `geko.toml`. The host keeps the
//! scope, the data directory and how data are downloaded; this file, where the OSM extracts come from.

use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub osm: OsmConfig,
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

impl Config {
    pub fn parse(text: &str, file: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| GekoError::Config(format!("{}: {e}", if file.is_empty() { "geko-router.toml" } else { file })))
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
}
