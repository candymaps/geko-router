//! The usual speed of the roads by hour and day of the week, learned when the bundle is built from the
//! measurements a source keeps (kind `history-csv`), and what the bundle says of traffic: the sensors
//! matched and these profiles (`router/traffic`). A week has 168 slots, a holiday of the calendar
//! of the pack counting as a Sunday.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Timelike, Utc};
use chrono_tz::Tz;
use geko_module_sdk::{GekoError, Result};
use serde::Deserialize;

use crate::pack::{self, Source};
use crate::sensors;

/// The kind of source this file reads.
pub const KIND: &str = "history-csv";

pub const SLOTS: usize = 7 * 24;
/// A slot with fewer measurements than this says nothing.
const MIN_SAMPLES: u32 = 4;
/// A slot of an edge that nothing was measured in.
const UNKNOWN: u16 = u16::MAX;

/// The file of the sensors matched, in the traffic part of the bundle.
pub const SENSORS_FILE: &str = "sensors.tsv";
const PROFILE_PREFIX: &str = "profile-";

/// The days that count as Sundays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holidays {
    /// Month and day.
    fixed: Vec<(u32, u32)>,
    /// Days from Easter Sunday.
    easter_relative: Vec<i32>,
}

impl Holidays {
    pub fn of(calendar: &pack::Calendar) -> Self {
        let fixed = calendar.fixed.iter().filter_map(|d| d.split_once('-')).filter_map(|(m, d)| Some((m.parse().ok()?, d.parse().ok()?))).collect();
        Self { fixed, easter_relative: calendar.easter_relative.clone() }
    }

    fn holds(&self, date: NaiveDate) -> bool {
        self.fixed.contains(&(date.month(), date.day())) || self.easter_relative.iter().any(|days| easter(date.year()).checked_add_signed(chrono::Duration::days(*days as i64)) == Some(date))
    }
}

/// Easter Sunday of a year of the Gregorian calendar (the anonymous algorithm).
fn easter(year: i32) -> NaiveDate {
    let (a, b, c) = (year % 19, year / 100, year % 100);
    let (d, e) = (b / 4, b % 4);
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let (i, k) = (c / 4, c % 4);
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32).expect("a date of Easter")
}

/// The slot of an hour of a local date.
fn slot(holidays: &Holidays, date: NaiveDate, hour: u32) -> usize {
    let day = if holidays.holds(date) { 6 } else { date.weekday().num_days_from_monday() as usize };
    day * 24 + hour as usize
}

/// What a source of this kind says of its files.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Spec {
    /// Zip files, each with a CSV.
    pub files: Vec<String>,
    /// The source of the sensors these are the measurements of, whose ids and levels they share.
    pub sensors: String,
    /// The load, in percent, at which each level of service above the first begins.
    pub load_levels: Vec<u32>,
    delimiter: String,
    time_format: String,
    columns: Columns,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Columns {
    id: String,
    time: String,
    load: String,
    error: String,
    error_ok: String,
}

impl Spec {
    pub fn of(source: &Source) -> std::result::Result<Self, String> {
        toml::Value::Table(source.table.clone()).try_into().map_err(|e: toml::de::Error| format!("source {}: {}", source.name, e.message()))
    }
}

/// The measurements of each sensor, by slot: their sum, in thousandths of free flow, and how many.
#[derive(Debug, Default)]
pub struct Sums {
    by_sensor: HashMap<u32, Vec<(u64, u32)>>,
}

