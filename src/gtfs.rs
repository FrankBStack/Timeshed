//! A minimal GTFS reader: only the files RAPTOR needs, read into compact
//! index-based structs. Reads either an unpacked directory or a zip.

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Stop {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// 0 = stop/platform, 1 = station, 2 = entrance, ...
    pub location_type: u8,
    pub parent_station: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub id: String,
    pub short_name: String,
    pub long_name: String,
    pub route_type: u16,
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Trip {
    pub id: String,
    /// index into `Feed::routes`
    pub route: u32,
    /// index into `Feed::services`
    pub service: u32,
    pub headsign: String,
    pub direction: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct StopTime {
    pub trip: u32,
    pub stop: u32,
    pub seq: u32,
    /// seconds after midnight of the service day; may exceed 86400
    pub arrival: u32,
    pub departure: u32,
    pub pickup: bool,
    pub drop_off: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calendar {
    /// Monday..Sunday
    pub days: [bool; 7],
    pub start: NaiveDate,
    pub end: NaiveDate,
}

#[derive(Debug, Clone, Copy)]
pub struct Frequency {
    pub trip: u32,
    pub start: u32,
    pub end: u32,
    pub headway: u32,
    pub exact: bool,
}

/// Service ids plus the rules that say which of them run on a given date.
/// Kept separate from the feed so it can be carried into the built bundle.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ServiceCalendar {
    pub ids: Vec<String>,
    pub calendar: HashMap<u32, Calendar>,
    /// service -> (date, added?)
    pub calendar_dates: HashMap<u32, Vec<(NaiveDate, bool)>>,
}

impl ServiceCalendar {
    /// Which services run on `date`, as a bitmap indexed like `self.ids`.
    pub fn active(&self, date: NaiveDate) -> Vec<bool> {
        let weekday = date.weekday().num_days_from_monday() as usize;
        (0..self.ids.len() as u32)
            .map(|s| {
                let mut active = match self.calendar.get(&s) {
                    Some(c) => c.days[weekday] && date >= c.start && date <= c.end,
                    None => false,
                };
                if let Some(exceptions) = self.calendar_dates.get(&s) {
                    for &(d, added) in exceptions {
                        if d == date {
                            active = added;
                        }
                    }
                }
                active
            })
            .collect()
    }

    /// First and last date on which any service runs.
    pub fn date_range(&self) -> Option<(NaiveDate, NaiveDate)> {
        let mut lo: Option<NaiveDate> = None;
        let mut hi: Option<NaiveDate> = None;
        let mut push = |d: NaiveDate| {
            lo = Some(lo.map_or(d, |x| x.min(d)));
            hi = Some(hi.map_or(d, |x| x.max(d)));
        };
        for c in self.calendar.values() {
            push(c.start);
            push(c.end);
        }
        for v in self.calendar_dates.values() {
            for &(d, added) in v {
                if added {
                    push(d);
                }
            }
        }
        Some((lo?, hi?))
    }
}

#[derive(Debug, Default)]
pub struct Feed {
    pub stops: Vec<Stop>,
    pub routes: Vec<Route>,
    pub trips: Vec<Trip>,
    /// sorted by (trip, seq)
    pub stop_times: Vec<StopTime>,
    pub services: ServiceCalendar,
    pub frequencies: Vec<Frequency>,
}

/// Parse "H:MM:SS" (hours may exceed 23) into seconds. Empty -> None.
pub fn parse_time(s: &str) -> Result<Option<u32>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let mut parts = s.split(':');
    let h: u32 = parts.next().unwrap_or("").parse().with_context(|| format!("bad time {s:?}"))?;
    let m: u32 = parts.next().unwrap_or("0").parse().with_context(|| format!("bad time {s:?}"))?;
    let sec: u32 = parts.next().unwrap_or("0").parse().with_context(|| format!("bad time {s:?}"))?;
    Ok(Some(h * 3600 + m * 60 + sec))
}

pub fn format_time(secs: u32) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y%m%d").with_context(|| format!("bad date {s:?}"))
}

