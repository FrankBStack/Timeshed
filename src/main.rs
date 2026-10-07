use timeshed::gtfs;

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
                feed.services.len(),
                feed.frequencies.len(),
                t0.elapsed()
            );
            if let Some(date) = date {
                let active = feed.active_services(date);
                let trips = feed.trips.iter().filter(|t| active[t.service as usize]).count();
                println!("{date}: {} active services, {trips} trips", active.iter().filter(|a| **a).count());
            }
        }
    }
    Ok(())
}