impl Sums {
    /// Adds the rows of `csv` of the sensors in `wanted`, each at the speed of the level of its load.
    pub fn add(&mut self, spec: &Spec, levels: &sensors::Spec, holidays: &Holidays, wanted: &HashMap<u32, u32>, csv: impl BufRead) -> Result<u64> {
        let mut lines = csv.lines();
        let header = lines.next().ok_or_else(|| GekoError::Data("a file of measurements is empty".into()))??;
        let split = |line: &str| line.split(spec.delimiter.as_str()).map(|c| c.trim().trim_matches('"').to_owned()).collect::<Vec<_>>();
        let names = split(&header);
        let column = |name: &str| names.iter().position(|n| n == name).ok_or_else(|| GekoError::Data(format!("a file of measurements has no column {name:?}")));
        let (id, time, load, error) = (column(&spec.columns.id)?, column(&spec.columns.time)?, column(&spec.columns.load)?, column(&spec.columns.error)?);
        let mut used = 0;
        for line in lines {
            let line = line?;
            let cells: Vec<&str> = line.split(spec.delimiter.as_str()).map(|c| c.trim().trim_matches('"')).collect();
            let Some(sensor) = cells.get(id).and_then(|c| c.parse::<u32>().ok()).filter(|s| wanted.contains_key(s)) else { continue };
            if cells.get(error) != Some(&spec.columns.error_ok.as_str()) {
                continue;
            }
            let Some(load) = cells.get(load).and_then(|c| c.parse::<f64>().ok()).filter(|l| *l >= 0.0) else { continue };
            let Some(at) = cells.get(time).and_then(|c| NaiveDateTime::parse_from_str(c, &spec.time_format).ok()) else { continue };
            let level = spec.load_levels.iter().filter(|from| load >= **from as f64).count();
            let cell = &mut self.by_sensor.entry(sensor).or_insert_with(|| vec![(0, 0); SLOTS])[slot(holidays, at.date(), at.hour())];
            cell.0 += levels.speed(level as u8) as u64;
            cell.1 += 1;
            used += 1;
        }
        Ok(used)
    }

    /// The usual speed of each edge that has a sensor, by slot: the slowest where two share an edge.
    pub fn profile(&self, matched: &HashMap<u32, u32>) -> BTreeMap<u32, Vec<u16>> {
        let mut edges: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
        for (sensor, cells) in &self.by_sensor {
            let Some(edge) = matched.get(sensor) else { continue };
            let row = edges.entry(*edge).or_insert_with(|| vec![UNKNOWN; SLOTS]);
            for (speed, (sum, count)) in row.iter_mut().zip(cells) {
                if *count >= MIN_SAMPLES {
                    *speed = (*speed).min((*sum / *count as u64) as u16);
                }
            }
        }
        edges
    }
}

/// The rows of the CSV in the zip at `path` go through `read`.
pub fn read_zip<T>(path: &Path, read: impl FnOnce(&mut dyn BufRead) -> Result<T>) -> Result<T> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
    let entry = archive.by_index(0).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
    read(&mut BufReader::with_capacity(1 << 20, entry))
}

/// The usual speeds of the edges a source measures.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    timezone: Tz,
    holidays: Holidays,
    edges: Vec<(u32, Vec<u16>)>,
}

impl Profile {
    pub fn new(timezone: &str, holidays: Holidays, edges: BTreeMap<u32, Vec<u16>>) -> Result<Self> {
        let timezone = timezone.parse::<Tz>().map_err(|e| GekoError::Data(format!("time zone {timezone:?}: {e}")))?;
        Ok(Self { timezone, holidays, edges: edges.into_iter().collect() })
    }

    /// The slot of a moment, in the time of the country of the source.
    pub fn slot(&self, at: DateTime<Utc>) -> u16 {
        let local = at.with_timezone(&self.timezone);
        slot(&self.holidays, local.date_naive(), local.hour()) as u16
    }

