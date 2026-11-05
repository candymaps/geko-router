//! The pack of Spain as it is in the repository: `cargo test -p geko-module-router --test pack-es`.

use std::path::Path;

use geko_module_router::pack::CountryPack;
use geko_module_sdk::country::RegionCode;

fn pack() -> CountryPack {
    CountryPack::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/es")).unwrap()
}

fn region(code: &str) -> RegionCode {
    RegionCode::new(code).unwrap()
}

#[test]
fn it_says_what_a_country_says() {
    let pack = pack();
    assert_eq!(pack.country.as_str(), "ES");
    assert_eq!(pack.timezone, "Europe/Madrid");
    assert_eq!(pack.crs.unwrap().projected, "EPSG:25830");
    assert!(pack.calendar.fixed.contains(&"10-12".to_owned()));
    assert_eq!(pack.calendar.easter_relative, [-2]);
}

#[test]
fn every_province_has_a_box_and_an_extract() {
    let pack = pack();
    let groups: Vec<_> = pack.regions.iter().filter_map(|r| r.parent.as_ref()).collect();
    let partitions: Vec<_> = pack.regions.iter().filter(|r| !groups.contains(&&r.code)).collect();
    assert_eq!(partitions.len(), 52, "50 provinces, Ceuta and Melilla");
    for partition in partitions {
        assert!(pack.boxes.contains_key(&partition.code), "{} has no box", partition.code);
    }
    for region in &pack.regions {
        assert!(pack.osm.regions.contains_key(region.code.as_str()), "{} has no extract", region.code);
    }
}

#[test]
fn a_province_takes_the_extract_of_its_community() {
    let pack = pack();
    for (province, extract) in [("ES-PM", "islas-baleares"), ("ES-SE", "andalucia"), ("ES-M", "madrid"), ("ES-TF", "canary-islands"), ("ES-B", "cataluna")] {
        assert_eq!(pack.osm.regions[province], extract);
    }
    assert_eq!(pack.osm.country, ["spain", "canary-islands"]);
}

#[test]
fn a_box_holds_its_capital() {
    let pack = pack();
    for (province, lat, lon) in [("ES-PM", 39.57, 2.65), ("ES-M", 40.42, -3.70), ("ES-SE", 37.39, -5.98), ("ES-TF", 28.47, -16.25), ("ES-CE", 35.89, -5.32), ("ES-B", 41.39, 2.17)] {
        let b = pack.boxes[&region(province)];
        assert!((b.min_lat..=b.max_lat).contains(&lat) && (b.min_lon..=b.max_lon).contains(&lon), "{province}: {b:?}");
    }
}

#[test]
fn the_sources_are_those_of_the_licences_decided() {
    let pack = pack();
    assert_eq!(pack.sources.keys().collect::<Vec<_>>(), ["dgt-incidents", "madrid-sensors"]);
    assert!(pack.sources["madrid-sensors"].enabled);
    assert_eq!(pack.sources["madrid-sensors"].license.name, "CC BY 4.0");
    assert!(!pack.sources["dgt-incidents"].enabled, "until the DGT clarifies its licence (docs/ROUTER_PLAN.md, point 28)");
}
