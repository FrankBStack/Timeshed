//! Walking graph extracted from an OpenStreetMap PBF file. The graph type
//! itself lives in `walk`; this module only knows how to build one.
//!
//! The extract is read twice: once to pick up the coordinates of every node
//! inside the bounding box, then once more for ways, keeping only the ones a
//! pedestrian can use. Edges are bidirectional, weighted by length in meters.

use crate::geo::{BBox, haversine_m};
use crate::walk::WalkGraph;
#[cfg(test)]
use crate::walk::WalkSearch;
use anyhow::{Context, Result};
use osmpbf::{Element, ElementReader};
use std::collections::HashMap;
use std::path::Path;

/// Can a pedestrian use this way? Tags are (key, value) pairs.
pub fn is_walkable<'a>(tags: impl Iterator<Item = (&'a str, &'a str)>) -> bool {
    let mut highway = None;
    let mut foot = None;
    let mut access = None;
    let mut sidewalk = None;
    for (k, v) in tags {
        match k {
            "highway" => highway = Some(v),
            "foot" => foot = Some(v),
            "access" => access = Some(v),
            "sidewalk" | "sidewalk:both" | "sidewalk:left" | "sidewalk:right"
                if v != "no" && v != "none" => {
                    sidewalk = Some(v)
                }
            _ => {}
        }
    }
    let Some(highway) = highway else { return false };
    let foot_ok = matches!(foot, Some("yes" | "designated" | "permissive"));
    if matches!(foot, Some("no" | "private")) {
        return false;
    }
    if matches!(access, Some("no" | "private")) && !foot_ok {
        return false;
    }
    match highway {
        "footway" | "path" | "pedestrian" | "steps" | "living_street" | "residential" | "service"
        | "unclassified" | "tertiary" | "tertiary_link" | "secondary" | "secondary_link" | "primary"
        | "primary_link" | "track" | "road" | "corridor" | "elevator" | "crossing" | "platform" => true,
        "cycleway" | "bridleway" => foot_ok || foot.is_none(),
        "trunk" | "trunk_link" => foot_ok || sidewalk.is_some(),
        _ => false,
    }
}

/// Read the walking network inside `bbox`. Runs of shape nodes are merged
/// into single edges no longer than `max_edge_m`, which keeps isochrones
/// smooth while dropping most of the nodes.
pub fn read_walk_graph(path: &Path, bbox: BBox, max_edge_m: f32) -> Result<WalkGraph> {
    // pass 1: coordinates of every node in the box
    let t0 = std::time::Instant::now();
    let reader = ElementReader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    let coords: HashMap<i64, (f64, f64)> = reader.par_map_reduce(
        |el| {
            let mut out = Vec::new();
            let (id, lat, lon) = match el {
                Element::Node(n) => (n.id(), n.lat(), n.lon()),
                Element::DenseNode(n) => (n.id(), n.lat(), n.lon()),
                _ => return out,
            };
            if bbox.contains(lon, lat) {
                out.push((id, (lat, lon)));
            }
            out
        },
        Vec::new,
        |mut a, b| {
            a.extend(b);
            a
        },
    )?
    .into_iter()
    .collect();
    log::info!("{} nodes inside bbox ({:.1?})", coords.len(), t0.elapsed());

    // pass 2: walkable ways, keeping only refs we have coordinates for
    let t1 = std::time::Instant::now();
    let reader = ElementReader::from_path(path)?;
    let ways: Vec<Vec<i64>> = reader.par_map_reduce(
        |el| {
            let mut out = Vec::new();
            if let Element::Way(w) = el
                && is_walkable(w.tags()) {
                    // A way leaving the box is cut at the box edge: runs of
                    // in-box nodes become separate segments.
                    let mut run: Vec<i64> = Vec::new();
                    for r in w.refs() {
                        if coords.contains_key(&r) {
                            run.push(r);
                        } else if run.len() >= 2 {
                            out.push(std::mem::take(&mut run));
                        } else {
                            run.clear();
                        }
                    }
                    if run.len() >= 2 {
                        out.push(run);
                    }
                }
            out
        },
        Vec::new,
        |mut a, b| {
            a.extend(b);
            a
        },
    )?;
    log::info!("{} walkable way segments ({:.1?})", ways.len(), t1.elapsed());

    // number the nodes that ways actually use
    let mut index: HashMap<i64, u32> = HashMap::new();
    let mut lat: Vec<f32> = Vec::new();
    let mut lon: Vec<f32> = Vec::new();
    let mut edges: Vec<(u32, u32, f32)> = Vec::new();
    for way in &ways {
        let mut prev: Option<(u32, (f64, f64))> = None;
        for &r in way {
            let here = coords[&r];
            let n = *index.entry(r).or_insert_with(|| {
                lat.push(here.0 as f32);
                lon.push(here.1 as f32);
                (lat.len() - 1) as u32
            });
            if let Some((p, there)) = prev
                && p != n
            {
                // lengths from the full-precision coordinates
                let d = haversine_m(there.0, there.1, here.0, here.1) as f32;
                edges.push((p, n, d));
                edges.push((n, p, d));
            }
            prev = Some((n, here));
        }
    }
    drop(coords);

    let mut g = build_csr(lat, lon, edges, bbox);
    let raw = g.node_count();
    g = largest_component(&g);
    let connected = g.node_count();
    g = contract(&g, max_edge_m);
    log::info!(
        "walk graph: {raw} nodes read, {} dropped as disconnected, {} merged into longer edges; {} nodes, {} edges left",
        raw - connected,
        connected - g.node_count(),
        g.node_count(),
        g.edge_count()
    );
    Ok(g)
}