    /// What is usual at a slot, on the edges where something is known then.
    pub fn speeds(&self, slot: u16) -> impl Iterator<Item = (u32, u16)> + '_ {
        self.edges.iter().map(move |(edge, row)| (*edge, row[slot as usize])).filter(|(_, speed)| *speed != UNKNOWN)
    }

    pub fn edges(&self) -> usize {
        self.edges.len()
    }

    fn write(&self, path: &Path) -> Result<()> {
        let list = |v: Vec<String>| v.join(",");
        let mut text = format!("timezone\t{}\nfixed\t{}\neaster\t{}\n", self.timezone.name(), list(self.holidays.fixed.iter().map(|(m, d)| format!("{m}-{d}")).collect()), list(self.holidays.easter_relative.iter().map(i32::to_string).collect()));
        for (edge, row) in &self.edges {
            text.push_str(&format!("{edge}\t{}\n", list(row.iter().map(u16::to_string).collect())));
        }
        fs::write(path, text)?;
        Ok(())
    }

    fn read(path: &Path) -> Result<Self> {
        let bad = || GekoError::Data(format!("{}: not a profile of traffic", path.display()));
        let text = fs::read_to_string(path)?;
        let mut lines = text.lines();
        let mut field = |name: &str| lines.next().and_then(|l| l.strip_prefix(name)).and_then(|l| l.strip_prefix('\t')).map(str::to_owned).ok_or_else(bad);
        let (timezone, fixed, easter) = (field("timezone")?, field("fixed")?, field("easter")?);
        let fixed = fixed.split(',').filter(|d| !d.is_empty()).map(|d| d.split_once('-').and_then(|(m, d)| Some((m.parse().ok()?, d.parse().ok()?))).ok_or_else(bad)).collect::<Result<_>>()?;
        let easter_relative = easter.split(',').filter(|d| !d.is_empty()).map(|d| d.parse().map_err(|_| bad())).collect::<Result<_>>()?;
        let mut edges = BTreeMap::new();
        for line in lines {
            let (edge, row) = line.split_once('\t').ok_or_else(bad)?;
            let row: Vec<u16> = row.split(',').map(|s| s.parse().map_err(|_| bad())).collect::<Result<_>>()?;
            if row.len() != SLOTS {
                return Err(bad());
            }
            edges.insert(edge.parse().map_err(|_| bad())?, row);
        }
        Self::new(&timezone, Holidays { fixed, easter_relative }, edges)
    }
}

/// What the bundle says of traffic: the edge of each sensor, by source, and the profiles.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Bundled {
    pub sensors: BTreeMap<String, HashMap<u32, u32>>,
    pub profiles: BTreeMap<String, Profile>,
}

impl Bundled {
    pub fn write(&self, dir: &Path) -> Result<()> {
        let mut rows: Vec<(&str, u32, u32)> = self.sensors.iter().flat_map(|(name, map)| map.iter().map(move |(sensor, edge)| (name.as_str(), *sensor, *edge))).collect();
        rows.sort_unstable();
        let text: String = rows.iter().map(|(name, sensor, edge)| format!("{name}\t{sensor}\t{edge}\n")).collect();
        fs::write(dir.join(SENSORS_FILE), text)?;
        for (name, profile) in &self.profiles {
            profile.write(&dir.join(format!("{PROFILE_PREFIX}{name}.tsv")))?;
        }
        Ok(())
    }

