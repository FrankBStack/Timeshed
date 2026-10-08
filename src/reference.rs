//! A deliberately naive earliest-arrival router, used only to check RAPTOR.
//!
//! Plain Dijkstra over stops on the raw feed: settling a stop at time t
//! relaxes every footpath out of it and boards every trip that departs it at
//! or after t, alighting at every later stop. No route grouping, no rounds,
//! no pruning beyond the time budget. Slow, and obviously right.

use crate::gtfs::Feed;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const UNREACHED: u32 = u32::MAX;

/// One expanded trip: (service, events in sequence order).
struct TripInstance {
    service: u32,
    stops: Vec<u32>,
    arr: Vec<u32>,
    dep: Vec<u32>,
    board: Vec<bool>,
    alight: Vec<bool>,
}

pub struct Reference {
    stop_count: usize,
    trips: Vec<TripInstance>,
    /// for each stop: (trip instance, position) pairs that visit it
    by_stop: Vec<Vec<(u32, u32)>>,
    /// footpaths in feed stop indices: (to, meters)
    transfers: Vec<Vec<(u32, f32)>>,
}

impl Reference {
    pub fn new(feed: &Feed, transfers: Vec<Vec<(u32, f32)>>) -> Reference {
        let mut trips = Vec::new();
        for (t, trip) in feed.trips.iter().enumerate() {
            let sts = feed.trip_stop_times(t as u32);
            if sts.len() < 2 {
                continue;
            }
            let make = |offset: i64| TripInstance {
                service: trip.service,
                stops: sts.iter().map(|s| s.stop).collect(),
                arr: sts.iter().map(|s| (s.arrival as i64 + offset).max(0) as u32).collect(),
                dep: sts.iter().map(|s| (s.departure as i64 + offset).max(0) as u32).collect(),
                board: sts.iter().map(|s| s.pickup).collect(),
                alight: sts.iter().map(|s| s.drop_off).collect(),
            };
            let freqs: Vec<_> = feed.frequencies.iter().filter(|f| f.trip == t as u32).collect();
            if freqs.is_empty() {
                trips.push(make(0));
            } else {
                for f in freqs {
                    let mut start = f.start;
                    while start < f.end {
                        trips.push(make(start as i64 - sts[0].departure as i64));
                        start += f.headway.max(1);
                    }
                }
            }
        }
        let mut by_stop = vec![Vec::new(); feed.stops.len()];
        for (i, trip) in trips.iter().enumerate() {
            for (pos, &s) in trip.stops.iter().enumerate() {
                by_stop[s as usize].push((i as u32, pos as u32));
            }
        }
        Reference { stop_count: feed.stops.len(), trips, by_stop, transfers }
    }

    /// Earliest arrival at every feed stop, or UNREACHED.
    pub fn run(&self, active: &[bool], sources: &[(u32, u32)], walk_pace: f64, board_slack: u32, limit: u32) -> Vec<u32> {
        let mut best = vec![UNREACHED; self.stop_count];
        let mut done = vec![false; self.stop_count];
        let mut heap = BinaryHeap::new();
        for &(s, t) in sources {
            if t <= limit && t < best[s as usize] {
                best[s as usize] = t;
                heap.push(Reverse((t, s)));
            }
        }
        while let Some(Reverse((t, s))) = heap.pop() {
            if done[s as usize] || t > best[s as usize] {
                continue;
            }
            done[s as usize] = true;
            let mut relax = |stop: u32, time: u32, heap: &mut BinaryHeap<Reverse<(u32, u32)>>| {
                if time <= limit && time < best[stop as usize] {
                    best[stop as usize] = time;
                    heap.push(Reverse((time, stop)));
                }
            };
            for &(to, meters) in &self.transfers[s as usize] {
                relax(to, t + (meters as f64 * walk_pace).round() as u32, &mut heap);
            }
            for &(ti, pos) in &self.by_stop[s as usize] {
                let trip = &self.trips[ti as usize];
                let pos = pos as usize;
                if !active[trip.service as usize] || !trip.board[pos] || trip.dep[pos] < t + board_slack {
                    continue;
                }
                for j in pos + 1..trip.stops.len() {
                    if trip.alight[j] {
                        relax(trip.stops[j], trip.arr[j], &mut heap);
                    }
                }
            }
        }
        best
    }
}

/// Everything needed to compare RAPTOR against the reference on one feed:
/// the two routers plus the mapping between their stop numbering.
pub struct Harness<'a> {
    pub feed: &'a Feed,
    pub tt: &'a crate::timetable::Timetable,
    pub reference: Reference,
    /// feed stop index for each timetable stop
    pub feed_stop: Vec<u32>,
}

