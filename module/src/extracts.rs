//! The Geofabrik extracts of the scope: those of its countries and regions, chosen from the Geofabrik
//! index and `countries.toml`, the same as the geocoder's so that both share their downloads.

use std::collections::{BTreeMap, BTreeSet};

use geko_module_sdk::scope::{Partition, Scope};
use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

pub const DEFAULT_BASE: &str = "https://download.geofabrik.de";
pub const INDEX_FILE: &str = "index-v1-nogeom.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Extract {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(rename = "iso3166-1:alpha2", default)]
    pub iso3166_1: Vec<String>,
    #[serde(rename = "iso3166-2", default)]
    pub iso3166_2: Vec<String>,
    pub urls: Urls,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Urls {
    pub pbf: String,
}

impl Extract {
    pub fn file_name(&self) -> String {
        format!("{}-latest.osm.pbf", self.id)
    }

    /// Its URL, from `base` (Geofabrik or a mirror with its layout).
    pub fn url(&self, base: &str) -> String {
        match self.urls.pbf.strip_prefix(DEFAULT_BASE) {
            Some(path) => format!("{}{path}", base.trim_end_matches('/')),
            None => self.urls.pbf.clone(),
        }
    }
}

/// What `countries.toml` says of a country.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Country {
    /// The extracts whose union is the whole country; empty: those the index gives its ISO code.
    #[serde(default)]
    country: Vec<String>,
    /// The extract of each region or province, by ISO 3166-2 code.
    #[serde(default)]
    regions: BTreeMap<String, String>,
}

fn countries() -> BTreeMap<String, Country> {
    toml::from_str(include_str!("../countries.toml")).expect("countries.toml is valid")
}

pub struct Catalog {
    extracts: Vec<Extract>,
    countries: BTreeMap<String, Country>,
}

impl Catalog {
    pub fn parse(json: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Index {
            features: Vec<Feature>,
        }
        #[derive(Deserialize)]
        struct Feature {
            properties: Extract,
        }
        let index: Index = serde_json::from_slice(json).map_err(|e| GekoError::Data(format!("invalid Geofabrik index: {e}")))?;
        Ok(Self { extracts: index.features.into_iter().map(|f| f.properties).collect(), countries: countries() })
    }

    fn get(&self, id: &str) -> Result<&Extract> {
        self.extracts.iter().find(|e| e.id == id).ok_or_else(|| GekoError::Data(format!("Geofabrik extract `{id}` not found in the index")))
    }

    /// The extracts of the scope, each once, ordered by id (the order they are merged in).
    pub fn for_scope(&self, scope: &Scope) -> Result<Vec<&Extract>> {
        let mut ids = BTreeSet::new();
        for p in scope.partitions() {
            ids.extend(self.partition_ids(&p)?);
        }
        ids.iter().map(|id| self.get(id)).collect()
    }

    fn partition_ids(&self, partition: &Partition) -> Result<Vec<String>> {
        let country = self.countries.get(&partition.country.lowercase());
        let Some(region) = &partition.region else { return self.country_ids(partition.country.as_str()) };
        if let Some(id) = country.and_then(|c| c.regions.get(region.as_str())) {
            return Ok(vec![id.clone()]);
        }
        let by_iso: Vec<String> =
            self.extracts.iter().filter(|e| e.iso3166_2.iter().any(|c| c.eq_ignore_ascii_case(region.as_str()))).map(|e| e.id.clone()).collect();
        if !by_iso.is_empty() {
            return Ok(by_iso);
        }
        tracing::warn!(%region, "no regional Geofabrik extract; using the whole country");
        self.country_ids(partition.country.as_str())
    }

    fn country_ids(&self, country: &str) -> Result<Vec<String>> {
        if let Some(c) = self.countries.get(&country.to_ascii_lowercase())
            && !c.country.is_empty()
        {
            return Ok(c.country.clone());
        }
        let matching: Vec<&Extract> = self.extracts.iter().filter(|e| e.iso3166_1.iter().any(|c| c == country)).collect();
        let ids: BTreeSet<&str> = matching.iter().map(|e| e.id.as_str()).collect();
        let top: Vec<String> =
            matching.iter().filter(|e| e.parent.as_deref().is_none_or(|p| !ids.contains(p))).map(|e| e.id.clone()).collect();
        if top.is_empty() {
            return Err(GekoError::Data(format!("no Geofabrik extract for country {country}")));
        }
        Ok(top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_module_sdk::country::{CountryCode, RegionCode};

    const INDEX: &str = r#"{ "features": [
        { "properties": { "id": "europe", "urls": { "pbf": "https://download.geofabrik.de/europe-latest.osm.pbf" } } },
        { "properties": { "id": "spain", "parent": "europe", "iso3166-1:alpha2": ["ES"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/spain-latest.osm.pbf" } } },
        { "properties": { "id": "islas-baleares", "parent": "spain",
            "urls": { "pbf": "https://download.geofabrik.de/europe/spain/islas-baleares-latest.osm.pbf" } } },
        { "properties": { "id": "canary-islands", "parent": "africa",
            "urls": { "pbf": "https://download.geofabrik.de/africa/canary-islands-latest.osm.pbf" } } },
        { "properties": { "id": "france", "parent": "europe", "iso3166-1:alpha2": ["FR"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/france-latest.osm.pbf" } } },
        { "properties": { "id": "guyane", "parent": "france", "iso3166-1:alpha2": ["FR"], "iso3166-2": ["FR-GF"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/france/guyane-latest.osm.pbf" } } }
    ] }"#;

    fn ids(scope: &[(&str, &[&str])]) -> Vec<String> {
        let mut s = Scope::default();
        for (country, regions) in scope {
            s.add(CountryCode::new(country).unwrap(), regions.iter().map(|r| RegionCode::new(r).unwrap()).collect()).unwrap();
        }
        Catalog::parse(INDEX.as_bytes()).unwrap().for_scope(&s).unwrap().into_iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn extracts_of_countries_regions_and_provinces() {
        assert_eq!(ids(&[("ES", &[])]), ["canary-islands", "spain"]);
        assert_eq!(ids(&[("ES", &["ES-PM", "ES-IB"])]), ["islas-baleares"]);
        assert_eq!(ids(&[("FR", &[])]), ["france"]);
        assert_eq!(ids(&[("FR", &["FR-GF"])]), ["guyane"]);
        assert_eq!(ids(&[("FR", &["FR-75"])]), ["france"]);
        assert_eq!(ids(&[("ES", &["ES-PM"]), ("FR", &["FR-GF"])]), ["guyane", "islas-baleares"]);
    }

    #[test]
    fn mirrors_keep_the_path() {
        let c = Catalog::parse(INDEX.as_bytes()).unwrap();
        let e = c.get("islas-baleares").unwrap();
        assert_eq!(e.url("https://mirror.example/geofabrik/"), "https://mirror.example/geofabrik/europe/spain/islas-baleares-latest.osm.pbf");
        assert_eq!(e.url(DEFAULT_BASE), e.urls.pbf);
        assert_eq!(e.file_name(), "islas-baleares-latest.osm.pbf");
    }
}
