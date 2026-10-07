use timeshed::engine::{BuildOpts, Engine, Query, QueryOpts};
use timeshed::geo::BBox;
use timeshed::gtfs::{format_time, parse_time};
use timeshed::{gtfs, osm, timetable};

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
        Cmd::Build { gtfs, osm, bbox, name, out } => {
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
            let walk = osm::read_walk_graph(&osm, bbox)?;
            let engine = Engine::build(name, tt, walk, BuildOpts::default());
            engine.save(&out)?;
            log::info!("wrote {} in {:.1?}", out.display(), t0.elapsed());
        }
        Cmd::Query { bundle, lat, lon, date, time, max, walk_speed } => {
            let t0 = std::time::Instant::now();
            let engine = Engine::load(&bundle)?;
            log::info!("loaded {} ({} stops, {} nodes) in {:.1?}", engine.name, engine.tt.stops.len(), engine.walk.node_count(), t0.elapsed());
            let opts = QueryOpts { date, depart: time_arg(&time)?, max_secs: max * 60, walk_speed_mps: walk_speed, ..Default::default() };
            let mut q = Query::new(&engine, walk_speed);
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
        }
    }
    Ok(())
}