enum Source {
    Dir(PathBuf),
    Zip(zip::ZipArchive<File>),
}

impl Source {
    fn open(path: &Path) -> Result<Source> {
        if path.is_dir() {
            Ok(Source::Dir(path.to_path_buf()))
        } else {
            let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
            Ok(Source::Zip(zip::ZipArchive::new(f)?))
        }
    }

    /// Returns None if the file does not exist in the feed.
    fn file(&mut self, name: &str) -> Result<Option<Box<dyn Read + '_>>> {
        match self {
            Source::Dir(dir) => {
                let p = dir.join(name);
                if !p.exists() {
                    return Ok(None);
                }
                Ok(Some(Box::new(File::open(p)?)))
            }
            Source::Zip(archive) => {
                // Some agencies zip a folder, so the entry may be "folder/stops.txt".
                let found = archive
                    .file_names()
                    .find(|n| *n == name || n.ends_with(&format!("/{name}")))
                    .map(str::to_owned);
                match found {
                    None => Ok(None),
                    Some(n) => Ok(Some(Box::new(archive.by_name(&n)?))),
                }
            }
        }
    }
}

fn reader<'a>(src: &'a mut Source, name: &str) -> Result<Option<csv::Reader<Box<dyn Read + 'a>>>> {
    Ok(src.file(name)?.map(|r| {
        csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .flexible(true)
            .from_reader(r)
    }))
}

fn required<'a>(src: &'a mut Source, name: &str) -> Result<csv::Reader<Box<dyn Read + 'a>>> {
    reader(src, name)?.with_context(|| format!("feed is missing required file {name}"))
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

#[derive(Deserialize)]
struct StopRow {
    stop_id: String,
    #[serde(default)]
    stop_name: String,
    #[serde(default)]
    stop_lat: Option<f64>,
    #[serde(default)]
    stop_lon: Option<f64>,
    #[serde(default)]
    location_type: Option<u8>,
    #[serde(default)]
    parent_station: Option<String>,
}

#[derive(Deserialize)]
struct RouteRow {
    route_id: String,
    #[serde(default)]
    route_short_name: String,
    #[serde(default)]
    route_long_name: String,
    #[serde(default)]
    route_type: Option<u16>,
    #[serde(default)]
    route_color: Option<String>,
}

#[derive(Deserialize)]
struct TripRow {
    trip_id: String,
    route_id: String,
    service_id: String,
    #[serde(default)]
    trip_headsign: String,
    #[serde(default)]
    direction_id: Option<u8>,
}

#[derive(Deserialize)]
struct StopTimeRow {
    trip_id: String,
    #[serde(default)]
    arrival_time: String,
    #[serde(default)]
    departure_time: String,
    stop_id: String,
    stop_sequence: u32,
    #[serde(default)]
    pickup_type: Option<u8>,
    #[serde(default)]
    drop_off_type: Option<u8>,
}

#[derive(Deserialize)]
struct CalendarRow {
    service_id: String,
    monday: u8,
    tuesday: u8,
    wednesday: u8,
    thursday: u8,
    friday: u8,
    saturday: u8,
    sunday: u8,
    start_date: String,
    end_date: String,
}

#[derive(Deserialize)]
struct CalendarDateRow {
    service_id: String,
    date: String,
    exception_type: u8,
}

#[derive(Deserialize)]
struct FrequencyRow {
    trip_id: String,
    start_time: String,
    end_time: String,
    headway_secs: u32,
    #[serde(default)]
    exact_times: Option<u8>,
}

