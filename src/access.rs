//! Batch accessibility: for every origin and departure time, how much of the
//! weighted destination set is reachable within the budget.

use crate::engine::{Engine, Query, QueryOpts};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct Origin {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone)]
pub struct Dest {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub weights: Vec<f64>,
}

pub struct Dests {
    pub names: Vec<String>,
    pub dests: Vec<Dest>,
}

fn column(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers.iter().position(|h| h.eq_ignore_ascii_case(name)).with_context(|| format!("csv has no {name} column"))
}

/// id,lat,lon[,...]. Extra columns are ignored.
pub fn read_origins(path: &Path) -> Result<Vec<Origin>> {
    let mut r = csv::Reader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    let h = r.headers()?.clone();
    let (id, lat, lon) = (column(&h, "id")?, column(&h, "lat")?, column(&h, "lon")?);
    let mut out = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        out.push(Origin { id: rec[id].to_string(), lat: rec[lat].parse()?, lon: rec[lon].parse()? });
    }
    Ok(out)
}

/// id,lat,lon,weight1,weight2,... Every column after lon is a weight.
pub fn read_dests(path: &Path) -> Result<Dests> {
    let mut r = csv::Reader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    let h = r.headers()?.clone();
    let (id, lat, lon) = (column(&h, "id")?, column(&h, "lat")?, column(&h, "lon")?);
    let weight_cols: Vec<usize> = (0..h.len()).filter(|&i| i != id && i != lat && i != lon).collect();
    if weight_cols.is_empty() {
        bail!("destinations need at least one weight column");
    }
    let names = weight_cols.iter().map(|&i| h[i].to_string()).collect();
    let mut dests = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        let weights = weight_cols.iter().map(|&i| rec[i].parse::<f64>()).collect::<Result<Vec<_>, _>>()?;
        dests.push(Dest { id: rec[id].to_string(), lat: rec[lat].parse()?, lon: rec[lon].parse()?, weights });
    }
    Ok(Dests { names, dests })
}

pub struct AccessOpts {
    pub base: QueryOpts,
    pub departures: Vec<u32>,
    /// how far a destination may sit from the walking network
    pub dest_snap_max_m: f64,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub origin: String,
    pub depart: u32,
    pub on_network: bool,
    pub count: u32,
    pub reached: Vec<f64>,
}

/// Snapped destinations: node plus straight-line walk, in parallel arrays.
struct SnappedDests {
    node: Vec<u32>,
    snap_m: Vec<f64>,
}

pub fn run(engine: &Engine, origins: &[Origin], dests: &Dests, opts: &AccessOpts) -> Vec<Row> {
    let index = engine.index();
    let mut snapped = SnappedDests { node: Vec::with_capacity(dests.dests.len()), snap_m: Vec::with_capacity(dests.dests.len()) };
    let mut off = 0usize;
    for d in &dests.dests {
        match index.nearest(d.lat, d.lon, opts.dest_snap_max_m) {
            Some((n, m)) => {
                snapped.node.push(n);
                snapped.snap_m.push(m);
            }
            None => {
                off += 1;
                snapped.node.push(u32::MAX);
                snapped.snap_m.push(0.0);
            }
        }
    }
    if off > 0 {
        log::warn!("{off} of {} destinations are more than {} m from the walking network", dests.dests.len(), opts.dest_snap_max_m);
    }

    let nw = dests.names.len();
    let done = AtomicUsize::new(0);
    let total = origins.len();
    let t0 = std::time::Instant::now();

    origins
        .par_iter()
        .map_init(
            || Query::new(engine),
            |q, o| {
                let mut rows = Vec::with_capacity(opts.departures.len());
                for &depart in &opts.departures {
                    let qo = QueryOpts { depart, ..opts.base.clone() };
                    let on_network = q.run(o.lat, o.lon, &qo);
                    let mut reached = vec![0.0; nw];
                    let mut count = 0u32;
                    if on_network {
                        let pace = q.pace();
                        for (i, d) in dests.dests.iter().enumerate() {
                            let n = snapped.node[i];
                            if n == u32::MAX {
                                continue;
                            }
                            if let Some(t) = q.node_secs(n) {
                                let t = t + (snapped.snap_m[i] * pace).round() as u32;
                                if t <= qo.max_secs {
                                    count += 1;
                                    for (acc, w) in reached.iter_mut().zip(&d.weights) {
                                        *acc += w;
                                    }
                                }
                            }
                        }
                    }
                    rows.push(Row { origin: o.id.clone(), depart, on_network, count, reached });
                }
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(500) || n == total {
                    log::info!("{n}/{total} origins ({:.0?})", t0.elapsed());
                }
                rows
            },
        )
        .flatten()
        .collect()
}

pub fn write_rows(path: &Path, names: &[String], rows: &[Row]) -> Result<()> {
    let mut w = csv::Writer::from_path(path).with_context(|| format!("creating {}", path.display()))?;
    let mut header = vec!["origin".to_string(), "depart".into(), "on_network".into(), "count".into()];
    header.extend(names.iter().cloned());
    w.write_record(&header)?;
    for r in rows {
        let mut rec = vec![r.origin.clone(), crate::gtfs::format_time(r.depart), r.on_network.to_string(), r.count.to_string()];
        rec.extend(r.reached.iter().map(|v| format!("{v}")));
        w.write_record(&rec)?;
    }
    w.flush()?;
    Ok(())
}
