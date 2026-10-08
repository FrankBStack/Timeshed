//! The walking graph and how to search it: a spatial index for snapping
//! points to nodes, and a reusable bounded Dijkstra.

use crate::geo::{BBox, LocalProj};
use rstar::{RTree, primitives::GeomWithData};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Serialize, Deserialize, Clone)]
pub struct WalkGraph {
    pub lat: Vec<f64>,
    pub lon: Vec<f64>,
    pub osm_id: Vec<i64>,
    /// CSR adjacency: neighbours of node `n` are `targets[offsets[n]..offsets[n+1]]`
    pub offsets: Vec<u32>,
    pub targets: Vec<u32>,
    /// edge length in meters, parallel to `targets`
    pub lengths: Vec<f32>,
    pub bbox: BBox,
}

impl WalkGraph {
    pub fn node_count(&self) -> usize {
        self.lat.len()
    }

    pub fn edge_count(&self) -> usize {
        self.targets.len()
    }

    #[inline]
    pub fn neighbours(&self, n: u32) -> impl Iterator<Item = (u32, f32)> + '_ {
        let (a, b) = (self.offsets[n as usize] as usize, self.offsets[n as usize + 1] as usize);
        self.targets[a..b].iter().copied().zip(self.lengths[a..b].iter().copied())
    }
}


type NodePoint = GeomWithData<[f64; 2], u32>;

/// R-tree over graph nodes in a local metric projection.
pub struct WalkIndex {
    pub proj: LocalProj,
    tree: RTree<NodePoint>,
}

impl WalkIndex {
    pub fn build(g: &WalkGraph) -> WalkIndex {
        let proj = LocalProj::new(g.bbox.mid_lat(), g.bbox.mid_lon());
        let pts: Vec<NodePoint> = (0..g.node_count())
            .map(|i| GeomWithData::new(proj.to_xy(g.lat[i], g.lon[i]), i as u32))
            .collect();
        WalkIndex { proj, tree: RTree::bulk_load(pts) }
    }

    /// Nearest node and its distance in meters, if within `max_m`.
    pub fn nearest(&self, lat: f64, lon: f64, max_m: f64) -> Option<(u32, f64)> {
        let p = self.proj.to_xy(lat, lon);
        let n = self.tree.nearest_neighbor(p)?;
        let d = dist(n.geom(), &p);
        (d <= max_m).then_some((n.data, d))
    }

    /// Every node within `radius_m`, with distances.
    pub fn within(&self, lat: f64, lon: f64, radius_m: f64) -> impl Iterator<Item = (u32, f64)> + '_ {
        let p = self.proj.to_xy(lat, lon);
        self.tree.locate_within_distance(p, radius_m * radius_m).map(move |n| (n.data, dist(n.geom(), &p)))
    }
}

fn dist(a: &[f64; 2], b: &[f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

pub const UNREACHED: u32 = u32::MAX;

/// Bounded multi-source Dijkstra with reusable buffers, so a batch of
/// queries does not reallocate a node-sized array every time.
pub struct WalkSearch<'a> {
    g: &'a WalkGraph,
    label: Vec<u32>,
    touched: Vec<u32>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl<'a> WalkSearch<'a> {
    pub fn new(g: &'a WalkGraph) -> WalkSearch<'a> {
        WalkSearch { g, label: vec![UNREACHED; g.node_count()], touched: Vec::new(), heap: BinaryHeap::new() }
    }

    pub fn reset(&mut self) {
        for &n in &self.touched {
            self.label[n as usize] = UNREACHED;
        }
        self.touched.clear();
        self.heap.clear();
    }

    /// Run from `sources` (node, starting label), never expanding labels
    /// above `limit`. Labels are absolute: they start where the source says.
    /// `pace` is seconds per meter; pass 1.0 to search in meters.
    pub fn run(&mut self, sources: impl IntoIterator<Item = (u32, u32)>, limit: u32, pace: f64) {
        self.reset();
        for (n, t) in sources {
            if t <= limit && t < self.label[n as usize] {
                if self.label[n as usize] == UNREACHED {
                    self.touched.push(n);
                }
                self.label[n as usize] = t;
                self.heap.push(Reverse((t, n)));
            }
        }
        while let Some(Reverse((t, n))) = self.heap.pop() {
            if t > self.label[n as usize] {
                continue; // stale entry
            }
            for (m, len) in self.g.neighbours(n) {
                let t2 = t + (len as f64 * pace).round() as u32;
                if t2 <= limit && t2 < self.label[m as usize] {
                    if self.label[m as usize] == UNREACHED {
                        self.touched.push(m);
                    }
                    self.label[m as usize] = t2;
                    self.heap.push(Reverse((t2, m)));
                }
            }
        }
    }

    #[inline]
    pub fn label(&self, n: u32) -> Option<u32> {
        let t = self.label[n as usize];
        (t != UNREACHED).then_some(t)
    }

    /// Every (node, label) reached by the last run, in no particular order.
    pub fn reached(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.touched.iter().map(move |&n| (n, self.label[n as usize]))
    }

    pub fn reached_count(&self) -> usize {
        self.touched.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight line of 5 nodes 100 m apart, plus one node 10 km away.
    fn line_graph() -> WalkGraph {
        let lat: Vec<f64> = (0..5).map(|i| 43.0 + i as f64 * 0.0009).chain([43.1]).collect();
        let lon = vec![-87.9; 6];
        let mut offsets = vec![0u32];
        let mut targets = Vec::new();
        let mut lengths = Vec::new();
        for i in 0..6u32 {
            if i > 0 && i < 5 {
                targets.push(i - 1);
                lengths.push(100.0);
            }
            if i + 1 < 5 {
                targets.push(i + 1);
                lengths.push(100.0);
            }
            offsets.push(targets.len() as u32);
        }
        let mut bbox = BBox::empty();
        for i in 0..6 {
            bbox.include(lon[i], lat[i]);
        }
        WalkGraph { lat, lon, osm_id: (0..6).collect(), offsets, targets, lengths, bbox }
    }

    #[test]
    fn bounded_dijkstra() {
        let g = line_graph();
        let mut s = WalkSearch::new(&g);
        s.run([(0, 0)], 250, 1.0);
        assert_eq!(s.label(0), Some(0));
        assert_eq!(s.label(2), Some(200));
        assert_eq!(s.label(3), None, "300 m is past the limit");
        assert_eq!(s.reached_count(), 3);
        // multi-source with offsets, and a reused workspace
        s.run([(4, 1000), (0, 1050)], 10_000, 1.0);
        assert_eq!(s.label(2), Some(1200));
        assert_eq!(s.label(1), Some(1150));
        assert_eq!(s.label(5), None, "unconnected");
    }

    #[test]
    fn snapping() {
        let g = line_graph();
        let idx = WalkIndex::build(&g);
        let (n, d) = idx.nearest(43.0018, -87.9001, 100.0).unwrap();
        assert_eq!(n, 2);
        assert!(d < 15.0, "{d}");
        assert!(idx.nearest(43.05, -87.9, 100.0).is_none());
        assert_eq!(idx.within(43.0, -87.9, 150.0).count(), 2);
    }
}
