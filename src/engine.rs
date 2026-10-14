//! The routing bundle: timetable + walking graph + the glue between them,
//! and the door-to-door query that runs on it.

use crate::raptor::{Parent, Raptor};
use crate::timetable::Timetable;
use crate::walk::{WalkGraph, WalkIndex, WalkSearch};
use anyhow::Result;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bundle file header: magic bytes, then a little-endian format version.
/// Bump the version whenever a serialized struct changes shape.
pub const BUNDLE_MAGIC: &[u8; 4] = b"TSHD";
pub const BUNDLE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BuildOpts {
    /// how far a stop may be from the nearest walkable node
    pub snap_max_m: f64,
    /// longest footpath considered between two stops
    pub transfer_max_m: f64,
}

impl Default for BuildOpts {
    fn default() -> Self {
        BuildOpts { snap_max_m: 300.0, transfer_max_m: 1000.0 }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Engine {
    pub name: String,
    pub tt: Timetable,
    pub walk: WalkGraph,
    /// walking node for each stop, or u32::MAX if none was close enough
    pub stop_node: Vec<u32>,
    /// straight-line meters from the stop to its node
    pub stop_snap_m: Vec<f32>,
    pub build_opts: BuildOpts,
    #[serde(skip)]
    pub index: Option<WalkIndex>,
    #[serde(skip)]
    pub node_stops: HashMap<u32, Vec<u32>>,
}

impl Engine {
    pub fn build(name: String, mut tt: Timetable, walk: WalkGraph, opts: BuildOpts) -> Engine {
        let index = WalkIndex::build(&walk);
        let mut stop_node = vec![u32::MAX; tt.stops.len()];
        let mut stop_snap_m = vec![0f32; tt.stops.len()];
        let mut unsnapped = 0;
        for (i, s) in tt.stops.iter().enumerate() {
            match index.nearest(s.lat, s.lon, opts.snap_max_m) {
                Some((n, d)) => {
                    stop_node[i] = n;
                    stop_snap_m[i] = d as f32;
                }
                None => unsnapped += 1,
            }
        }
        if unsnapped > 0 {
            log::warn!("{unsnapped} stops have no walkable node within {} m", opts.snap_max_m);
        }
        let node_stops = reverse_map(&stop_node);

        // Footpaths: from each stop, walk up to transfer_max_m and record
        // every other stop found on the way.
        let t0 = std::time::Instant::now();
        let mut search = WalkSearch::new(&walk);
        let mut count = 0usize;
        for s in 0..tt.stops.len() {
            let n = stop_node[s];
            if n == u32::MAX {
                continue;
            }
            search.run([(n, stop_snap_m[s].round() as u32)], opts.transfer_max_m as u32, 1.0);
            let mut out = Vec::new();
            for (node, meters) in search.reached() {
                if let Some(stops) = node_stops.get(&node) {
                    for &t in stops {
                        if t as usize != s {
                            out.push((t, meters as f32 + stop_snap_m[t as usize]));
                        }
                    }
                }
            }
            out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            out.dedup_by_key(|x| x.0);
            count += out.len();
            tt.transfers[s] = out;
        }
        log::info!("{count} footpaths between stops ({:.1?})", t0.elapsed());

        Engine { name, tt, walk, stop_node, stop_snap_m, build_opts: opts, index: Some(index), node_stops }
    }

    #[cfg(feature = "native")]
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        use anyhow::Context;
        std::fs::write(path, self.to_bytes()?).with_context(|| format!("writing {}", path.display()))
    }

    #[cfg(feature = "native")]
    pub fn load(path: &std::path::Path) -> Result<Engine> {
        use anyhow::Context;
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Engine::from_bytes(&bytes)
    }

    /// The bundle is a short header (magic, format version) followed by the
    /// bincode payload, so a mismatch is reported in words rather than as a
    /// failed deserialization.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(BUNDLE_MAGIC.len() + 4);
        out.extend_from_slice(BUNDLE_MAGIC);
        out.extend_from_slice(&BUNDLE_VERSION.to_le_bytes());
        bincode::serialize_into(&mut out, self)?;
        Ok(out)
    }

