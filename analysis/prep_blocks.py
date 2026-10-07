#!/usr/bin/env python3
"""Build the origin and destination tables for the accessibility run.

Origins: every inhabited 2020 census block in the study county (one row
per block, located at its internal point).
Destinations: every block inside the walking bbox with at least one job
in LODES, weighted by job counts.

Inputs are the raw TIGER block shapefile zip and the LODES8 WAC/RAC csvs.
"""
import argparse
from pathlib import Path

import geopandas as gpd
import pandas as pd


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--blocks", required=True, help="tl_2020_SS_tabblock20.zip")
    ap.add_argument("--wac", required=True, help="LODES WAC csv.gz (jobs by workplace block)")
    ap.add_argument("--rac", required=True, help="LODES RAC csv.gz (workers by home block)")
    ap.add_argument("--county", required=True, help="3-digit county FIPS for origins, e.g. 079")
    ap.add_argument("--bbox", required=True, help="min_lon,min_lat,max_lon,max_lat (destinations)")
    ap.add_argument("--out", required=True, type=Path, help="output directory")
    args = ap.parse_args()

    bbox = tuple(float(x) for x in args.bbox.split(","))
    args.out.mkdir(parents=True, exist_ok=True)

    print("reading blocks...")
    blocks = gpd.read_file(f"zip://{args.blocks}", bbox=bbox, engine="pyogrio")
    blocks = blocks[["GEOID20", "COUNTYFP20", "POP20", "HOUSING20", "ALAND20", "AWATER20",
                     "INTPTLAT20", "INTPTLON20", "geometry"]].copy()
    blocks["lat"] = blocks["INTPTLAT20"].astype(float)
    blocks["lon"] = blocks["INTPTLON20"].astype(float)
    inside = blocks["lon"].between(bbox[0], bbox[2]) & blocks["lat"].between(bbox[1], bbox[3])
    blocks = blocks[inside].drop(columns=["INTPTLAT20", "INTPTLON20"])
    print(f"  {len(blocks)} blocks in bbox")

    print("reading LODES...")
    wac = pd.read_csv(args.wac, dtype={"w_geocode": str},
                      usecols=["w_geocode", "C000", "CE01", "CE02", "CE03", "CNS16", "CNS18"])
    wac = wac.rename(columns={"w_geocode": "GEOID20", "C000": "jobs",
                              "CE01": "jobs_low_wage", "CE02": "jobs_mid_wage", "CE03": "jobs_high_wage",
                              "CNS16": "jobs_health", "CNS18": "jobs_food"})
    rac = pd.read_csv(args.rac, dtype={"h_geocode": str}, usecols=["h_geocode", "C000", "CE01"])
    rac = rac.rename(columns={"h_geocode": "GEOID20", "C000": "workers", "CE01": "workers_low_wage"})
    blocks = blocks.merge(wac, on="GEOID20", how="left").merge(rac, on="GEOID20", how="left")
    for c in wac.columns[1:].tolist() + rac.columns[1:].tolist():
        blocks[c] = blocks[c].fillna(0).astype(int)

    county = blocks[blocks["COUNTYFP20"] == args.county]
    origins = county[county["POP20"] > 0][["GEOID20", "lat", "lon"]].rename(columns={"GEOID20": "id"})
    origins.to_csv(args.out / "origins.csv", index=False)
    print(f"  {len(origins)} inhabited origin blocks in county {args.county} "
          f"(pop {county['POP20'].sum():,}, resident workers {county['workers'].sum():,})")

    dests = blocks[blocks["jobs"] > 0][["GEOID20", "lat", "lon", "jobs", "jobs_low_wage", "jobs_health", "jobs_food"]]
    dests = dests.rename(columns={"GEOID20": "id"})
    dests.to_csv(args.out / "dests.csv", index=False)
    print(f"  {len(dests)} destination blocks with {dests['jobs'].sum():,} jobs "
          f"({county['jobs'].sum():,} of them in the county)")

    blocks.to_parquet(args.out / "blocks.parquet")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
