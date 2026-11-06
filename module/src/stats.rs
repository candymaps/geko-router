//! What the module has answered, for `v1/stats`: requests, errors and time of every route, and the
//! last hour minute by minute. The host measures requests per route for `/metrics`, but a module
//! cannot read it; these are its own counters, a few atomics per request, with no lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Upper bounds of the time buckets, in microseconds: from a nearest street to a large matrix.
const BOUNDS_US: [u64; 14] = [50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 1_000_000, 5_000_000];
const BUCKETS: usize = BOUNDS_US.len() + 1;
const MINUTES: usize = 60;
const SECONDS: usize = 60;

/// The routes that are counted, as `Request::route` names them.
const ROUTES: [&str; 6] = ["v1/route", "v1/nearest", "v1/table", "v1/isochrone", "v1/match", "v1/trip"];

fn bucket(us: u64) -> usize {
    BOUNDS_US.iter().position(|&b| us <= b).unwrap_or(BOUNDS_US.len())
}

/// The upper bound, in microseconds, of the bucket that holds the `q` quantile of the counts: the
/// real value is at most that. `None` without counts, or when it falls past the last bound.
fn quantile_us(counts: &[u64; BUCKETS], q: f64) -> Option<u64> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let wanted = (total as f64 * q).ceil().max(1.0) as u64;
    let mut seen = 0;
    for (i, &n) in counts.iter().enumerate() {
        seen += n;
        if seen >= wanted {
            return BOUNDS_US.get(i).copied();
        }
    }
    None
}

#[derive(Default)]
struct Route {
    requests: AtomicU64,
    errors: AtomicU64,
    time: [AtomicU64; BUCKETS],
}

/// A minute of the last hour, or a second of the last minute. The thread that finds the slot of an
/// older period starts it over; the few requests of the same instant that land before the reset are
/// lost.
#[derive(Default)]
struct Slot {
    /// Minutes (seconds, in the ring of seconds) since the epoch.
    at: AtomicU64,
    requests: AtomicU64,
    time: [AtomicU64; BUCKETS],
}

impl Slot {
    fn add(&self, at: u64, bucket: usize) {
        let seen = self.at.load(Ordering::Relaxed);
        if seen != at && self.at.compare_exchange(seen, at, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            self.requests.store(0, Ordering::Relaxed);
            self.time.iter().for_each(|t| t.store(0, Ordering::Relaxed));
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.time[bucket].fetch_add(1, Ordering::Relaxed);
    }

    /// Requests and time counts if the slot holds `at`, zero otherwise.
    fn read(&self, at: u64) -> (u64, [u64; BUCKETS]) {
        if self.at.load(Ordering::Relaxed) == at {
            (self.requests.load(Ordering::Relaxed), std::array::from_fn(|i| self.time[i].load(Ordering::Relaxed)))
        } else {
            (0, [0; BUCKETS])
        }
    }
}

/// The slots that end at `now`, oldest first.
fn window<const N: usize>(ring: &[Slot; N], now: u64) -> Vec<(u64, [u64; BUCKETS])> {
    (0..N as u64)
        .map(|i| {
            let at = now.saturating_sub(N as u64 - 1 - i);
            ring[(at % N as u64) as usize].read(at)
        })
        .collect()
}

pub struct Stats {
    started: u64,
    routes: [Route; ROUTES.len()],
    minutes: [Slot; MINUTES],
    seconds: [Slot; SECONDS],
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Default for Stats {
    fn default() -> Self {
        Self { started: now_secs(), routes: Default::default(), minutes: std::array::from_fn(|_| Slot::default()), seconds: std::array::from_fn(|_| Slot::default()) }
    }
}

impl Stats {
    pub fn record(&self, route: &str, status: u16, micros: u64) {
        self.record_at(now_secs(), route, status, micros);
    }

    fn record_at(&self, second: u64, route: &str, status: u16, micros: u64) {
        let Some(i) = ROUTES.iter().position(|r| *r == route) else { return };
        let b = bucket(micros);
        let r = &self.routes[i];
        r.requests.fetch_add(1, Ordering::Relaxed);
        if status >= 400 {
            r.errors.fetch_add(1, Ordering::Relaxed);
        }
        r.time[b].fetch_add(1, Ordering::Relaxed);
        let minute = second / 60;
        self.minutes[(minute % MINUTES as u64) as usize].add(minute, b);
        self.seconds[(second % SECONDS as u64) as usize].add(second, b);
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot_at(now_secs())
    }