    /// Deserialize a bundle held in memory (the browser build's only way in).
    pub fn from_bytes(bytes: &[u8]) -> Result<Engine> {
        if bytes.len() < 8 || &bytes[..4] != BUNDLE_MAGIC {
            anyhow::bail!("not a Timeshed bundle (missing header); rebuild it with `timeshed build`");
        }
        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if version != BUNDLE_VERSION {
            anyhow::bail!(
                "bundle is format version {version}, this build reads version {BUNDLE_VERSION}; rebuild it with `timeshed build`"
            );
        }
        let e: Engine = bincode::deserialize(&bytes[8..])?;
        Ok(e.finish_load())
    }

    fn finish_load(mut self) -> Engine {
        self.index = Some(WalkIndex::build(&self.walk));
        self.node_stops = reverse_map(&self.stop_node);
        self
    }

    pub fn index(&self) -> &WalkIndex {
        self.index.as_ref().expect("index is built on load")
    }
}

fn reverse_map(stop_node: &[u32]) -> HashMap<u32, Vec<u32>> {
    let mut m: HashMap<u32, Vec<u32>> = HashMap::new();
    for (s, &n) in stop_node.iter().enumerate() {
        if n != u32::MAX {
            m.entry(n).or_default().push(s as u32);
        }
    }
    m
}

#[derive(Clone, Debug)]
pub struct QueryOpts {
    pub date: NaiveDate,
    /// departure, seconds after midnight
    pub depart: u32,
    /// travel time budget in seconds
    pub max_secs: u32,
    pub max_rounds: usize,
    pub walk_speed_mps: f64,
    /// seconds you must be at a stop before a departure to board it
    pub board_slack_secs: u32,
    /// how far the origin point may be from the walking network
    pub origin_snap_max_m: f64,
}

impl Default for QueryOpts {
    fn default() -> Self {
        QueryOpts {
            date: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            depart: 8 * 3600,
            max_secs: 45 * 60,
            max_rounds: 6,
            walk_speed_mps: 1.3,
            board_slack_secs: 0,
            origin_snap_max_m: 500.0,
        }
    }
}

/// One leg of a reconstructed journey. Coordinates are [lon, lat].
#[derive(Serialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Leg {
    Walk { secs: u32, meters: f32, path: Vec<[f64; 2]> },
    Ride {
        route: String,
        long_name: String,
        color: Option<String>,
        from: String,
        to: String,
        /// seconds after midnight of the service day
        depart: u32,
        arrive: u32,
        /// stops ridden past, i.e. alight index minus board index
        stops: u32,
        path: Vec<[f64; 2]>,
    },
}

#[derive(Serialize, Debug, Clone)]
pub struct Itinerary {
    pub depart: u32,
    pub arrive: u32,
    pub total_secs: u32,
    pub legs: Vec<Leg>,
}

/// A reusable query workspace. One per thread.
pub struct Query<'a> {
    pub engine: &'a Engine,
    /// the walk out from the origin that seeds the stops
    origin_walk: WalkSearch<'a>,
    /// the walk from every reached stop that labels the nodes
    walk: WalkSearch<'a>,
    raptor: Raptor,
    active: Option<(NaiveDate, Vec<bool>)>,
    depart: u32,
    pace: f64,
    origin: Option<u32>,
}

