//! The pack of Luxembourg as it is in the repository: `cargo test -p geko-module-router --test pack-lu`.

use std::path::Path;

use geko_module_router::pack::CountryPack;

fn pack() -> CountryPack {
    CountryPack::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/lu")).unwrap()
}

#[test]
fn it_says_what_a_country_says() {
    let pack = pack();
    assert_eq!(pack.country.as_str(), "LU");
    assert_eq!(pack.timezone, "Europe/Luxembourg");
    assert!(pack.calendar.fixed.contains(&"06-23".to_owned()), "the National Day");
    assert_eq!(pack.calendar.easter_relative, [1, 39, 50]);
}

#[test]
fn it_is_one_partition_with_only_openstreetmap() {
    let pack = pack();
    assert!(pack.regions.is_empty() && pack.boxes.is_empty());
    assert!(pack.osm.country.is_empty() && pack.osm.regions.is_empty(), "the index of Geofabrik gives its extract");
    assert!(pack.sources.is_empty());
}
