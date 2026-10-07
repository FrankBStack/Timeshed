//! Small geographic helpers. Everything is WGS84 degrees in, meters out.

use serde::{Deserialize, Serialize};

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Great-circle distance in meters.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl BBox {
    pub fn empty() -> BBox {
        BBox { min_lon: f64::MAX, min_lat: f64::MAX, max_lon: f64::MIN, max_lat: f64::MIN }
    }

    pub fn include(&mut self, lon: f64, lat: f64) {
        self.min_lon = self.min_lon.min(lon);
        self.min_lat = self.min_lat.min(lat);
        self.max_lon = self.max_lon.max(lon);
        self.max_lat = self.max_lat.max(lat);
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        lon >= self.min_lon && lon <= self.max_lon && lat >= self.min_lat && lat <= self.max_lat
    }

    /// Grow on every side by roughly `meters`.
    pub fn buffer(&self, meters: f64) -> BBox {
        let dlat = meters / 111_320.0;
        let dlon = meters / (111_320.0 * self.mid_lat().to_radians().cos());
        BBox {
            min_lon: self.min_lon - dlon,
            min_lat: self.min_lat - dlat,
            max_lon: self.max_lon + dlon,
            max_lat: self.max_lat + dlat,
        }
    }

    pub fn mid_lat(&self) -> f64 {
        (self.min_lat + self.max_lat) / 2.0
    }

    pub fn mid_lon(&self) -> f64 {
        (self.min_lon + self.max_lon) / 2.0
    }

    /// "min_lon,min_lat,max_lon,max_lat"
    pub fn parse(s: &str) -> Option<BBox> {
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().ok()).collect::<Option<Vec<_>>>()?;
        if v.len() != 4 {
            return None;
        }
        Some(BBox { min_lon: v[0], min_lat: v[1], max_lon: v[2], max_lat: v[3] })
    }
}

/// A local flat projection (equirectangular) around a reference latitude.
/// Good to well under 1% over a metro area, and keeps spatial indexing cheap.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct LocalProj {
    lat0: f64,
    lon0: f64,
    cos_lat0: f64,
}

impl LocalProj {
    pub fn new(lat0: f64, lon0: f64) -> LocalProj {
        LocalProj { lat0, lon0, cos_lat0: lat0.to_radians().cos() }
    }

    /// (x, y) in meters east/north of the reference point.
    pub fn to_xy(&self, lat: f64, lon: f64) -> [f64; 2] {
        [(lon - self.lon0) * 111_320.0 * self.cos_lat0, (lat - self.lat0) * 111_320.0]
    }

    pub fn to_lat_lon(&self, xy: [f64; 2]) -> (f64, f64) {
        (self.lat0 + xy[1] / 111_320.0, self.lon0 + xy[0] / (111_320.0 * self.cos_lat0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_milwaukee_to_chicago() {
        // city hall to city hall, about 131 km
        let d = haversine_m(43.0417, -87.9094, 41.8837, -87.6320);
        assert!((d - 131_000.0).abs() < 2_000.0, "{d}");
    }

    #[test]
    fn local_projection_roundtrips() {
        let p = LocalProj::new(43.0, -87.9);
        let xy = p.to_xy(43.05, -87.95);
        let (lat, lon) = p.to_lat_lon(xy);
        assert!((lat - 43.05).abs() < 1e-9 && (lon + 87.95).abs() < 1e-9);
        // and the metric is close to haversine over a few km
        let d = (xy[0].powi(2) + xy[1].powi(2)).sqrt();
        let h = haversine_m(43.0, -87.9, 43.05, -87.95);
        assert!((d - h).abs() / h < 0.005, "{d} vs {h}");
    }
}
