//! The browser build: the whole engine behind a few wasm-bindgen calls.
//! Loaded once with the bundle bytes, then queried from a web worker.

use crate::engine::{Engine, Query, QueryOpts};
use crate::gtfs::parse_time;
use crate::isochrone::{Grid, IsochroneOpts};
use chrono::NaiveDate;
use serde_json::json;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WebEngine {
    engine: &'static Engine,
    query: Query<'static>,
}

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl WebEngine {
    /// Deserialize a bundle. The engine is leaked on purpose: it lives as
    /// long as the page, and the query workspace borrows it.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<WebEngine, JsValue> {
        console_error_panic_hook::set_once();
        let engine: &'static Engine = Box::leak(Box::new(Engine::from_bytes(bytes).map_err(err)?));
        Ok(WebEngine { engine, query: Query::new(engine) })
    }

    /// Same shape as the server's /api/info.
    pub fn info(&self) -> String {
        let e = self.engine;
        let b = e.tt.bbox();
        let (first, last) = e.tt.services.date_range().unwrap_or((NaiveDate::MIN, NaiveDate::MIN));
        json!({
            "name": e.name,
            "stops": e.tt.stops.len(),
            "routes": e.tt.route_info.len(),
            "walk_nodes": e.walk.node_count(),
            "bbox": [b.min_lon, b.min_lat, b.max_lon, b.max_lat],
            "center": [b.mid_lon(), b.mid_lat()],
            "first_date": first.to_string(),
            "last_date": last.to_string(),
        })
        .to_string()
    }

    /// Isochrone bands as a GeoJSON string, same shape as /api/isochrone.
    #[allow(clippy::too_many_arguments)]
    pub fn isochrone(
        &mut self,
        lat: f64,
        lon: f64,
        date: &str,
        time: &str,
        max_min: u32,
        band_min: u32,
        cell_m: f64,
        walk_speed: f64,
    ) -> Result<String, JsValue> {
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(err)?;
        let depart = parse_time(time).map_err(err)?.ok_or_else(|| err("time is required"))?;
        let opts = QueryOpts { date, depart, max_secs: max_min * 60, walk_speed_mps: walk_speed, ..Default::default() };
        if !self.query.run(lat, lon, &opts) {
            return Err(err("origin is too far from the walking network"));
        }
        let stops = self.query.reached_stops().filter(|s| s.2 > 0).count();
        let iso = IsochroneOpts { cell_m, band_secs: band_min * 60, max_secs: max_min * 60, ..Default::default() };
        let mut fc = match Grid::from_query(&self.query, &iso) {
            Some(grid) => grid.isobands(&iso).map_err(err)?,
            None => json!({ "type": "FeatureCollection", "features": [] }),
        };
        fc["properties"] = json!({ "stops_by_transit": stops, "nodes": self.query.reached_node_count() });
        Ok(fc.to_string())
    }
}