    /// What is in `dir`; nothing if the bundle was built with traffic off.
    pub fn read(dir: &Path) -> Result<Self> {
        let mut bundled = Self::default();
        if !dir.is_dir() {
            return Ok(bundled);
        }
        let path = dir.join(SENSORS_FILE);
        for line in fs::read_to_string(&path)?.lines() {
            let mut cells = line.split('\t');
            let (Some(name), Some(sensor), Some(edge)) = (cells.next(), cells.next().and_then(|c| c.parse().ok()), cells.next().and_then(|c| c.parse().ok())) else {
                return Err(GekoError::Data(format!("{}: {line:?}", path.display())));
            };
            bundled.sensors.entry(name.to_owned()).or_default().insert(sensor, edge);
        }
        let mut files: Vec<PathBuf> = fs::read_dir(dir)?.map(|e| Ok(e?.path())).collect::<Result<_>>()?;
        files.sort();
        for file in files {
            let name = file.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(PROFILE_PREFIX)).and_then(|n| n.strip_suffix(".tsv")).map(str::to_owned);
            if let Some(name) = name {
                bundled.profiles.insert(name, Profile::read(&file)?);
            }
        }
        Ok(bundled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::CountryPack;
    use chrono::TimeZone;

    fn pack() -> CountryPack {
        CountryPack::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../countries/es")).unwrap()
    }

    fn holidays() -> Holidays {
        Holidays::of(&pack().calendar)
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn easter_is_where_the_calendars_say() {
        assert_eq!(easter(2024), date(2024, 3, 31));
        assert_eq!(easter(2026), date(2026, 4, 5));
        assert_eq!(easter(2027), date(2027, 3, 28));
    }

    #[test]
    fn a_holiday_is_a_sunday() {
        let h = holidays();
        assert_eq!(slot(&h, date(2026, 11, 2), 8), 8, "a Monday");
        assert_eq!(slot(&h, date(2026, 11, 7), 0), 5 * 24, "a Saturday");
        assert_eq!(slot(&h, date(2026, 10, 12), 8), 6 * 24 + 8, "a fixed holiday, on a Monday");
        assert_eq!(slot(&h, date(2026, 4, 3), 8), 6 * 24 + 8, "Good Friday");
    }

    const CSV: &str = "id;\"fecha\";\"tipo_elem\";\"intensidad\";\"ocupacion\";\"carga\";\"vmed\";\"error\";periodo_integracion\n\
        7;\"2026-11-02 08:00:00\";\"URB\";\"100\";\"5\";\"80\";\"0\";\"N\";15\n\
        7;\"2026-11-02 08:15:00\";\"URB\";\"100\";\"5\";\"80\";\"0\";\"N\";15\n\
        7;\"2026-11-02 08:30:00\";\"URB\";\"100\";\"5\";\"60\";\"0\";\"N\";15\n\
        7;\"2026-11-02 08:45:00\";\"URB\";\"100\";\"5\";\"60\";\"0\";\"N\";15\n\
        7;\"2026-11-02 09:00:00\";\"URB\";\"100\";\"5\";\"10\";\"0\";\"N\";15\n\
        7;\"2026-11-02 10:00:00\";\"URB\";\"100\";\"5\";\"90\";\"0\";\"S\";15\n\
        8;\"2026-11-02 08:00:00\";\"URB\";\"100\";\"5\";\"90\";\"0\";\"N\";15\n";

    #[test]
    fn a_slot_is_the_mean_of_its_measurements_when_there_are_enough() {
        let pack = pack();
        let spec = Spec::of(&pack.sources["madrid-history"]).unwrap();
        let levels = sensors::Spec::of(&pack.sources[&spec.sensors]).unwrap();
        let matched: HashMap<u32, u32> = [(7, 70)].into();
        let mut sums = Sums::default();
        let used = sums.add(&spec, &levels, &holidays(), &matched, CSV.as_bytes()).unwrap();
        assert_eq!(used, 5, "not the failing row, nor the sensor without an edge");
        let profile = sums.profile(&matched);
        let row = &profile[&70];
        assert_eq!(row[8], (levels.speed(3) + levels.speed(2)) / 2);
        assert_eq!(row[9], UNKNOWN, "a single measurement");
    }

    #[test]
    fn a_profile_says_what_is_usual_in_local_time() {
        let mut row = vec![UNKNOWN; SLOTS];
        row[8] = 700;
        let profile = Profile::new("Europe/Madrid", holidays(), [(5, row)].into()).unwrap();
        // 07:30 UTC on a Monday of November is 08:30 in Madrid.
        let slot = profile.slot(Utc.with_ymd_and_hms(2026, 11, 2, 7, 30, 0).unwrap());
        assert_eq!(slot, 8);
        assert_eq!(profile.speeds(slot).collect::<Vec<_>>(), [(5, 700)]);
        assert_eq!(profile.speeds(9).count(), 0);
        // In summer the same hour is 09:30.
        assert_eq!(profile.slot(Utc.with_ymd_and_hms(2026, 7, 6, 7, 30, 0).unwrap()), 9);
    }

    #[test]
    fn what_the_bundle_holds_is_read_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Bundled::read(&dir.path().join("none")).unwrap(), Bundled::default());
        let mut row = vec![UNKNOWN; SLOTS];
        row[0] = 500;
        let bundled = Bundled { sensors: [("madrid-sensors".to_owned(), [(1, 2), (3, 4)].into())].into(), profiles: [("madrid-history".to_owned(), Profile::new("Europe/Madrid", holidays(), [(2, row)].into()).unwrap())].into() };
        bundled.write(dir.path()).unwrap();
        assert_eq!(Bundled::read(dir.path()).unwrap(), bundled);
    }
}
