//! RAPTOR: Round-bAsed Public Transit Optimized Router (Delling, Pajor,
//! Werneck 2012). One-to-all earliest arrival from a set of source stops.
//!
//! Round k finds the earliest arrival at every stop using at most k trips.
//! Each round scans only the routes that pass through stops improved in the
//! previous round, from the earliest such stop onwards, hopping onto the
//! earliest catchable trip and then relaxing footpaths.
//!
//! The paper assumes footpaths are transitively closed. Ours come from a
//! bounded walking search, so they are not: footpaths are relaxed with a
//! worklist until nothing improves, which makes chains of short walks legal
//! and the result independent of iteration order. The same relaxation runs
//! from the sources before the first round.

use crate::timetable::Timetable;

pub const UNREACHED: u32 = u32::MAX;

pub struct Raptor {
    /// earliest known arrival per stop
    best: Vec<u32>,
    /// arrival as of the end of the previous round; what boarding looks at
    prev: Vec<u32>,
    /// number of transit legs in the journey behind `best`
    legs: Vec<u8>,
    marked: Vec<bool>,
    marked_list: Vec<u32>,
    touched: Vec<u32>,
    /// earliest boarding position per route in the current round
    route_pos: Vec<u32>,
    queue: Vec<u32>,
    footpath_queue: Vec<u32>,
}

impl Raptor {
    pub fn new(tt: &Timetable) -> Raptor {
        Raptor {
            best: vec![UNREACHED; tt.stops.len()],
            prev: vec![UNREACHED; tt.stops.len()],
            legs: vec![0; tt.stops.len()],
            marked: vec![false; tt.stops.len()],
            marked_list: Vec::new(),
            touched: Vec::new(),
            route_pos: vec![u32::MAX; tt.routes.len()],
            queue: Vec::new(),
            footpath_queue: Vec::new(),
        }
    }

    /// Relax footpaths from every currently marked stop, and from any stop
    /// that improves as a result, until nothing changes.
    fn relax_footpaths(&mut self, tt: &Timetable, walk_pace: f64, limit: u32) {
        self.footpath_queue.clear();
        self.footpath_queue.extend_from_slice(&self.marked_list);
        while let Some(s) = self.footpath_queue.pop() {
            let from = self.best[s as usize];
            let legs = self.legs[s as usize];
            for &(to, meters) in &tt.transfers[s as usize] {
                let t = from + (meters as f64 * walk_pace).round() as u32;
                if t <= limit && t < self.best[to as usize] {
                    self.improve(to, t, legs);
                    self.footpath_queue.push(to);
                }
            }
        }
    }

    fn reset(&mut self) {
        for &s in &self.touched {
            self.best[s as usize] = UNREACHED;
            self.prev[s as usize] = UNREACHED;
            self.legs[s as usize] = 0;
            self.marked[s as usize] = false;
        }
        self.touched.clear();
        self.marked_list.clear();
    }

    #[inline]
    fn improve(&mut self, stop: u32, time: u32, legs: u8) {
        let s = stop as usize;
        if self.best[s] == UNREACHED {
            self.touched.push(stop);
        }
        self.best[s] = time;
        self.legs[s] = legs;
        if !self.marked[s] {
            self.marked[s] = true;
            self.marked_list.push(stop);
        }
    }