impl Feed {
    pub fn read(path: &Path) -> Result<Feed> {
        let mut src = Source::open(path)?;
        let mut feed = Feed::default();

        // stops
        let mut stop_index: HashMap<String, u32> = HashMap::new();
        for row in required(&mut src, "stops.txt")?.deserialize::<StopRow>() {
            let row = row.context("stops.txt")?;
            let (Some(lat), Some(lon)) = (row.stop_lat, row.stop_lon) else {
                continue; // entrances / generic nodes without coordinates
            };
            stop_index.insert(row.stop_id.clone(), feed.stops.len() as u32);
            feed.stops.push(Stop {
                id: row.stop_id,
                name: row.stop_name,
                lat,
                lon,
                location_type: row.location_type.unwrap_or(0),
                parent_station: non_empty(row.parent_station),
            });
        }

        // routes
        let mut route_index: HashMap<String, u32> = HashMap::new();
        for row in required(&mut src, "routes.txt")?.deserialize::<RouteRow>() {
            let row = row.context("routes.txt")?;
            route_index.insert(row.route_id.clone(), feed.routes.len() as u32);
            feed.routes.push(Route {
                id: row.route_id,
                short_name: row.route_short_name,
                long_name: row.route_long_name,
                route_type: row.route_type.unwrap_or(3),
                color: non_empty(row.route_color),
            });
        }

        // services: calendar.txt and calendar_dates.txt are each optional, but
        // at least one must exist.
        let mut service_index: HashMap<String, u32> = HashMap::new();
        let mut intern_service = |feed: &mut Feed, id: &str| -> u32 {
            *service_index.entry(id.to_owned()).or_insert_with(|| {
                feed.services.ids.push(id.to_owned());
                (feed.services.ids.len() - 1) as u32
            })
        };
        let mut saw_calendar = false;
        if let Some(mut r) = reader(&mut src, "calendar.txt")? {
            for row in r.deserialize::<CalendarRow>() {
                let row = row.context("calendar.txt")?;
                saw_calendar = true;
                let s = intern_service(&mut feed, &row.service_id);
                feed.services.calendar.insert(
                    s,
                    Calendar {
                        days: [
                            row.monday == 1,
                            row.tuesday == 1,
                            row.wednesday == 1,
                            row.thursday == 1,
                            row.friday == 1,
                            row.saturday == 1,
                            row.sunday == 1,
                        ],
                        start: parse_date(&row.start_date)?,
                        end: parse_date(&row.end_date)?,
                    },
                );
            }
        }
        if let Some(mut r) = reader(&mut src, "calendar_dates.txt")? {
            for row in r.deserialize::<CalendarDateRow>() {
                let row = row.context("calendar_dates.txt")?;
                saw_calendar = true;
                let s = intern_service(&mut feed, &row.service_id);
                feed.services.calendar_dates
                    .entry(s)
                    .or_default()
                    .push((parse_date(&row.date)?, row.exception_type == 1));
            }
        }
        if !saw_calendar {
            bail!("feed has neither calendar.txt nor calendar_dates.txt");
        }

        // trips
        let mut trip_index: HashMap<String, u32> = HashMap::new();
        let mut unknown_route = 0usize;
        for row in required(&mut src, "trips.txt")?.deserialize::<TripRow>() {
            let row = row.context("trips.txt")?;
            let Some(&route) = route_index.get(&row.route_id) else {
                unknown_route += 1;
                continue;
            };
            let service = intern_service(&mut feed, &row.service_id);
            trip_index.insert(row.trip_id.clone(), feed.trips.len() as u32);
            feed.trips.push(Trip {
                id: row.trip_id,
                route,
                service,
                headsign: row.trip_headsign,
                direction: row.direction_id,
            });
        }
        if unknown_route > 0 {
            log::warn!("{unknown_route} trips reference unknown routes; skipped");
        }

        // stop_times (the big one)
        let mut unknown = 0usize;
        let mut raw: Vec<(StopTime, Option<u32>, Option<u32>)> = Vec::new();
        for row in required(&mut src, "stop_times.txt")?.deserialize::<StopTimeRow>() {
            let row = row.context("stop_times.txt")?;
            let (Some(&trip), Some(&stop)) = (trip_index.get(&row.trip_id), stop_index.get(&row.stop_id)) else {
                unknown += 1;
                continue;
            };
            let arr = parse_time(&row.arrival_time)?;
            let dep = parse_time(&row.departure_time)?;
            raw.push((
                StopTime {
                    trip,
                    stop,
                    seq: row.stop_sequence,
                    arrival: 0,
                    departure: 0,
                    pickup: row.pickup_type.unwrap_or(0) != 1,
                    drop_off: row.drop_off_type.unwrap_or(0) != 1,
                },
                arr.or(dep),
                dep.or(arr),
            ));
        }
        if unknown > 0 {
            log::warn!("{unknown} stop_times reference unknown trips or stops; skipped");
        }
        raw.sort_by_key(|(st, _, _)| (st.trip, st.seq));
        feed.stop_times = interpolate_times(raw);

        // frequencies (optional)
        if let Some(mut r) = reader(&mut src, "frequencies.txt")? {
            for row in r.deserialize::<FrequencyRow>() {
                let row = row.context("frequencies.txt")?;
                let Some(&trip) = trip_index.get(&row.trip_id) else { continue };
                let (Some(start), Some(end)) = (parse_time(&row.start_time)?, parse_time(&row.end_time)?) else {
                    continue;
                };
                feed.frequencies.push(Frequency {
                    trip,
                    start,
                    end,
                    headway: row.headway_secs,
                    exact: row.exact_times.unwrap_or(0) == 1,
                });
            }
        }

        Ok(feed)
    }

