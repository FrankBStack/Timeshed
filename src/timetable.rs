//! RAPTOR-shaped timetable built from a GTFS feed.
//!
//! RAPTOR wants "routes" whose trips all visit exactly the same stop sequence
//! and never overtake one another. GTFS routes guarantee neither (branches,
//! short-turns, express variants), so trips are regrouped here.

use crate::gtfs::{Feed, ServiceCalendar, StopTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StopInfo {
    pub gtfs_id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RouteInfo {
    pub gtfs_id: String,
    pub short_name: String,
    pub long_name: String,
    pub route_type: u16,
    pub color: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct StopEvent {
    pub arr: u32,
    pub dep: u32,
    pub board: bool,
    pub alight: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct TripRef {
    pub gtfs_trip: u32,
    pub service: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Route {
    /// index into `Timetable::route_info`
    pub info: u32,
    /// stop indices in travel order
    pub stops: Vec<u32>,
    /// sorted by departure from `stops[0]`; no trip overtakes an earlier one
    pub trips: Vec<TripRef>,
    /// `events[trip * stops.len() + i]`
    pub events: Vec<StopEvent>,
}

impl Route {
    #[inline]
    pub fn event(&self, trip: usize, i: usize) -> &StopEvent {
        &self.events[trip * self.stops.len() + i]
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Timetable {
    pub stops: Vec<StopInfo>,
    pub route_info: Vec<RouteInfo>,
    pub routes: Vec<Route>,
    /// for every stop: (route, position of the stop within the route)
    pub stop_routes: Vec<Vec<(u32, u32)>>,
    /// footpaths between stops in seconds; filled in once a walking graph exists
    pub transfers: Vec<Vec<(u32, u32)>>,
    pub services: ServiceCalendar,
    pub trip_ids: Vec<String>,
}

/// One trip ready for grouping: its stop sequence and stop events.
struct TripInstance {
    gtfs_trip: u32,
    gtfs_route: u32,
    service: u32,
    stops: Vec<u32>,
    events: Vec<StopEvent>,
}

fn events_of(sts: &[StopTime], stop_map: &[u32], offset: i64) -> (Vec<u32>, Vec<StopEvent>) {
    let stops = sts.iter().map(|st| stop_map[st.stop as usize]).collect();
    let events = sts
        .iter()
        .map(|st| StopEvent {
            arr: (st.arrival as i64 + offset).max(0) as u32,
            dep: (st.departure as i64 + offset).max(0) as u32,
            board: st.pickup,
            alight: st.drop_off,
        })
        .collect();
    (stops, events)
}

impl Timetable {
    pub fn from_feed(feed: &Feed) -> Timetable {
        // Only keep stops that a trip actually serves; stations and unused
        // stops would just be dead weight (and bad snapping targets).
        let mut used = vec![false; feed.stops.len()];
        for st in &feed.stop_times {
            used[st.stop as usize] = true;
        }
        let mut stop_map = vec![u32::MAX; feed.stops.len()];
        let mut stops = Vec::new();
        for (i, s) in feed.stops.iter().enumerate() {
            if used[i] {
                stop_map[i] = stops.len() as u32;
                stops.push(StopInfo { gtfs_id: s.id.clone(), name: s.name.clone(), lat: s.lat, lon: s.lon });
            }
        }

        // Frequency-based trips are templates; expand them into real trips.
        let mut freq_by_trip: HashMap<u32, Vec<&crate::gtfs::Frequency>> = HashMap::new();
        for f in &feed.frequencies {
            freq_by_trip.entry(f.trip).or_default().push(f);
        }

        let mut instances: Vec<TripInstance> = Vec::new();
        for (t, trip) in feed.trips.iter().enumerate() {
            let sts = feed.trip_stop_times(t as u32);
            if sts.len() < 2 {
                continue;
            }
            let make = |offset: i64| {
                let (stops, events) = events_of(sts, &stop_map, offset);
                TripInstance { gtfs_trip: t as u32, gtfs_route: trip.route, service: trip.service, stops, events }
            };
            match freq_by_trip.get(&(t as u32)) {
                None => instances.push(make(0)),
                Some(freqs) => {
                    let first_dep = sts[0].departure as i64;
                    for f in freqs {
                        let headway = f.headway.max(1) as i64;
                        let mut start = f.start as i64;
                        while start < f.end as i64 {
                            instances.push(make(start - first_dep));
                            start += headway;
                        }
                    }
                }
            }
        }

        // Group by (gtfs route, exact stop sequence).
        let mut groups: HashMap<(u32, Vec<u32>), Vec<usize>> = HashMap::new();
        for (i, inst) in instances.iter().enumerate() {
            groups.entry((inst.gtfs_route, inst.stops.clone())).or_default().push(i);
        }
        let mut keys: Vec<_> = groups.keys().cloned().collect();
        keys.sort(); // deterministic route numbering

        let route_info: Vec<RouteInfo> = feed
            .routes
            .iter()
            .map(|r| RouteInfo {
                gtfs_id: r.id.clone(),
                short_name: r.short_name.clone(),
                long_name: r.long_name.clone(),
                route_type: r.route_type,
                color: r.color.clone(),
            })
            .collect();

        let mut routes: Vec<Route> = Vec::new();
        let mut split_count = 0usize;
        for key in keys {
            let mut members = groups.remove(&key).unwrap();
            members.sort_by_key(|&i| (instances[i].events[0].dep, instances[i].events.last().unwrap().arr));

            // Peel off non-overtaking chains until every trip has a home.
            let mut pending = members;
            let mut first = true;
            while !pending.is_empty() {
                let mut chain: Vec<usize> = Vec::new();
                let mut rest: Vec<usize> = Vec::new();
                for i in pending {
                    let overtakes_at = match chain.last() {
                        None => None,
                        Some(&last) => instances[i]
                            .events
                            .iter()
                            .zip(&instances[last].events)
                            .position(|(a, b)| a.arr < b.arr || a.dep < b.dep),
                    };
                    match overtakes_at {
                        None => chain.push(i),
                        Some(pos) => {
                            let last = *chain.last().unwrap();
                            log::debug!(
                                "trip {} overtakes {} at stop {} ({} vs {})",
                                feed.trips[instances[i].gtfs_trip as usize].id,
                                feed.trips[instances[last].gtfs_trip as usize].id,
                                stops[key.1[pos] as usize].gtfs_id,
                                crate::gtfs::format_time(instances[i].events[pos].arr),
                                crate::gtfs::format_time(instances[last].events[pos].arr),
                            );
                            rest.push(i);
                        }
                    }
                }
                if !first {
                    split_count += 1;
                }
                first = false;
                let stops = key.1.clone();
                let mut events = Vec::with_capacity(chain.len() * stops.len());
                let mut trips = Vec::with_capacity(chain.len());
                for &i in &chain {
                    events.extend_from_slice(&instances[i].events);
                    trips.push(TripRef { gtfs_trip: instances[i].gtfs_trip, service: instances[i].service });
                }
                routes.push(Route { info: key.0, stops, trips, events });
                pending = rest;
            }
        }
        if split_count > 0 {
            log::info!("{split_count} extra route groups created to keep trips from overtaking");
        }

        let mut stop_routes: Vec<Vec<(u32, u32)>> = vec![Vec::new(); stops.len()];
        for (r, route) in routes.iter().enumerate() {
            for (pos, &s) in route.stops.iter().enumerate() {
                stop_routes[s as usize].push((r as u32, pos as u32));
            }
        }

        Timetable {
            transfers: vec![Vec::new(); stops.len()],
            stops,
            route_info,
            routes,
            stop_routes,
            services: feed.services.clone(),
            trip_ids: feed.trips.iter().map(|t| t.id.clone()).collect(),
        }
    }

    pub fn bbox(&self) -> crate::geo::BBox {
        let mut b = crate::geo::BBox::empty();
        for s in &self.stops {
            b.include(s.lon, s.lat);
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtfs::{Route as GRoute, Stop, Trip};

    fn stop(i: u32) -> Stop {
        Stop { id: format!("s{i}"), name: String::new(), lat: 43.0, lon: -87.9 + i as f64 * 0.01, location_type: 0, parent_station: None }
    }

    fn st(trip: u32, stop: u32, seq: u32, t: u32) -> StopTime {
        StopTime { trip, stop, seq, arrival: t, departure: t, pickup: true, drop_off: true }
    }

    #[test]
    fn groups_by_stop_sequence_and_splits_overtakers() {
        let mut feed = Feed::default();
        feed.stops = (0..3).map(stop).collect();
        feed.routes.push(GRoute { id: "R".into(), short_name: "R".into(), long_name: String::new(), route_type: 3, color: None });
        feed.services.ids.push("wk".into());
        for i in 0..4 {
            feed.trips.push(Trip { id: format!("t{i}"), route: 0, service: 0, headsign: String::new(), direction: None });
        }
        // t0 and t1: same stops, t1 later everywhere. t2: express that
        // leaves after t1 but arrives before it (overtakes). t3: short-turn.
        feed.stop_times = vec![
            st(0, 0, 1, 100), st(0, 1, 2, 200), st(0, 2, 3, 300),
            st(1, 0, 1, 150), st(1, 1, 2, 250), st(1, 2, 3, 350),
            st(2, 0, 1, 160), st(2, 1, 2, 170), st(2, 2, 3, 180),
            st(3, 0, 1, 400), st(3, 1, 2, 500),
        ];
        let tt = Timetable::from_feed(&feed);
        assert_eq!(tt.stops.len(), 3);
        assert_eq!(tt.routes.len(), 3, "full chain, overtaker, short-turn");
        let full = tt.routes.iter().find(|r| r.trips.len() == 2).unwrap();
        assert_eq!(full.stops, vec![0, 1, 2]);
        assert_eq!(full.event(1, 2).arr, 350);
        assert_eq!(tt.stop_routes[2].len(), 2);
    }
}
