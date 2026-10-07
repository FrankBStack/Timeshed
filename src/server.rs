//! HTTP API and static file host for the live map.

use crate::engine::{Engine, Query, QueryOpts};
use crate::gtfs::parse_time;
use crate::isochrone::{Grid, IsochroneOpts};
use anyhow::Result;
use axum::extract::{Query as Params, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::json;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

/// The engine lives for the whole process; leaking it lets query
/// workspaces (which borrow it) live in thread locals.
#[derive(Clone, Copy)]
struct App {
    engine: &'static Engine,
}

thread_local! {
    static WORKSPACE: RefCell<Option<Query<'static>>> = const { RefCell::new(None) };
}

fn with_query<R>(engine: &'static Engine, f: impl FnOnce(&mut Query<'static>) -> R) -> R {
    WORKSPACE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let q = slot.get_or_insert_with(|| Query::new(engine));
        f(q)
    })
}

pub async fn serve(engine: Engine, web_dir: PathBuf, addr: SocketAddr) -> Result<()> {
    let engine: &'static Engine = Box::leak(Box::new(engine));
    let app = Router::new()
        .route("/api/info", get(info))
        .route("/api/isochrone", get(isochrone))
        .fallback_service(ServeDir::new(web_dir).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .with_state(App { engine });
    log::info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn info(State(app): State<App>) -> Json<serde_json::Value> {
    let e = app.engine;
    let b = e.tt.bbox();
    let (first, last) = e.tt.services.date_range().unwrap_or((NaiveDate::MIN, NaiveDate::MIN));
    Json(json!({
        "name": e.name,
        "stops": e.tt.stops.len(),
        "routes": e.tt.route_info.len(),
        "walk_nodes": e.walk.node_count(),
        "bbox": [b.min_lon, b.min_lat, b.max_lon, b.max_lat],
        "center": [b.mid_lon(), b.mid_lat()],
        "first_date": first.to_string(),
        "last_date": last.to_string(),
    }))
}

#[derive(Deserialize)]
struct IsoParams {
    lat: f64,
    lon: f64,
    date: NaiveDate,
    /// HH:MM
    time: String,
    /// minutes
    #[serde(default = "default_max")]
    max: u32,
    /// band width in minutes
    #[serde(default = "default_band")]
    band: u32,
    #[serde(default = "default_cell")]
    cell: f64,
    #[serde(default = "default_speed")]
    walk_speed: f64,
}

fn default_max() -> u32 {
    60
}
fn default_band() -> u32 {
    5
}
fn default_cell() -> f64 {
    100.0
}
fn default_speed() -> f64 {
    1.3
}

type ApiError = (StatusCode, String);

fn bad(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.into())
}

async fn isochrone(State(app): State<App>, Params(p): Params<IsoParams>) -> Result<impl IntoResponse, ApiError> {
    let depart = parse_time(&p.time).map_err(|e| bad(e.to_string()))?.ok_or_else(|| bad("time is required"))?;
    if !(1..=120).contains(&p.max) || !(1..=60).contains(&p.band) || !(30.0..=1000.0).contains(&p.cell) {
        return Err(bad("max must be 1..120 minutes, band 1..60, cell 30..1000 m"));
    }
    let opts = QueryOpts {
        date: p.date,
        depart,
        max_secs: p.max * 60,
        walk_speed_mps: p.walk_speed,
        ..Default::default()
    };
    let iso = IsochroneOpts { cell_m: p.cell, band_secs: p.band * 60, max_secs: p.max * 60, ..Default::default() };
    let engine = app.engine;
    let result = tokio::task::spawn_blocking(move || {
        with_query(engine, |q| {
            let t0 = std::time::Instant::now();
            if !q.run(p.lat, p.lon, &opts) {
                return Err(bad("origin is too far from the walking network"));
            }
            let query_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let stops = q.reached_stops().filter(|s| s.2 > 0).count();
            let mut fc = match Grid::from_query(q, &iso) {
                Some(grid) => grid.isobands(&iso).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
                None => json!({ "type": "FeatureCollection", "features": [] }),
            };
            fc["properties"] = json!({
                "query_ms": (query_ms * 10.0).round() / 10.0,
                "total_ms": (t0.elapsed().as_secs_f64() * 10000.0).round() / 10.0,
                "stops_by_transit": stops,
                "nodes": q.reached_node_count(),
            });
            Ok(fc)
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(result))
}