    /// Stop times of one trip, in sequence order.
    pub fn trip_stop_times(&self, trip: u32) -> &[StopTime] {
        // stop_times are sorted by trip, so binary search for the span
        let lo = self.stop_times.partition_point(|st| st.trip < trip);
        let hi = self.stop_times.partition_point(|st| st.trip <= trip);
        &self.stop_times[lo..hi]
    }
}

/// Fill in missing (non-timepoint) arrival/departure times by linear
/// interpolation between the nearest known times of the same trip.
fn interpolate_times(raw: Vec<(StopTime, Option<u32>, Option<u32>)>) -> Vec<StopTime> {
    let mut out: Vec<StopTime> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let trip = raw[i].0.trip;
        let mut j = i;
        while j < raw.len() && raw[j].0.trip == trip {
            j += 1;
        }
        let group = &raw[i..j];
        let known: Vec<(usize, u32, u32)> = group
            .iter()
            .enumerate()
            .filter_map(|(k, (_, a, d))| Some((k, (*a)?, (*d)?)))
            .collect();
        if known.is_empty() {
            log::warn!("trip {trip} has no times at all; dropped");
            i = j;
            continue;
        }
        for (k, (st, _, _)) in group.iter().enumerate() {
            let mut st = *st;
            // find bracketing known times
            let after = known.partition_point(|(kk, _, _)| *kk < k);
            if after < known.len() && known[after].0 == k {
                st.arrival = known[after].1;
                st.departure = known[after].2;
            } else if after == 0 {
                st.arrival = known[0].1;
                st.departure = known[0].1;
            } else if after == known.len() {
                let last = known[known.len() - 1];
                st.arrival = last.2;
                st.departure = last.2;
            } else {
                let (k0, _, d0) = known[after - 1];
                let (k1, a1, _) = known[after];
                let frac = (k - k0) as f64 / (k1 - k0) as f64;
                let t = d0 as f64 + frac * (a1 as f64 - d0 as f64);
                st.arrival = t.round() as u32;
                st.departure = st.arrival;
            }
            out.push(st);
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times_past_midnight() {
        assert_eq!(parse_time("08:05:00").unwrap(), Some(29100));
        assert_eq!(parse_time("27:15:00").unwrap(), Some(98100));
        assert_eq!(parse_time("").unwrap(), None);
        assert_eq!(format_time(98100), "27:15:00");
    }

    #[test]
    fn interpolates_missing_times() {
        let st = |seq| StopTime { trip: 0, stop: seq, seq, arrival: 0, departure: 0, pickup: true, drop_off: true };
        let raw = vec![
            (st(0), Some(100), Some(100)),
            (st(1), None, None),
            (st(2), None, None),
            (st(3), Some(400), Some(400)),
        ];
        let out = interpolate_times(raw);
        assert_eq!(out.iter().map(|s| s.arrival).collect::<Vec<_>>(), vec![100, 200, 300, 400]);
    }
}
