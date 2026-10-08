//! Turn a query's node labels into isochrone polygons.
//!
//! Reached nodes are splatted onto a regular lon/lat grid (each node
//! stamps its travel time plus a straight-line walk onto nearby cells), and
//! marching squares cuts the grid into bands of travel time.

use crate::engine::Query;
use crate::geo::BBox;
use contour::ContourBuilder;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct IsochroneOpts {
    /// grid cell size in meters
    pub cell_m: f64,
    /// how far off the network a node's label extends
    pub reach_m: f64,
    /// width of each band in seconds
    pub band_secs: u32,
    /// outermost edge in seconds
    pub max_secs: u32,
}

impl Default for IsochroneOpts {
    fn default() -> Self {
        IsochroneOpts { cell_m: 100.0, reach_m: 250.0, band_secs: 300, max_secs: 45 * 60 }
    }
}

/// A travel-time raster in lon/lat.
pub struct Grid {
    pub nx: usize,
    pub ny: usize,
    pub min_lon: f64,
    pub min_lat: f64,
    pub dlon: f64,
    pub dlat: f64,
    /// seconds; `UNREACHED` where nothing is near
    pub secs: Vec<f64>,
}

pub const UNREACHED: f64 = 1e9;

impl Grid {
    /// Rasterize the last run of `q`. None if nothing was reached.
    pub fn from_query(q: &Query, opts: &IsochroneOpts) -> Option<Grid> {
        let g = &q.engine.walk;
        let mut bbox = BBox::empty();
        let mut any = false;
        for (n, t) in q.reached_nodes() {
            if t <= opts.max_secs {
                let (lat, lon) = g.lat_lon(n);
                bbox.include(lon, lat);
                any = true;
            }
        }
        if !any {
            return None;
        }
        let bbox = bbox.buffer(opts.reach_m + opts.cell_m);
        let dlat = opts.cell_m / 111_320.0;
        let dlon = opts.cell_m / (111_320.0 * bbox.mid_lat().to_radians().cos());
        let nx = ((bbox.max_lon - bbox.min_lon) / dlon).ceil() as usize + 1;
        let ny = ((bbox.max_lat - bbox.min_lat) / dlat).ceil() as usize + 1;
        let mut secs = vec![UNREACHED; nx * ny];

        let r = (opts.reach_m / opts.cell_m).ceil() as i64;
        let pace = q.pace();
        for (n, t) in q.reached_nodes() {
            if t > opts.max_secs {
                continue;
            }
            let (lat, lon) = g.lat_lon(n);
            let fx = (lon - bbox.min_lon) / dlon;
            let fy = (lat - bbox.min_lat) / dlat;
            let (cx, cy) = (fx.round() as i64, fy.round() as i64);
            for j in (cy - r).max(0)..=(cy + r).min(ny as i64 - 1) {
                for i in (cx - r).max(0)..=(cx + r).min(nx as i64 - 1) {
                    let dx = (i as f64 - fx) * opts.cell_m;
                    let dy = (j as f64 - fy) * opts.cell_m;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d <= opts.reach_m {
                        let v = t as f64 + d * pace;
                        let cell = &mut secs[j as usize * nx + i as usize];
                        if v < *cell {
                            *cell = v;
                        }
                    }
                }
            }
        }
        Some(Grid { nx, ny, min_lon: bbox.min_lon, min_lat: bbox.min_lat, dlon, dlat, secs })
    }

    /// Isochrone bands as a GeoJSON FeatureCollection. Each feature carries
    /// `from` and `to` in minutes.
    pub fn isobands(&self, opts: &IsochroneOpts) -> anyhow::Result<Value> {
        let minutes: Vec<f64> = self.secs.iter().map(|s| if *s >= UNREACHED { UNREACHED } else { s / 60.0 }).collect();
        let mut thresholds: Vec<f64> = Vec::new();
        let mut t = 0u32;
        while t < opts.max_secs {
            thresholds.push(t as f64 / 60.0);
            t += opts.band_secs;
        }
        thresholds.push(opts.max_secs as f64 / 60.0);

        let builder = ContourBuilder::new(self.nx, self.ny, true)
            .x_origin(self.min_lon)
            .y_origin(self.min_lat)
            .x_step(self.dlon)
            .y_step(self.dlat);
        let bands = builder.isobands(&minutes, &thresholds)?;

        let features: Vec<Value> = bands
            .iter()
            .filter(|b| !b.geometry().0.is_empty())
            .map(|b| {
                let polys: Vec<Value> = b
                    .geometry()
                    .iter()
                    .map(|poly| {
                        let mut rings = vec![ring_json(poly.exterior().coords().map(|c| (c.x, c.y)))];
                        rings.extend(poly.interiors().iter().map(|r| ring_json(r.coords().map(|c| (c.x, c.y)))));
                        Value::Array(rings)
                    })
                    .collect();
                json!({
                    "type": "Feature",
                    "properties": { "from": b.min_v(), "to": b.max_v() },
                    "geometry": { "type": "MultiPolygon", "coordinates": polys }
                })
            })
            .collect();
        Ok(json!({ "type": "FeatureCollection", "features": features }))
    }
}

fn ring_json(coords: impl Iterator<Item = (f64, f64)>) -> Value {
    // 5 decimals is about a meter; plenty for a 100 m grid and keeps payloads small
    Value::Array(coords.map(|(x, y)| json!([round5(x), round5(y)])).collect())
}

fn round5(v: f64) -> f64 {
    (v * 1e5).round() / 1e5
}