impl<'a> Harness<'a> {
    pub fn new(feed: &'a Feed, tt: &'a crate::timetable::Timetable) -> Harness<'a> {
        let index: std::collections::HashMap<&str, u32> =
            feed.stops.iter().enumerate().map(|(i, s)| (s.id.as_str(), i as u32)).collect();
        let feed_stop: Vec<u32> = tt.stops.iter().map(|s| index[s.gtfs_id.as_str()]).collect();
        let mut transfers = vec![Vec::new(); feed.stops.len()];
        for (s, list) in tt.transfers.iter().enumerate() {
            transfers[feed_stop[s] as usize] = list.iter().map(|&(to, m)| (feed_stop[to as usize], m)).collect();
        }
        Harness { feed, tt, reference: Reference::new(feed, transfers), feed_stop }
    }

    /// Run both routers from `sources` (timetable stop indices) and return
    /// the timetable stops where they disagree, as (stop, raptor, reference).
    pub fn compare(
        &self,
        raptor: &mut crate::raptor::Raptor,
        active: &[bool],
        sources: &[(u32, u32)],
        max_rounds: usize,
        walk_pace: f64,
        board_slack: u32,
        limit: u32,
    ) -> Vec<(u32, u32, u32)> {
        raptor.run(self.tt, active, sources.iter().copied(), max_rounds, walk_pace, board_slack, limit);
        let feed_sources: Vec<(u32, u32)> = sources.iter().map(|&(s, t)| (self.feed_stop[s as usize], t)).collect();
        let truth = self.reference.run(active, &feed_sources, walk_pace, board_slack, limit);
        (0..self.tt.stops.len() as u32)
            .filter_map(|s| {
                let a = raptor.arrival(s).unwrap_or(crate::raptor::UNREACHED);
                let b = truth[self.feed_stop[s as usize] as usize];
                (a != b).then_some((s, a, b))
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests_helpers {
    use super::*;
    use crate::gtfs::{Frequency, Route, Stop, StopTime, Trip};
    use rand::prelude::*;

    /// A random little network: stops on a grid, routes that wander across
    /// it, trips at irregular times, some short-turns, some frequency-based
    /// trips, some no-pickup stops, footpaths between near stops.
    pub fn random_feed(rng: &mut StdRng) -> Feed {
        let mut feed = Feed::default();
        let n_stops = rng.random_range(8..40);
        for i in 0..n_stops {
            feed.stops.push(Stop {
                id: format!("s{i}"),
                name: String::new(),
                lat: 43.0 + rng.random_range(0..6) as f64 * 0.004,
                lon: -87.9 + rng.random_range(0..6) as f64 * 0.004,
                location_type: 0,
                parent_station: None,
            });
        }
        feed.services.ids = vec!["a".into(), "b".into()];
        let n_routes = rng.random_range(2..8);
        for r in 0..n_routes {
            feed.routes.push(Route { id: format!("r{r}"), short_name: String::new(), long_name: String::new(), route_type: 3, color: None });
            let len = rng.random_range(2..8usize);
            let path: Vec<u32> = (0..len).map(|_| rng.random_range(0..n_stops) as u32).collect();
            let n_trips = rng.random_range(1..12);
            for k in 0..n_trips {
                let trip = feed.trips.len() as u32;
                let short_turn = rng.random_bool(0.2);
                let use_len = if short_turn { rng.random_range(2..=len) } else { len };
                feed.trips.push(Trip {
                    id: format!("r{r}t{k}"),
                    route: r as u32,
                    service: rng.random_range(0..2),
                    headsign: String::new(),
                    direction: None,
                });
                let mut t = 6 * 3600 + rng.random_range(0..12 * 3600);
                for (seq, &stop) in path[..use_len].iter().enumerate() {
                    let dwell = if rng.random_bool(0.3) { rng.random_range(0..120) } else { 0 };
                    feed.stop_times.push(StopTime {
                        trip,
                        stop,
                        seq: seq as u32,
                        arrival: t,
                        departure: t + dwell,
                        pickup: !rng.random_bool(0.1),
                        drop_off: !rng.random_bool(0.1),
                    });
                    t += dwell + rng.random_range(60..900);
                }
                if rng.random_bool(0.15) {
                    let start = 7 * 3600 + rng.random_range(0..3600);
                    feed.frequencies.push(Frequency { trip, start, end: start + rng.random_range(600..7200), headway: rng.random_range(300..1800), exact: true });
                }
            }
        }
        feed.stop_times.sort_by_key(|st| (st.trip, st.seq));
        feed
    }

    pub fn random_transfers(rng: &mut StdRng, n: usize) -> Vec<Vec<(u32, f32)>> {
        let mut t = vec![Vec::new(); n];
        for s in 0..n {
            for _ in 0..rng.random_range(0..4) {
                let to = rng.random_range(0..n);
                if to != s {
                    t[s].push((to as u32, rng.random_range(50.0..900.0)));
                }
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::tests_helpers::*;
    use super::*;
    use crate::raptor::Raptor;
    use crate::timetable::Timetable;
    use rand::prelude::*;

    #[test]
    fn raptor_matches_brute_force_on_random_feeds() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut mismatches = 0;
        let mut compared = 0;
        for _ in 0..200 {
            let feed = random_feed(&mut rng);
            let mut tt = Timetable::from_feed(&feed);
            // footpaths in timetable numbering, which is what the engine does
            tt.transfers = random_transfers(&mut rng, tt.stops.len());
            let harness = Harness::new(&feed, &tt);
            let mut raptor = Raptor::new(&tt);
            for _ in 0..50 {
                let active = [rng.random_bool(0.8), rng.random_bool(0.5)];
                let n_src = rng.random_range(1..3);
                let sources: Vec<(u32, u32)> = (0..n_src)
                    .map(|_| (rng.random_range(0..tt.stops.len()) as u32, 6 * 3600 + rng.random_range(0..13 * 3600)))
                    .collect();
                let start = sources.iter().map(|s| s.1).min().unwrap();
                let limit = start + rng.random_range(600..4 * 3600);
                let slack = [0, 0, 30, 60][rng.random_range(0..4)];
                let bad = harness.compare(&mut raptor, &active, &sources, 100, 1.0 / 1.3, slack, limit);
                compared += tt.stops.len();
                if !bad.is_empty() {
                    mismatches += bad.len();
                    eprintln!("sources {sources:?} active {active:?} slack {slack} limit {limit}: {:?}", &bad[..bad.len().min(5)]);
                }
            }
        }
        assert_eq!(mismatches, 0, "{mismatches} of {compared} stop labels differ from brute force");
        assert!(compared > 100_000);
    }

    #[test]
    fn harness_round_cap_only_removes_journeys() {
        // With few rounds RAPTOR may miss journeys, but it must never invent one.
        let mut rng = StdRng::seed_from_u64(11);
        let feed = random_feed(&mut rng);
        let mut tt = Timetable::from_feed(&feed);
        tt.transfers = random_transfers(&mut rng, tt.stops.len());
        let harness = Harness::new(&feed, &tt);
        let mut raptor = Raptor::new(&tt);
        for _ in 0..100 {
            let sources = vec![(rng.random_range(0..tt.stops.len()) as u32, 7 * 3600 + rng.random_range(0..7200))];
            for (_, a, b) in harness.compare(&mut raptor, &[true, true], &sources, 1, 1.0 / 1.3, 0, u32::MAX) {
                assert!(a >= b, "one round found {a}, brute force {b}");
            }
        }
    }
}

#[cfg(test)]
mod debug_tests {
    use super::tests_helpers::*;
    use super::*;
    use crate::raptor::Raptor;
    use crate::timetable::Timetable;
    use rand::prelude::*;

    #[test]
    #[ignore]
    fn dump_first_mismatch() {
        let mut rng = StdRng::seed_from_u64(7);
        for feed_no in 0..60 {
            let feed = random_feed(&mut rng);
            let mut tt = Timetable::from_feed(&feed);
            tt.transfers = random_transfers(&mut rng, tt.stops.len());
            let harness = Harness::new(&feed, &tt);
            let mut raptor = Raptor::new(&tt);
            for q in 0..40 {
                let active = [rng.random_bool(0.8), rng.random_bool(0.5)];
                let n_src = rng.random_range(1..3);
                let sources: Vec<(u32, u32)> = (0..n_src)
                    .map(|_| (rng.random_range(0..tt.stops.len()) as u32, 6 * 3600 + rng.random_range(0..13 * 3600)))
                    .collect();
                let start = sources.iter().map(|s| s.1).min().unwrap();
                let limit = start + rng.random_range(600..4 * 3600);
                let bad = harness.compare(&mut raptor, &active, &sources, 100, 1.0 / 1.3, 0, limit);
                if !bad.is_empty() {
                    println!("feed {feed_no} query {q}: sources {sources:?} active {active:?} limit {limit}");
                    println!("mismatches (tt stop, raptor, brute): {bad:?}");
                    println!("tt stop -> feed stop: {:?}", harness.feed_stop);
                    for (r, route) in tt.routes.iter().enumerate() {
                        println!("route {r} (gtfs {}) stops {:?}", tt.route_info[route.info as usize].gtfs_id, route.stops);
                        for (k, trip) in route.trips.iter().enumerate() {
                            let evs: Vec<String> = (0..route.stops.len())
                                .map(|i| {
                                    let e = route.event(k, i);
                                    format!("{}{}/{}{}", if e.board { "" } else { "!b" }, e.arr, e.dep, if e.alight { "" } else { "!a" })
                                })
                                .collect();
                            println!("   trip {} svc {} : {}", tt.trip_ids[trip.gtfs_trip as usize], trip.service, evs.join("  "));
                        }
                    }
                    for (s, list) in tt.transfers.iter().enumerate() {
                        if !list.is_empty() {
                            println!("transfers from {s}: {list:?}");
                        }
                    }
                    for s in 0..tt.stops.len() as u32 {
                        println!("  stop {s}: raptor {:?} legs {}", raptor.arrival(s), raptor.legs(s));
                    }
                    return;
                }
            }
        }
    }
}
