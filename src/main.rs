use timeshed::geo::BBox;
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
    },
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
        Cmd::Build { gtfs, osm, bbox } => {
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
            log::info!("done in {:.1?}; {} nodes, {} edges", t0.elapsed(), walk.node_count(), walk.edge_count());
        }
    }
    Ok(())
}