    fn snapshot_at(&self, now_second: u64) -> Value {
        let now = now_second / 60;
        let routes: Vec<Value> = ROUTES
            .iter()
            .zip(&self.routes)
            .map(|(name, r)| {
                let counts: [u64; BUCKETS] = std::array::from_fn(|i| r.time[i].load(Ordering::Relaxed));
                json!({
                    "route": name,
                    "requests": r.requests.load(Ordering::Relaxed),
                    "errors": r.errors.load(Ordering::Relaxed),
                    "p50_us": quantile_us(&counts, 0.5),
                    "p95_us": quantile_us(&counts, 0.95),
                    "p99_us": quantile_us(&counts, 0.99),
                })
            })
            .collect();
        // Oldest first; a period with no requests counts zero.
        let series = window(&self.minutes, now);
        let recent = window(&self.seconds, now_second);
        json!({
            "since_unix": self.started,
            "routes": routes,
            "last_hour": {
                "end_unix": (now + 1) * 60,
                "requests": series.iter().map(|m| m.0).collect::<Vec<_>>(),
                "p99_us": series.iter().map(|m| quantile_us(&m.1, 0.99)).collect::<Vec<_>>(),
            },
            "last_minute": {
                "end_unix": now_second + 1,
                "requests": recent.iter().map(|m| m.0).collect::<Vec<_>>(),
                "p99_us": recent.iter().map(|m| quantile_us(&m.1, 0.99)).collect::<Vec<_>>(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_are_the_bound_of_their_bucket() {
        let mut counts = [0; BUCKETS];
        assert_eq!(quantile_us(&counts, 0.5), None);
        counts[bucket(80)] = 90;
        counts[bucket(400)] = 9;
        counts[bucket(4_000)] = 1;
        assert_eq!(quantile_us(&counts, 0.5), Some(100));
        assert_eq!(quantile_us(&counts, 0.95), Some(500));
        assert_eq!(quantile_us(&counts, 1.0), Some(5_000));
        counts[bucket(9_000_000)] = 100;
        assert_eq!(quantile_us(&counts, 1.0), None, "past the last bound");
    }

    #[test]
    fn counts_routes_errors_and_the_last_hour() {
        let stats = Stats::default();
        stats.record_at(60_000, "v1/route", 200, 230);
        stats.record_at(60_000, "v1/route", 404, 40);
        stats.record_at(60_600, "v1/table", 200, 2_000);
        stats.record_at(60_600, "v1/status", 200, 2);
        let s = stats.snapshot_at(60_600);
        let route = &s["routes"][0];
        assert_eq!((route["requests"].as_u64(), route["errors"].as_u64(), route["p99_us"].as_u64()), (Some(2), Some(1), Some(250)));
        let hour = s["last_hour"]["requests"].as_array().unwrap();
        assert_eq!(hour.len(), MINUTES);
        assert_eq!((hour[MINUTES - 1].as_u64(), hour[MINUTES - 11].as_u64(), hour[0].as_u64()), (Some(1), Some(2), Some(0)), "a route that is not counted adds nothing");
    }

    #[test]
    fn a_minute_an_hour_old_starts_over() {
        let stats = Stats::default();
        stats.record_at(60_000, "v1/route", 200, 3);
        stats.record_at(60_000 + 60 * MINUTES as u64, "v1/route", 200, 3);
        let s = stats.snapshot_at(60_000 + 60 * MINUTES as u64);
        let total: u64 = s["last_hour"]["requests"].as_array().unwrap().iter().filter_map(Value::as_u64).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn the_last_minute_goes_second_by_second() {
        let stats = Stats::default();
        stats.record_at(5_000, "v1/route", 200, 3);
        stats.record_at(5_058, "v1/route", 200, 3);
        stats.record_at(5_058, "v1/route", 200, 3);
        stats.record_at(5_059, "v1/route", 200, 3);
        let s = stats.snapshot_at(5_059);
        let secs = s["last_minute"]["requests"].as_array().unwrap();
        assert_eq!(secs.len(), SECONDS);
        assert_eq!((secs[SECONDS - 1].as_u64(), secs[SECONDS - 2].as_u64(), secs[0].as_u64()), (Some(1), Some(2), Some(1)), "5,000 is 59 seconds ago, still inside");
        assert_eq!(s["last_minute"]["end_unix"].as_u64(), Some(5_060));
    }
}
