#!/usr/bin/env python3
"""Clip an OSM PBF to a bounding box, keeping ways that touch it.

Only needed for tools that cannot clip themselves (r5 builds a street
network for the whole file it is given). Timeshed clips on its own.
"""
import argparse

import osmium


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("dst")
    ap.add_argument("--bbox", required=True, help="min_lon,min_lat,max_lon,max_lat")
    args = ap.parse_args()
    min_lon, min_lat, max_lon, max_lat = (float(x) for x in args.bbox.split(","))

    print("pass 1: nodes in box")
    inside = set()
    for node in osmium.FileProcessor(args.src, osmium.osm.NODE):
        loc = node.location
        if min_lon <= loc.lon <= max_lon and min_lat <= loc.lat <= max_lat:
            inside.add(node.id)
    print(f"  {len(inside):,} nodes")

    print("pass 2: ways touching the box, plus the nodes they need")
    keep_nodes = set()
    way_count = 0
    for way in osmium.FileProcessor(args.src, osmium.osm.WAY):
        refs = [n.ref for n in way.nodes]
        if any(r in inside for r in refs):
            keep_nodes.update(refs)
            way_count += 1
    print(f"  {way_count:,} ways, {len(keep_nodes):,} nodes")

    print("pass 3: write")
    writer = osmium.SimpleWriter(args.dst)
    for node in osmium.FileProcessor(args.src, osmium.osm.NODE):
        if node.id in keep_nodes:
            writer.add_node(node)
    for way in osmium.FileProcessor(args.src, osmium.osm.WAY):
        if any(n.ref in inside for n in way.nodes):
            writer.add_way(way)
    writer.close()
    print(f"wrote {args.dst}")


if __name__ == "__main__":
    main()