impl<'a> Query<'a> {
    pub fn new(engine: &'a Engine) -> Query<'a> {
        Query {
            engine,
            origin_walk: WalkSearch::new(&engine.walk),
            walk: WalkSearch::new(&engine.walk),
            raptor: Raptor::new(&engine.tt),
            active: None,
            depart: 0,
            pace: 1.0,
            origin: None,
        }
    }

    fn active_services(&mut self, date: NaiveDate) -> &[bool] {
        if self.active.as_ref().is_none_or(|(d, _)| *d != date) {
            self.active = Some((date, self.engine.tt.services.active(date)));
        }
        &self.active.as_ref().unwrap().1
    }

    /// Door-to-door one-to-all search from a point. Returns false if the
    /// point is too far from the walking network.
    pub fn run(&mut self, lat: f64, lon: f64, opts: &QueryOpts) -> bool {
        let e = self.engine;
        let Some((origin, snap_m)) = e.index().nearest(lat, lon, opts.origin_snap_max_m) else {
            self.origin = None;
            return false;
        };
        self.origin = Some(origin);
        self.depart = opts.depart;
        let pace = 1.0 / opts.walk_speed_mps;
        let start = opts.depart + (snap_m * pace).round() as u32;
        let limit = opts.depart + opts.max_secs;

        // 1. walk from the origin to every stop in reach
        self.origin_walk.run([(origin, start)], limit, pace);
        let mut sources: Vec<(u32, u32)> = Vec::new();
        for (node, t) in self.origin_walk.reached() {
            if let Some(stops) = e.node_stops.get(&node) {
                for &s in stops {
                    sources.push((s, t + (e.stop_snap_m[s as usize] as f64 * pace).round() as u32));
                }
            }
        }

        // 2. ride
        let active = self.active_services(opts.date).to_vec();
        self.raptor.run(&e.tt, &active, sources, opts.max_rounds, pace, opts.board_slack_secs, limit);

        // 3. walk from every reached stop (and the origin) to every node in reach
        let mut sources: Vec<(u32, u32)> = vec![(origin, start)];
        for (s, arr, _) in self.raptor.reached() {
            let n = e.stop_node[s as usize];
            if n != u32::MAX {
                sources.push((n, arr + (e.stop_snap_m[s as usize] as f64 * pace).round() as u32));
            }
        }
        self.walk.run(sources, limit, pace);
        self.pace = pace;
        true
    }

    /// Travel time in seconds to a walking node, after `run`.
    #[inline]
    pub fn node_secs(&self, node: u32) -> Option<u32> {
        self.walk.label(node).map(|t| t - self.depart)
    }

    /// (node, travel seconds) for every node reached by the last run.
    pub fn reached_nodes(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.walk.reached().map(move |(n, t)| (n, t - self.depart))
    }

    /// (stop, travel seconds, transit legs) for every stop reached by transit
    /// or by the initial walk.
    pub fn reached_stops(&self) -> impl Iterator<Item = (u32, u32, u8)> + '_ {
        self.raptor.reached().map(move |(s, t, legs)| (s, t - self.depart, legs))
    }

    /// Travel time to an arbitrary point: nearest node plus a straight-line
    /// walk from it. None if the point is off the network or out of reach.
    pub fn point_secs(&self, lat: f64, lon: f64, snap_max_m: f64) -> Option<u32> {
        let (node, d) = self.engine.index().nearest(lat, lon, snap_max_m)?;
        Some(self.node_secs(node)? + (d * self.pace).round() as u32)
    }

    /// Seconds per meter used by the last run.
    pub fn pace(&self) -> f64 {
        self.pace
    }

