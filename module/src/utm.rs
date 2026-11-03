//! ETRS89 / UTM zone 30N (EPSG:25830) to latitude and longitude: the coordinates the sensors of Madrid
//! come in. The Krüger series of the transverse Mercator projection on the GRS80 ellipsoid, exact to
//! well under a millimetre in the zone, which is more than a sensor's position is worth. ETRS89 and
//! WGS84 agree within a metre or two, far less than the distance a sensor is matched to a street.

const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_222_101;
const K0: f64 = 0.9996;
const FALSE_EASTING: f64 = 500_000.0;
/// The central meridian of zone 30, in degrees.
const LON0: f64 = -3.0;

struct Series {
    /// The radius of the rectifying sphere times `K0`.
    radius: f64,
    beta: [f64; 4],
    delta: [f64; 4],
}

fn series() -> Series {
    let n = F / (2.0 - F);
    let (n2, n3, n4) = (n * n, n * n * n, n * n * n * n);
    Series {
        radius: K0 * A / (1.0 + n) * (1.0 + n2 / 4.0 + n4 / 64.0),
        beta: [
            n / 2.0 - 2.0 * n2 / 3.0 + 37.0 * n3 / 96.0 - n4 / 360.0,
            n2 / 48.0 + n3 / 15.0 - 437.0 * n4 / 1440.0,
            17.0 * n3 / 480.0 - 37.0 * n4 / 840.0,
            4397.0 * n4 / 161_280.0,
        ],
        delta: [
            2.0 * n - 2.0 * n2 / 3.0 - 2.0 * n3 + 116.0 * n4 / 45.0,
            7.0 * n2 / 3.0 - 8.0 * n3 / 5.0 - 227.0 * n4 / 45.0,
            56.0 * n3 / 15.0 - 136.0 * n4 / 35.0,
            4279.0 * n4 / 630.0,
        ],
    }
}

/// `(latitude, longitude)` in degrees of the point at `easting` and `northing`, in metres.
pub fn to_lat_lon(easting: f64, northing: f64) -> (f64, f64) {
    let s = series();
    let xi = northing / s.radius;
    let eta = (easting - FALSE_EASTING) / s.radius;
    let (mut xi_p, mut eta_p) = (xi, eta);
    for (j, beta) in s.beta.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi_p -= beta * (k * xi).sin() * (k * eta).cosh();
        eta_p -= beta * (k * xi).cos() * (k * eta).sinh();
    }
    let chi = (xi_p.sin() / eta_p.cosh()).asin();
    let mut lat = chi;
    for (j, delta) in s.delta.iter().enumerate() {
        lat += delta * (2.0 * (j as f64 + 1.0) * chi).sin();
    }
    let lon = LON0.to_radians() + eta_p.sinh().atan2(xi_p.cos());
    (lat.to_degrees(), lon.to_degrees())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The projection the other way, from its own series, to check the inverse against.
    fn to_utm(lat: f64, lon: f64) -> (f64, f64) {
        let n = F / (2.0 - F);
        let (n2, n3, n4) = (n * n, n * n * n, n * n * n * n);
        let alpha = [
            n / 2.0 - 2.0 * n2 / 3.0 + 5.0 * n3 / 16.0 + 41.0 * n4 / 180.0,
            13.0 * n2 / 48.0 - 3.0 * n3 / 5.0 + 557.0 * n4 / 1440.0,
            61.0 * n3 / 240.0 - 103.0 * n4 / 140.0,
            49_561.0 * n4 / 161_280.0,
        ];
        let radius = series().radius;
        let (phi, dl) = (lat.to_radians(), (lon - LON0).to_radians());
        let c = 2.0 * n.sqrt() / (1.0 + n);
        let t = ((phi.sin()).atanh() - c * (c * phi.sin()).atanh()).sinh();
        let xi = (t / dl.cos()).atan();
        let eta = (dl.sin() / (1.0 + t * t).sqrt()).atanh();
        let (mut e, mut north) = (eta, xi);
        for (j, a) in alpha.iter().enumerate() {
            let k = 2.0 * (j as f64 + 1.0);
            e += a * (k * xi).cos() * (k * eta).sinh();
            north += a * (k * xi).sin() * (k * eta).cosh();
        }
        (FALSE_EASTING + radius * e, radius * north)
    }

    #[test]
    fn the_central_meridian_is_a_straight_line_north() {
        for northing in [0.0, 4_000_000.0, 4_472_588.0] {
            let (_, lon) = to_lat_lon(500_000.0, northing);
            assert!((lon + 3.0).abs() < 1e-12, "{lon}");
        }
        let (lat, _) = to_lat_lon(500_000.0, 0.0);
        assert!(lat.abs() < 1e-12);
    }

    /// 40 degrees north on the central meridian is 4 427 757 m north in this zone.
    #[test]
    fn the_meridian_arc_at_forty_degrees() {
        let (lat, _) = to_lat_lon(500_000.0, 4_427_757.2);
        assert!((lat - 40.0).abs() < 1e-5, "{lat}");
    }

    #[test]
    fn both_ways_agree_across_spain() {
        for (lat, lon) in [(40.4168, -3.7038), (39.47, -0.38), (41.38, -2.17), (37.39, -5.99), (43.26, -2.93), (42.88, -8.54)] {
            let (e, n) = to_utm(lat, lon);
            let (back_lat, back_lon) = to_lat_lon(e, n);
            assert!((back_lat - lat).abs() < 1e-9 && (back_lon - lon).abs() < 1e-9, "{lat},{lon} -> {e},{n} -> {back_lat},{back_lon}");
        }
    }

    #[test]
    fn the_sensors_of_madrid_are_in_madrid() {
        // The first sensor of the feed (Sepúlveda, Cebreros).
        let (lat, lon) = to_lat_lon(436_004.103, 4_472_588.474);
        assert!((40.38..40.42).contains(&lat) && (-3.78..-3.72).contains(&lon), "{lat},{lon}");
    }
}