    /// `sources` are (stop, time already there). `walk_pace` is seconds per
    /// meter for footpaths. `board_slack` is how many seconds before a
    /// departure you must be at the stop to board it. No label above
    /// `limit` is kept.
    pub fn run(
        &mut self,
        tt: &Timetable,
        active: &[bool],
        sources: impl IntoIterator<Item = (u32, u32)>,
        max_rounds: usize,
        walk_pace: f64,
        board_slack: u32,
        limit: u32,
    ) {
        self.reset();
        for (s, t) in sources {
            if t <= limit && t < self.best[s as usize] {
                self.improve(s, t, 0);
            }
        }
        self.relax_footpaths(tt, walk_pace, limit);

        for round in 1..=max_rounds {
            // Labels from the previous round are what we can board with.
            for &s in &self.touched {
                self.prev[s as usize] = self.best[s as usize];
            }

            // Collect routes serving marked stops, with the earliest position.
            self.queue.clear();
            for &s in &self.marked_list {
                for &(r, pos) in &tt.stop_routes[s as usize] {
                    let cur = &mut self.route_pos[r as usize];
                    if *cur == u32::MAX {
                        self.queue.push(r);
                        *cur = pos;
                    } else if pos < *cur {
                        *cur = pos;
                    }
                }
                self.marked[s as usize] = false;
            }
            self.marked_list.clear();

            // Scan each route once.
            for qi in 0..self.queue.len() {
                let r = self.queue[qi];
                let route = &tt.routes[r as usize];
                let start = std::mem::replace(&mut self.route_pos[r as usize], u32::MAX) as usize;
                let n = route.stops.len();
                let mut trip: Option<usize> = None;
                for i in start..n {
                    let s = route.stops[i];
                    if let Some(t) = trip {
                        let ev = route.event(t, i);
                        if ev.alight && ev.arr < self.best[s as usize] && ev.arr <= limit {
                            self.improve(s, ev.arr, round as u8);
                        }
                    }
                    // Could we have boarded an earlier trip at this stop?
                    let ready = self.prev[s as usize];
                    if ready == UNREACHED {
                        continue;
                    }
                    let ready = ready + board_slack;
                    let can_board_earlier = match trip {
                        None => true,
                        Some(t) => ready <= route.event(t, i).dep,
                    };
                    if !can_board_earlier {
                        continue;
                    }
                    // Trips never overtake within a route, so departures at
                    // position i are sorted: binary search, then skip trips
                    // that don't run today or don't pick up here.
                    let mut t = partition_by_dep(route, i, ready);
                    let upper = trip.unwrap_or(route.trips.len());
                    while t < upper {
                        let ev = route.event(t, i);
                        if ev.board && active[route.trips[t].service as usize] {
                            break;
                        }
                        t += 1;
                    }
                    if t < upper {
                        trip = Some(t);
                    }
                }
            }

            // Footpaths from stops improved this round.
            self.relax_footpaths(tt, walk_pace, limit);

            if self.marked_list.is_empty() {
                break;
            }
        }
        for &s in &self.marked_list {
            self.marked[s as usize] = false;
        }
        self.marked_list.clear();
    }

    #[inline]
    pub fn arrival(&self, stop: u32) -> Option<u32> {
        let t = self.best[stop as usize];
        (t != UNREACHED).then_some(t)
    }

    #[inline]
    pub fn legs(&self, stop: u32) -> u8 {
        self.legs[stop as usize]
    }

    /// (stop, arrival, legs) for every reached stop.
    pub fn reached(&self) -> impl Iterator<Item = (u32, u32, u8)> + '_ {
        self.touched.iter().map(move |&s| (s, self.best[s as usize], self.legs[s as usize]))
    }
}

