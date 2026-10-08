//! The routing bundle: timetable + walking graph + the glue between them,
//! and the door-to-door query that runs on it.

use crate::raptor::Raptor;
use crate::timetable::Timetable;
use crate::walk::{WalkGraph, WalkIndex, WalkSearch};
use anyhow::Result;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
        let f = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        bincode::serialize_into(std::io::BufWriter::new(f), self)?;
        Ok(())
    }

    #[cfg(feature = "native")]
    pub fn load(path: &std::path::Path) -> Result<Engine> {
        use anyhow::Context;
        let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let e: Engine = bincode::deserialize_from(std::io::BufReader::new(f))?;
        Ok(e.finish_load())
    }

    /// Deserialize a bundle held in memory (the browser build's only way in).
    pub fn from_bytes(bytes: &[u8]) -> Result<Engine> {
        let e: Engine = bincode::deserialize(bytes)?;
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

/// A reusable query workspace. One per thread.
pub struct Query<'a> {
    pub engine: &'a Engine,
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
        self.walk.run([(origin, start)], limit, pace);
        let mut sources: Vec<(u32, u32)> = Vec::new();
        for (node, t) in self.walk.reached() {
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

    pub fn origin_node(&self) -> Option<u32> {
        self.origin
    }

    pub fn reached_node_count(&self) -> usize {
        self.walk.reached_count()
    }
}