fn build_csr(lat: Vec<f32>, lon: Vec<f32>, mut edges: Vec<(u32, u32, f32)>, bbox: BBox) -> WalkGraph {
    edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(a.2.total_cmp(&b.2)));
    edges.dedup_by_key(|e| (e.0, e.1)); // keeps the shortest of parallel edges
    let n = lat.len();
    let mut offsets = vec![0u32; n + 1];
    for e in &edges {
        offsets[e.0 as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let targets = edges.iter().map(|e| e.1).collect();
    let lengths = edges.iter().map(|e| e.2).collect();
    WalkGraph { lat, lon, offsets, targets, lengths, bbox }
}

/// Keep only the largest connected component. Everything else is a park
/// path or a stub that would otherwise trap snapped points.
fn largest_component(g: &WalkGraph) -> WalkGraph {
    let n = g.node_count();
    let mut comp = vec![u32::MAX; n];
    let mut sizes: Vec<usize> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n as u32 {
        if comp[start as usize] != u32::MAX {
            continue;
        }
        let c = sizes.len() as u32;
        sizes.push(0);
        stack.push(start);
        comp[start as usize] = c;
        while let Some(v) = stack.pop() {
            sizes[c as usize] += 1;
            for (w, _) in g.neighbours(v) {
                if comp[w as usize] == u32::MAX {
                    comp[w as usize] = c;
                    stack.push(w);
                }
            }
        }
    }
    let Some((big, _)) = sizes.iter().enumerate().max_by_key(|(_, s)| **s) else {
        return g.clone();
    };
    let big = big as u32;
    keep_nodes(g, |v| comp[v as usize] == big)
}

/// Rebuild the graph with only the nodes `keep` accepts and the edges
/// between them.
fn keep_nodes(g: &WalkGraph, keep: impl Fn(u32) -> bool) -> WalkGraph {
    let n = g.node_count();
    let mut remap = vec![u32::MAX; n];
    let mut lat = Vec::new();
    let mut lon = Vec::new();
    for v in 0..n as u32 {
        if keep(v) {
            remap[v as usize] = lat.len() as u32;
            lat.push(g.lat[v as usize]);
            lon.push(g.lon[v as usize]);
        }
    }
    let mut edges = Vec::new();
    for v in 0..n as u32 {
        if remap[v as usize] == u32::MAX {
            continue;
        }
        for (w, d) in g.neighbours(v) {
            if remap[w as usize] != u32::MAX {
                edges.push((remap[v as usize], remap[w as usize], d));
            }
        }
    }
    build_csr(lat, lon, edges, g.bbox)
}

/// Merge every node that only joins two edges into its neighbours, as long
/// as the merged edge stays under `max_edge_m`. Most OSM nodes are shape
/// points on curves; walking distances are unchanged.
fn contract(g: &WalkGraph, max_edge_m: f32) -> WalkGraph {
    let n = g.node_count();
    let mut adj: Vec<Vec<(u32, f32)>> = (0..n as u32).map(|v| g.neighbours(v).collect()).collect();
    let mut removed = vec![false; n];
    let mut changed = true;
    while changed {
        changed = false;
        for v in 0..n as u32 {
            let vi = v as usize;
            if removed[vi] || adj[vi].len() != 2 {
                continue;
            }
            let ((a, la), (b, lb)) = (adj[vi][0], adj[vi][1]);
            if a == b || a == v || b == v || la + lb > max_edge_m {
                continue;
            }
            let merged = la + lb;
            // a now connects to b instead of v, and b to a; keep the shorter
            // edge if a parallel one already exists
            for (from, new_to) in [(a, b), (b, a)] {
                let list = &mut adj[from as usize];
                list.retain(|&(to, _)| to != v);
                match list.iter_mut().find(|(to, _)| *to == new_to) {
                    Some(e) => e.1 = e.1.min(merged),
                    None => list.push((new_to, merged)),
                }
            }
            adj[vi].clear();
            removed[vi] = true;
            changed = true;
        }
    }
    let mut edges = Vec::new();
    for v in 0..n as u32 {
        for &(w, d) in &adj[v as usize] {
            edges.push((v, w, d));
        }
    }
    let full = build_csr(g.lat.clone(), g.lon.clone(), edges, g.bbox);
    keep_nodes(&full, |v| !removed[v as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walkable(tags: &[(&str, &str)]) -> bool {
        is_walkable(tags.iter().copied())
    }

    #[test]
    fn classifies_ways() {
        assert!(walkable(&[("highway", "residential")]));
        assert!(walkable(&[("highway", "footway"), ("footway", "sidewalk")]));
        assert!(walkable(&[("highway", "cycleway")]));
        assert!(!walkable(&[("highway", "cycleway"), ("foot", "no")]));
        assert!(!walkable(&[("highway", "motorway")]));
        assert!(!walkable(&[("highway", "trunk")]));
        assert!(walkable(&[("highway", "trunk"), ("sidewalk", "both")]));
        assert!(!walkable(&[("highway", "service"), ("access", "private")]));
        assert!(walkable(&[("highway", "service"), ("access", "private"), ("foot", "yes")]));
        assert!(!walkable(&[("building", "yes")]));
    }

    #[test]
    fn keeps_largest_component() {
        // triangle 0-1-2 plus an isolated pair 3-4
        let lat = vec![43.0, 43.001, 43.002, 44.0, 44.001];
        let lon = vec![-87.9; 5];
        let edges = vec![(0, 1, 1.0), (1, 0, 1.0), (1, 2, 1.0), (2, 1, 1.0), (3, 4, 1.0), (4, 3, 1.0)];
        let g = build_csr(lat, lon, edges, BBox::empty());
        let g = largest_component(&g);
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 4);
        assert_eq!(g.lat, vec![43.0, 43.001, 43.002]);
    }

    #[test]
    fn contracts_shape_nodes_but_keeps_distances() {
        // a path 0-1-2-3-4 with 40 m hops; node 2 also has a spur to 5
        let lat = vec![43.0; 6];
        let lon = vec![-87.9, -87.8995, -87.899, -87.8985, -87.898, -87.899];
        let mut edges = Vec::new();
        for (a, b, d) in [(0, 1, 40.0), (1, 2, 40.0), (2, 3, 40.0), (3, 4, 40.0), (2, 5, 10.0)] {
            edges.push((a, b, d));
            edges.push((b, a, d));
        }
        let g = build_csr(lat, lon, edges, BBox::empty());
        let c = contract(&g, 100.0);
        // 1 and 3 are shape nodes and go; 2 has degree 3 and stays; ends stay
        assert_eq!(c.node_count(), 4);
        let mut s = WalkSearch::new(&c);
        s.run([(0, 0)], 1000, 1.0);
        // nodes 0,2,4,5 become 0,1,2,3
        assert_eq!(s.label(2), Some(160));
        assert_eq!(s.label(3), Some(90));
        // with a 50 m cap nothing merges (each merge would be 80 m)
        assert_eq!(contract(&g, 50.0).node_count(), 6);
    }
}