/// Index of the first trip whose departure at position `i` is >= `time`.
#[inline]
fn partition_by_dep(route: &crate::timetable::Route, i: usize, time: u32) -> usize {
    let n = route.stops.len();
    let (mut lo, mut hi) = (0usize, route.trips.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if route.events[mid * n + i].dep < time {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtfs::ServiceCalendar;
    use crate::timetable::{Route, RouteInfo, StopEvent, StopInfo, TripRef};

    fn ev(arr: u32, dep: u32) -> StopEvent {
        StopEvent { arr, dep, board: true, alight: true }
    }

    /// Two routes. A: stops 0-1-2 at 600s intervals, trips every 900 s from
    /// 7:00. B: stops 2-3 (via a footpath from stop 1 to stop 3's twin, 4).
    fn tiny() -> Timetable {
        let stops = (0..5)
            .map(|i| StopInfo { gtfs_id: i.to_string(), name: String::new(), lat: 0.0, lon: 0.0 })
            .collect();
        let mut a = Route { info: 0, stops: vec![0, 1, 2], trips: vec![], events: vec![] };
        for k in 0..4u32 {
            let d = 7 * 3600 + k * 900;
            a.trips.push(TripRef { gtfs_trip: k, service: 0 });
            a.events.extend([ev(d, d), ev(d + 600, d + 600), ev(d + 1200, d + 1200)]);
        }
        let mut b = Route { info: 1, stops: vec![2, 3], trips: vec![], events: vec![] };
        for k in 0..4u32 {
            let d = 7 * 3600 + 1300 + k * 900;
            b.trips.push(TripRef { gtfs_trip: 10 + k, service: k % 2 }); // odd trips: inactive
            b.events.extend([ev(d, d), ev(d + 300, d + 300)]);
        }
        let routes = vec![a, b];
        let mut stop_routes = vec![Vec::new(); 5];
        for (r, route) in routes.iter().enumerate() {
            for (p, &s) in route.stops.iter().enumerate() {
                stop_routes[s as usize].push((r as u32, p as u32));
            }
        }
        let mut transfers: Vec<Vec<(u32, f32)>> = vec![Vec::new(); 5];
        transfers[1].push((4, 100.0));
        Timetable {
            stops,
            route_info: vec![
                RouteInfo { gtfs_id: "A".into(), short_name: "A".into(), long_name: String::new(), route_type: 3, color: None },
                RouteInfo { gtfs_id: "B".into(), short_name: "B".into(), long_name: String::new(), route_type: 3, color: None },
            ],
            routes,
            stop_routes,
            transfers,
            services: ServiceCalendar { ids: vec!["wk".into(), "off".into()], ..Default::default() },
            trip_ids: vec![],
        }
    }

    #[test]
    fn rides_transfers_and_walks() {
        let tt = tiny();
        let mut r = Raptor::new(&tt);
        // at stop 0 at 7:05 -> catch the 7:15 trip, arrive stop 2 at 7:35
        r.run(&tt, &[true, false], [(0, 7 * 3600 + 300)], 5, 1.0, 0, u32::MAX);
        assert_eq!(r.arrival(2), Some(7 * 3600 + 900 + 1200));
        assert_eq!(r.legs(2), 1);
        // transfer to B: the 7:36:40 trip is inactive (service 1), next is 7:51:40
        assert_eq!(r.arrival(3), Some(7 * 3600 + 1300 + 1800 + 300));
        assert_eq!(r.legs(3), 2);
        // footpath from stop 1 (arrive 7:25) to stop 4: 100 m at 1 s/m
        assert_eq!(r.arrival(4), Some(7 * 3600 + 900 + 600 + 100));
        assert_eq!(r.legs(4), 1);
        assert_eq!(r.reached().count(), 5);
    }

    #[test]
    fn respects_limit_and_resets() {
        let tt = tiny();
        let mut r = Raptor::new(&tt);
        r.run(&tt, &[true, true], [(0, 7 * 3600)], 5, 1.0, 0, 7 * 3600 + 700);
        assert_eq!(r.arrival(1), Some(7 * 3600 + 600));
        assert_eq!(r.arrival(2), None);
        r.run(&tt, &[true, true], [(3, 0)], 5, 1.0, 0, u32::MAX);
        assert_eq!(r.arrival(0), None, "stale labels must be cleared");
        assert_eq!(r.arrival(3), Some(0));
    }

    #[test]
    fn boarding_slack_misses_tight_connections() {
        let tt = tiny();
        let mut r = Raptor::new(&tt);
        // at stop 0 exactly at 7:15: no slack catches the 7:15 trip
        r.run(&tt, &[true, true], [(0, 7 * 3600 + 900)], 5, 1.0, 0, u32::MAX);
        assert_eq!(r.arrival(2), Some(7 * 3600 + 900 + 1200));
        // with a minute of slack it is the 7:30 trip
        r.run(&tt, &[true, true], [(0, 7 * 3600 + 900)], 5, 1.0, 60, u32::MAX);
        assert_eq!(r.arrival(2), Some(7 * 3600 + 1800 + 1200));
    }
}
