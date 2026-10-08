use timeshed::access::{self, AccessOpts};
use timeshed::engine::{BuildOpts, Engine, Query, QueryOpts};
use timeshed::geo::BBox;
use timeshed::gtfs::{format_time, parse_time};
use timeshed::isochrone::{Grid, IsochroneOpts};
use timeshed::{gtfs, osm, server, timetable};

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "timeshed", about = "Door-to-door transit travel time engine")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read a GTFS feed and print summary statistics
    Inspect {
        /// Path to a GTFS zip or unpacked directory
        feed: PathBuf,
        /// Date to report active service for (YYYY-MM-DD)
        #[arg(long)]
        date: Option<chrono::NaiveDate>,
    },
    /// Build the routing bundle from a GTFS feed and an OSM extract
    Build {
        /// Path to a GTFS zip or unpacked directory
        #[arg(long)]
        gtfs: PathBuf,
        /// Path to an OSM .pbf extract covering the transit network
        #[arg(long)]
        osm: PathBuf,
        /// Walking network bounding box "min_lon,min_lat,max_lon,max_lat".
        /// Default: the stops' bounding box plus 2.5 km.
        #[arg(long, value_parser = parse_bbox)]
        bbox: Option<BBox>,
        /// Merge runs of shape nodes into edges up to this long (meters)
        #[arg(long, default_value_t = 150.0)]
        max_edge: f32,
        /// Name of the bundle (shown in the UI)
        #[arg(long, default_value = "transit")]
        name: String,
        /// Where to write the bundle
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Run one door-to-door query and print what it reached
    Query {
        /// Bundle written by `build`
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long, allow_negative_numbers = true)]
        lat: f64,
        #[arg(long, allow_negative_numbers = true)]
        lon: f64,
        /// Service date (YYYY-MM-DD)
        #[arg(long)]
        date: chrono::NaiveDate,
        /// Departure time (HH:MM or HH:MM:SS)
        #[arg(long, default_value = "08:00")]
        time: String,
        /// Travel time budget in minutes
        #[arg(long, default_value_t = 45)]
        max: u32,
        /// Walking speed in m/s
        #[arg(long, default_value_t = 1.3)]
        walk_speed: f64,
        /// Seconds you must be at a stop before departure to board
        #[arg(long, default_value_t = 0)]
        board_slack: u32,
        /// Write the isochrone bands as GeoJSON to this file
        #[arg(long)]
        geojson: Option<PathBuf>,
    },
    /// Serve the live map and the HTTP API
    Serve {
        /// Bundle written by `build`
        #[arg(long)]
        bundle: PathBuf,
        /// Directory with the static frontend
        #[arg(long, default_value = "web")]
        web: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: std::net::SocketAddr,
    },
    /// Batch accessibility: weighted destinations reachable from every origin
    Access {
        /// Bundle written by `build`
        #[arg(long)]
        bundle: PathBuf,
        /// CSV with id,lat,lon
        #[arg(long)]
        origins: PathBuf,
        /// CSV with id,lat,lon and one or more weight columns
        #[arg(long)]
        dests: PathBuf,
        /// Service date (YYYY-MM-DD)
        #[arg(long)]
        date: chrono::NaiveDate,
        /// Explicit departure times, comma separated (HH:MM)
        #[arg(long, value_delimiter = ',')]
        times: Vec<String>,
        /// Or a window: first departure (HH:MM)...
        #[arg(long)]
        from: Option<String>,
        /// ...last departure, exclusive (HH:MM)...
        #[arg(long)]
        to: Option<String>,
        /// ...stepping this many minutes
        #[arg(long, default_value_t = 10)]
        every: u32,
        /// Travel time budget in minutes
        #[arg(long, default_value_t = 45)]
        max: u32,
        /// Walking speed in m/s
        #[arg(long, default_value_t = 1.3)]
        walk_speed: f64,
        /// Seconds you must be at a stop before departure to board
        #[arg(long, default_value_t = 0)]
        board_slack: u32,
        /// Output CSV
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Pairwise travel times, origins x destinations, at one departure
    Matrix {
        /// Bundle written by `build`
        #[arg(long)]
        bundle: PathBuf,
        /// CSV with id,lat,lon
        #[arg(long)]
        origins: PathBuf,
        /// CSV with id,lat,lon (other columns ignored)
        #[arg(long)]
        dests: PathBuf,
        /// Service date (YYYY-MM-DD)
        #[arg(long)]
        date: chrono::NaiveDate,
        /// Departure time (HH:MM)
        #[arg(long)]
        time: String,
        /// Travel time budget in minutes
        #[arg(long, default_value_t = 120)]
        max: u32,
        /// Walking speed in m/s
        #[arg(long, default_value_t = 1.3)]
        walk_speed: f64,
        /// Seconds you must be at a stop before departure to board
        #[arg(long, default_value_t = 0)]
        board_slack: u32,
        /// Output CSV: origin,dest,seconds (unreachable pairs are omitted)
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Check RAPTOR against a brute-force router on random queries
    Verify {
        /// Bundle written by `build`
        #[arg(long)]
        bundle: PathBuf,
        /// The GTFS feed the bundle was built from
        #[arg(long)]
        gtfs: PathBuf,
        /// Number of random queries
        #[arg(long, default_value_t = 1000)]
        queries: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Travel time budget in minutes
        #[arg(long, default_value_t = 90)]
        max: u32,
    },
}

fn time_arg(s: &str) -> Result<u32> {
    parse_time(s)?.ok_or_else(|| anyhow::anyhow!("empty time"))
}

fn parse_bbox(s: &str) -> Result<BBox, String> {
    BBox::parse(s).ok_or_else(|| format!("expected min_lon,min_lat,max_lon,max_lat, got {s:?}"))
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Inspect { feed, date } => {
            let t0 = std::time::Instant::now();
            let feed = gtfs::Feed::read(&feed)?;
            println!(
                "{} stops, {} routes, {} trips, {} stop_times, {} services, {} frequencies ({:.1?})",
                feed.stops.len(),
                feed.routes.len(),
                feed.trips.len(),
                feed.stop_times.len(),
                feed.services.ids.len(),
                feed.frequencies.len(),
                t0.elapsed()
            );
            if let Some(date) = date {
                let active = feed.services.active(date);
                let trips = feed.trips.iter().filter(|t| active[t.service as usize]).count();
                println!("{date}: {} active services, {trips} trips", active.iter().filter(|a| **a).count());
            }
        }
        Cmd::Build { gtfs, osm, bbox, max_edge, name, out } => {
            let t0 = std::time::Instant::now();
            let feed = gtfs::Feed::read(&gtfs)?;
            let tt = timetable::Timetable::from_feed(&feed);
            log::info!(
                "timetable: {} stops, {} raptor routes from {} gtfs routes, {} trips ({:.1?})",
                tt.stops.len(),
                tt.routes.len(),
                tt.route_info.len(),
                tt.routes.iter().map(|r| r.trips.len()).sum::<usize>(),
                t0.elapsed()
            );
            let bbox = bbox.unwrap_or_else(|| tt.bbox().buffer(2500.0));
            log::info!("walking bbox: {bbox:?}");
            let walk = osm::read_walk_graph(&osm, bbox, max_edge)?;
            let engine = Engine::build(name, tt, walk, BuildOpts::default());
            engine.save(&out)?;
            log::info!("wrote {} in {:.1?}", out.display(), t0.elapsed());
        }
        Cmd::Query { bundle, lat, lon, date, time, max, walk_speed, board_slack, geojson } => {
            let t0 = std::time::Instant::now();
            let engine = Engine::load(&bundle)?;
            log::info!("loaded {} ({} stops, {} nodes) in {:.1?}", engine.name, engine.tt.stops.len(), engine.walk.node_count(), t0.elapsed());
            let opts = QueryOpts { date, depart: time_arg(&time)?, max_secs: max * 60, walk_speed_mps: walk_speed, board_slack_secs: board_slack, ..Default::default() };
            let mut q = Query::new(&engine);
            let t1 = std::time::Instant::now();
            if !q.run(lat, lon, &opts) {
                anyhow::bail!("origin is too far from the walking network");
            }
            let elapsed = t1.elapsed();
            let stops: Vec<_> = q.reached_stops().collect();
            let by_legs = |k: u8| stops.iter().filter(|s| s.2 == k).count();
            println!(
                "from ({lat}, {lon}) at {} on {date}, {max} min budget: {} stops reached (walk only {}, 1 trip {}, 2 trips {}, 3+ trips {}), {} walking nodes ({elapsed:.1?})",
                format_time(opts.depart),
                stops.len(),
                by_legs(0),
                by_legs(1),
                by_legs(2),
                stops.iter().filter(|s| s.2 >= 3).count(),
                q.reached_node_count()
            );
            let mut far: Vec<_> = stops.iter().filter(|s| s.2 > 0).collect();
            far.sort_by_key(|s| std::cmp::Reverse(s.1));
            for (s, secs, legs) in far.iter().take(5) {
                let st = &engine.tt.stops[*s as usize];
                println!("  {:>5} min, {legs} trips: {} ({}, {})", secs / 60, st.name, st.lat, st.lon);
            }
            if let Some(path) = geojson {
                let t2 = std::time::Instant::now();
                let iso = IsochroneOpts { max_secs: max * 60, ..Default::default() };
                let fc = match Grid::from_query(&q, &iso) {
                    Some(grid) => grid.isobands(&iso)?,
                    None => serde_json::json!({ "type": "FeatureCollection", "features": [] }),
                };
                std::fs::write(&path, serde_json::to_vec(&fc)?)?;
                println!("wrote {} ({:.1?})", path.display(), t2.elapsed());
            }
        }
        Cmd::Serve { bundle, web, addr } => {
            let t0 = std::time::Instant::now();
            let engine = Engine::load(&bundle)?;
            log::info!("loaded {} ({} stops, {} nodes) in {:.1?}", engine.name, engine.tt.stops.len(), engine.walk.node_count(), t0.elapsed());
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::serve(engine, web, addr))?;
        }
        Cmd::Access { bundle, origins, dests, date, times, from, to, every, max, walk_speed, board_slack, out } => {
            let mut departures: Vec<u32> = times.iter().map(|t| time_arg(t)).collect::<Result<_>>()?;
            if let (Some(from), Some(to)) = (from, to) {
                let (mut t, end) = (time_arg(&from)?, time_arg(&to)?);
                while t < end {
                    departures.push(t);
                    t += every * 60;
                }
            }
            if departures.is_empty() {
                anyhow::bail!("give --times or --from/--to");
            }
            let t0 = std::time::Instant::now();
            let engine = Engine::load(&bundle)?;
            let origins = access::read_origins(&origins)?;
            let dests = access::read_dests(&dests)?;
            log::info!(
                "{} origins x {} departures on {date}, {} destinations weighted by {:?}, {max} min budget",
                origins.len(),
                departures.len(),
                dests.dests.len(),
                dests.names
            );
            let opts = AccessOpts {
                base: QueryOpts { date, max_secs: max * 60, walk_speed_mps: walk_speed, board_slack_secs: board_slack, ..Default::default() },
                departures,
                dest_snap_max_m: 500.0,
            };
            let rows = access::run(&engine, &origins, &dests, &opts);
            access::write_rows(&out, &dests.names, &rows)?;
            let off = rows.iter().filter(|r| !r.on_network).count() / opts.departures.len().max(1);
            log::info!("wrote {} rows to {} ({off} origins off the network) in {:.1?}", rows.len(), out.display(), t0.elapsed());
        }
        Cmd::Matrix { bundle, origins, dests, date, time, max, walk_speed, board_slack, out } => {
            let t0 = std::time::Instant::now();
            let engine = Engine::load(&bundle)?;
            let origins = access::read_origins(&origins)?;
            let dests = access::read_origins(&dests)?
                .into_iter()
                .map(|o| access::Dest { id: o.id, lat: o.lat, lon: o.lon, weights: vec![] })
                .collect::<Vec<_>>();
            let opts = QueryOpts { date, depart: time_arg(&time)?, max_secs: max * 60, walk_speed_mps: walk_speed, board_slack_secs: board_slack, ..Default::default() };
            let m = access::matrix(&engine, &origins, &dests, &opts, 500.0);
            let mut w = csv::Writer::from_path(&out)?;
            w.write_record(["origin", "dest", "seconds"])?;
            let mut n = 0usize;
            for (o, row) in origins.iter().zip(&m) {
                for (d, t) in dests.iter().zip(row) {
                    if let Some(t) = t {
                        w.write_record([&o.id, &d.id, &t.to_string()])?;
                        n += 1;
                    }
                }
            }
            w.flush()?;
            log::info!("{n} reachable pairs of {} written to {} ({:.1?})", origins.len() * dests.len(), out.display(), t0.elapsed());
        }
        Cmd::Verify { bundle, gtfs, queries, seed, max } => {
            use rand::prelude::*;
            use rayon::prelude::*;
            use timeshed::raptor::Raptor;
            use timeshed::reference::Harness;

            let engine = Engine::load(&bundle)?;
            let feed = gtfs::Feed::read(&gtfs)?;
            let tt = &engine.tt;
            let harness = Harness::new(&feed, tt);
            let (first, last) = tt.services.date_range().ok_or_else(|| anyhow::anyhow!("feed has no service dates"))?;
            let days = (last - first).num_days().max(0) as u64;
            log::info!("{queries} random queries over {} stops, dates {first}..{last}, {max} min budget", tt.stops.len());

            // one query spec per seed so runs are reproducible and parallel
            type Spec = (chrono::NaiveDate, Vec<(u32, u32)>, u32);
            /// (query index, mismatches, labels reached, labels lost to the round cap)
            type Outcome = (usize, Vec<(u32, u32, u32)>, usize, usize);
            let specs: Vec<Spec> = (0..queries as u64)
                .map(|i| {
                    let mut rng = StdRng::seed_from_u64(seed.wrapping_mul(1_000_003).wrapping_add(i));
                    let date = first + chrono::Days::new(rng.random_range(0..=days));
                    let n = rng.random_range(1..=3);
                    let start = 4 * 3600 + rng.random_range(0..22 * 3600);
                    let sources = (0..n)
                        .map(|_| (rng.random_range(0..tt.stops.len()) as u32, start + rng.random_range(0..600)))
                        .collect();
                    let slack = [0, 0, 30, 60][rng.random_range(0..4)];
                    (date, sources, slack)
                })
                .collect();

            let t0 = std::time::Instant::now();
            let pace = 1.0 / 1.3;
            let results: Vec<Outcome> = specs
                .par_iter()
                .enumerate()
                .map_init(
                    || (Raptor::new(tt), Raptor::new(tt)),
                    |(raptor, capped), (i, (date, sources, slack))| {
                        let active = tt.services.active(*date);
                        let start = sources.iter().map(|s| s.1).min().unwrap();
                        let limit = start + max * 60;
                        let bad = harness.compare(raptor, &active, sources, 100, pace, *slack, limit);
                        // how much does the default round cap cost?
                        capped.run(tt, &active, sources.iter().copied(), QueryOpts::default().max_rounds, pace, *slack, limit);
                        let reached = raptor.reached().count();
                        let lost = (0..tt.stops.len() as u32)
                            .filter(|&s| raptor.arrival(s) != capped.arrival(s))
                            .count();
                        (i, bad, reached, lost)
                    },
                )
                .collect();

            let mut mismatches = 0usize;
            let mut labels = 0usize;
            let mut lost = 0usize;
            let mut shown = 0;
            for (i, bad, reached, l) in &results {
                labels += reached;
                lost += l;
                if !bad.is_empty() {
                    mismatches += bad.len();
                    if shown < 10 {
                        shown += 1;
                        let (date, sources, _) = &specs[*i];
                        let (s, a, b) = bad[0];
                        println!(
                            "MISMATCH query {i} on {date} from {:?}: stop {} ({}) raptor={} brute={} (+{} more)",
                            sources,
                            tt.stops[s as usize].gtfs_id,
                            tt.stops[s as usize].name,
                            if a == u32::MAX { "unreached".to_string() } else { format_time(a) },
                            if b == u32::MAX { "unreached".to_string() } else { format_time(b) },
                            bad.len() - 1
                        );
                    }
                }
            }
            println!(
                "{queries} queries, {labels} reached stop labels compared against brute force: {mismatches} mismatches ({:.1?})",
                t0.elapsed()
            );
            println!(
                "labels that need more than {} trips (lost under the default round cap): {lost} ({:.4}%)",
                QueryOpts::default().max_rounds,
                100.0 * lost as f64 / labels.max(1) as f64
            );
            if mismatches > 0 {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
