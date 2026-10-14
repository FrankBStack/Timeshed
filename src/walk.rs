//! The walking graph and how to search it: a spatial index for snapping
//! points to nodes, and a reusable bounded Dijkstra.

use crate::geo::{BBox, LocalProj};
use rstar::{RTree, primitives::GeomWithData};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Serialize, Deserialize, Clone)]
pub struct WalkGraph {
    /// f32 is about half a meter at this latitude, and half the bundle size
    pub lat: Vec<f32>,
    pub lon: Vec<f32>,
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
    pub fn lat_lon(&self, n: u32) -> (f64, f64) {
        (self.lat[n as usize] as f64, self.lon[n as usize] as f64)
    }

    #[inline]
    pub fn degree(&self, n: u32) -> usize {
        (self.offsets[n as usize + 1] - self.offsets[n as usize]) as usize
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
        let pts: Vec<NodePoint> = (0..g.node_count() as u32)
            .map(|i| {
                let (lat, lon) = g.lat_lon(i);
                GeomWithData::new(proj.to_xy(lat, lon), i)
            })
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

    /// Attach a point to the walking network at the nearest point on the
    /// nearest edge, not just the nearest node: a point halfway along a
    /// 150 m block should enter the graph 75 m from either end, not 75 m
    /// plus a detour. Looks at the edges of the closest few nodes. None if
    /// nothing is within `max_m`.
    pub fn snap(&self, g: &WalkGraph, lat: f64, lon: f64, max_m: f64) -> Option<Snap> {
        let p = self.proj.to_xy(lat, lon);
        let mut best: Option<Snap> = None;
        let mut consider = |s: Snap| {
            if best.as_ref().is_none_or(|b| s.off_m < b.off_m) {
                best = Some(s);
            }
        };
        for (k, n) in self.tree.nearest_neighbor_iter(p).enumerate() {
            let d_node = dist(n.geom(), &p);
            if d_node > max_m || k >= 8 {
                break;
            }
            let u = n.data;
            let a = *n.geom();
            consider(Snap { a: u, b: u, to_a_m: 0.0, to_b_m: 0.0, off_m: d_node });
            for (v, len) in g.neighbours(u) {
                let (vlat, vlon) = g.lat_lon(v);
                let b = self.proj.to_xy(vlat, vlon);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let l2 = dx * dx + dy * dy;
                if l2 < 1e-6 {
                    continue;
                }
                let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0);
                let foot = [a[0] + t * dx, a[1] + t * dy];
                let off = dist(&foot, &p);
                consider(Snap { a: u, b: v, to_a_m: t * len as f64, to_b_m: (1.0 - t) * len as f64, off_m: off });
            }
        }
        best
    }
}

/// Where a point attaches to the network: on the edge between `a` and `b`,
/// `to_a_m` along the edge from the foot of the perpendicular to `a`,
/// `to_b_m` to `b`, and `off_m` from the point to the foot. A point snapped
/// to a node has `a == b` and both distances zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    pub a: u32,
    pub b: u32,
    pub to_a_m: f64,
    pub to_b_m: f64,
    pub off_m: f64,
}

impl Snap {
    /// The two (node, meters) anchors a search can start from.
    pub fn anchors(&self) -> [(u32, f64); 2] {
        [(self.a, self.to_a_m + self.off_m), (self.b, self.to_b_m + self.off_m)]
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
    /// node we came from, or UNREACHED at a source
    prev: Vec<u32>,
    /// the source node whose label this one descends from
    seed: Vec<u32>,
    touched: Vec<u32>,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl<'a> WalkSearch<'a> {
    pub fn new(g: &'a WalkGraph) -> WalkSearch<'a> {
        let n = g.node_count();
        WalkSearch { g, label: vec![UNREACHED; n], prev: vec![UNREACHED; n], seed: vec![UNREACHED; n], touched: Vec::new(), heap: BinaryHeap::new() }
    }

    pub fn reset(&mut self) {
        for &n in &self.touched {
            self.label[n as usize] = UNREACHED;
            self.prev[n as usize] = UNREACHED;
            self.seed[n as usize] = UNREACHED;
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
                self.prev[n as usize] = UNREACHED;
                self.seed[n as usize] = n;
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
                    self.prev[m as usize] = n;
                    self.seed[m as usize] = self.seed[n as usize];
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

    /// The source node a reached node's label descends from.
    #[inline]
    pub fn seed_of(&self, n: u32) -> Option<u32> {
        let s = self.seed[n as usize];
        (s != UNREACHED).then_some(s)
    }

    /// The walk from a node's source to the node, as node ids; empty if unreached.
    pub fn path_to(&self, n: u32) -> Vec<u32> {
        let mut path = Vec::new();
        if self.label(n).is_none() {
            return path;
        }
        let mut cur = n;
        while cur != UNREACHED {
            path.push(cur);
            cur = self.prev[cur as usize];
        }
        path.reverse();
        path
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
        let lat: Vec<f32> = (0..5).map(|i| 43.0 + i as f32 * 0.0009).chain([43.1]).collect();
        let lon = vec![-87.9f32; 6];
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
            bbox.include(lon[i] as f64, lat[i] as f64);
        }
        WalkGraph { lat, lon, offsets, targets, lengths, bbox }
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
        // paths and seeds: node 2 was reached from source 4 (1000+200 < 1050+200)
        assert_eq!(s.seed_of(2), Some(4));
        assert_eq!(s.path_to(2), vec![4, 3, 2]);
        assert_eq!(s.seed_of(1), Some(0));
        assert_eq!(s.path_to(1), vec![0, 1]);
        assert!(s.path_to(5).is_empty());
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

    #[test]
    fn snaps_to_the_nearest_point_on_an_edge() {
        // a point beside the middle of the 1-2 edge (nodes are 100 m apart
        // along the line, so the midpoint is 50 m from each) and 20 m off it
        let g = line_graph();
        let idx = WalkIndex::build(&g);
        let mid_lat = (g.lat[1] + g.lat[2]) as f64 / 2.0;
        let s = idx.snap(&g, mid_lat, -87.9 - 20.0 / (111_320.0 * (43.0f64).to_radians().cos()), 100.0).unwrap();
        let (a, b) = if s.a < s.b { (s.a, s.b) } else { (s.b, s.a) };
        assert_eq!((a, b), (1, 2), "{s:?}");
        assert!((s.to_a_m - 50.0).abs() < 2.0 && (s.to_b_m - 50.0).abs() < 2.0, "{s:?}");
        assert!((s.off_m - 20.0).abs() < 1.0, "{s:?}");
        let [(na, ma), (nb, mb)] = s.anchors();
        assert!(na != nb && (ma - 70.0).abs() < 2.0 && (mb - 70.0).abs() < 2.0);
        // far away: nothing
        assert!(idx.snap(&g, 44.0, -87.9, 100.0).is_none());
        // a point right on node 0 snaps to it with zero along-edge distance
        let s0 = idx.snap(&g, g.lat[0] as f64, g.lon[0] as f64, 100.0).unwrap();
        assert!(s0.to_a_m < 1.0 && s0.off_m < 1.0, "{s0:?}");
    }
}