    /// The journey behind the last run's label at a point: walk, rides and
    /// transfers, walk. None if the point is off the network or unreached.
    pub fn itinerary(&self, lat: f64, lon: f64, snap_max_m: f64) -> Option<Itinerary> {
        let e = self.engine;
        let g = &e.walk;
        let pace = self.pace;
        let origin = self.origin?;
        let (end_node, end_m) = e.index().nearest(lat, lon, snap_max_m)?;
        let arrive_node = self.walk.label(end_node)?;
        let seed = self.walk.seed_of(end_node)?;
        let node_xy = |n: u32| { let (la, lo) = g.lat_lon(n); [lo, la] };
        let walk_leg = |path: Vec<u32>, extra_m: f64, tail: Option<[f64; 2]>| -> Leg {
            let mut meters = 0.0f64;
            for w in path.windows(2) {
                meters += g.neighbours(w[0]).find(|(m, _)| *m == w[1]).map(|(_, d)| d as f64).unwrap_or(0.0);
            }
            let mut coords: Vec<[f64; 2]> = path.iter().map(|&n| node_xy(n)).collect();
            if let Some(t) = tail { coords.push(t); }
            Leg::Walk { secs: ((meters + extra_m) * pace).round() as u32, meters: (meters + extra_m) as f32, path: coords }
        };
        let mut legs: Vec<Leg> = Vec::new();

        if seed == origin && self.walk.label(seed) == self.origin_walk.label(origin) {
            // walked the whole way
            legs.push(walk_leg(self.walk.path_to(end_node), end_m, Some([lon, lat])));
        } else {
            // which stop seeded the final walk? the one whose arrival matches the seed's label
            let last_stop = *e.node_stops.get(&seed)?.iter().min_by_key(|&&s| {
                self.raptor.arrival(s).map_or(u32::MAX, |a| a + (e.stop_snap_m[s as usize] as f64 * pace).round() as u32)
            })?;
            let chain = self.raptor.journey(last_stop);
            let (first_stop, _) = *chain.first()?;
            // walk from the origin to the first stop
            let first_node = e.stop_node[first_stop as usize];
            let st = &e.tt.stops[first_stop as usize];
            legs.push(walk_leg(self.origin_walk.path_to(first_node), e.stop_snap_m[first_stop as usize] as f64, Some([st.lon, st.lat])));
            for &(stop, parent) in &chain[1..] {
                match parent {
                    Parent::Source => {}
                    Parent::Trip { route, trip, from_stop } => {
                        let r = &e.tt.routes[route as usize];
                        let info = &e.tt.route_info[r.info as usize];
                        let i = r.stops.iter().position(|&x| x == from_stop)?;
                        let j = (i + 1..r.stops.len()).find(|&k| r.stops[k] == stop)?;
                        let path = r.stops[i..=j].iter().map(|&x| { let st = &e.tt.stops[x as usize]; [st.lon, st.lat] }).collect();
                        legs.push(Leg::Ride {
                            route: if info.short_name.is_empty() { info.long_name.clone() } else { info.short_name.clone() },
                            long_name: info.long_name.clone(),
                            color: info.color.clone(),
                            from: e.tt.stops[from_stop as usize].name.clone(),
                            to: e.tt.stops[stop as usize].name.clone(),
                            depart: r.event(trip as usize, i).dep,
                            arrive: r.event(trip as usize, j).arr,
                            stops: (j - i) as u32,
                            path,
                        });
                    }
                    Parent::Walk { from_stop } => {
                        let a = &e.tt.stops[from_stop as usize];
                        let b = &e.tt.stops[stop as usize];
                        let meters = e.tt.transfers[from_stop as usize].iter().find(|(to, _)| *to == stop).map_or(0.0, |(_, m)| *m);
                        legs.push(Leg::Walk { secs: (meters as f64 * pace).round() as u32, meters, path: vec![[a.lon, a.lat], [b.lon, b.lat]] });
                    }
                }
            }
            // walk from the last stop to the point
            let st = &e.tt.stops[last_stop as usize];
            let mut path = self.walk.path_to(end_node);
            let mut coords_prefix = vec![[st.lon, st.lat]];
            let leg = walk_leg(std::mem::take(&mut path), e.stop_snap_m[last_stop as usize] as f64 + end_m, Some([lon, lat]));
            if let Leg::Walk { secs, meters, path } = leg {
                coords_prefix.extend(path);
                legs.push(Leg::Walk { secs, meters, path: coords_prefix });
            }
        }
        let arrive = arrive_node + (end_m * pace).round() as u32;
        Some(Itinerary { depart: self.depart, arrive, total_secs: arrive - self.depart, legs })
    }

    pub fn origin_node(&self) -> Option<u32> {
        self.origin
    }

    pub fn reached_node_count(&self) -> usize {
        self.walk.reached_count()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::BBox;

    fn tiny_engine() -> Engine {
        let walk = WalkGraph { lat: vec![43.0], lon: vec![-87.9], offsets: vec![0, 0], targets: vec![], lengths: vec![], bbox: BBox::empty() };
        Engine::build("tiny".into(), Timetable::default(), walk, BuildOpts::default())
    }

    #[test]
    fn bundle_round_trips_with_a_header() {
        let bytes = tiny_engine().to_bytes().unwrap();
        assert_eq!(&bytes[..4], BUNDLE_MAGIC);
        let back = Engine::from_bytes(&bytes).unwrap();
        assert_eq!(back.name, "tiny");
        assert_eq!(back.walk.node_count(), 1);
    }

    #[test]
    fn wrong_magic_and_version_are_reported_in_words() {
        let mut bytes = tiny_engine().to_bytes().unwrap();
        bytes[4] = 99;
        let err = Engine::from_bytes(&bytes).err().expect("should fail").to_string();
        assert!(err.contains("format version 99") && err.contains("timeshed build"), "{err}");
        bytes[0] = b'X';
        let err = Engine::from_bytes(&bytes).err().expect("should fail").to_string();
        assert!(err.contains("not a Timeshed bundle"), "{err}");
    }
}
