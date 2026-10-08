#!/usr/bin/env python3
"""Compare Timeshed's door-to-door travel times with r5 (Conveyal's router,
via r5py) on the same feed, the same OSM extract and the same points.

The two routers share no code. They do differ in how they build the
walking network, snap points to it and round times, so the question is
not "are they identical" but "how far apart are they and in which
direction".
"""
import argparse
import datetime as dt
import json
import subprocess
from pathlib import Path

import geopandas as gpd
import matplotlib
import numpy as np
import pandas as pd

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

WALK_MPS = 1.3


def sample_points(blocks: Path, n_orig: int, n_dest: int, seed: int, county: str):
    b = gpd.read_parquet(blocks)
    rng = np.random.default_rng(seed)
    orig = b[(b["COUNTYFP20"] == county) & (b["POP20"] > 0)].sample(n_orig, random_state=int(rng.integers(1 << 31)))
    dest = b[b["jobs"] > 0].sample(n_dest, random_state=int(rng.integers(1 << 31)))
    o = pd.DataFrame({"id": orig["GEOID20"].values, "lat": orig["lat"].values, "lon": orig["lon"].values})
    d = pd.DataFrame({"id": dest["GEOID20"].values, "lat": dest["lat"].values, "lon": dest["lon"].values})
    return o, d


def run_timeshed(binary, bundle, o_csv, d_csv, date, time, max_min, board_slack, out):
    subprocess.run(
        [binary, "matrix", "--bundle", bundle, "--origins", o_csv, "--dests", d_csv, "--date", date,
         "--time", time, "--max", str(max_min), "--walk-speed", str(WALK_MPS),
         "--board-slack", str(board_slack), "-o", out],
        check=True,
    )
    return pd.read_csv(out, dtype={"origin": str, "dest": str})


def run_r5(osm_pbf, gtfs, o: pd.DataFrame, d: pd.DataFrame, date, time, max_min):
    import r5py

    net = r5py.TransportNetwork(osm_pbf, [gtfs])
    to_gdf = lambda df: gpd.GeoDataFrame(df[["id"]], geometry=gpd.points_from_xy(df["lon"], df["lat"]), crs="EPSG:4326")
    hh, mm = (int(x) for x in time.split(":"))
    departure = dt.datetime.combine(dt.date.fromisoformat(date), dt.time(hh, mm))
    ttm = r5py.TravelTimeMatrix(
        net,
        origins=to_gdf(o),
        destinations=to_gdf(d),
        departure=departure,
        departure_time_window=dt.timedelta(minutes=1),
        transport_modes=[r5py.TransportMode.TRANSIT, r5py.TransportMode.WALK],
        speed_walking=WALK_MPS * 3.6,
        max_time=dt.timedelta(minutes=max_min),
        max_time_walking=dt.timedelta(minutes=max_min),
        max_public_transport_rides=8,
    )
    df = pd.DataFrame(ttm)
    df = df.rename(columns={"from_id": "origin", "to_id": "dest", "travel_time": "r5_min"})
    df["origin"] = df["origin"].astype(str)
    df["dest"] = df["dest"].astype(str)
    return df.dropna(subset=["r5_min"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bundle", default="data/bundles/milwaukee.bin")
    ap.add_argument("--gtfs", default="data/raw/gtfs/mcts.zip")
    ap.add_argument("--osm", default="data/raw/osm/milwaukee.osm.pbf", help="clipped extract, r5 cannot clip")
    ap.add_argument("--blocks", default="data/analysis/blocks.parquet")
    ap.add_argument("--binary", default="target/release/timeshed")
    ap.add_argument("--date", default="2026-10-08")
    ap.add_argument("--time", default="08:00")
    ap.add_argument("--max", type=int, default=120)
    ap.add_argument("--origins", type=int, default=150)
    ap.add_argument("--dests", type=int, default=150)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--board-slack", type=int, default=60,
                    help="seconds before departure you must be at the stop; R5 hard-codes 60")
    ap.add_argument("--reuse-r5", action="store_true", help="read r5.csv from a previous run instead of routing again")
    ap.add_argument("--out", type=Path, default=Path("data/analysis/crosscheck"))
    ap.add_argument("--docs", type=Path, default=Path("docs"))
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    o, d = sample_points(Path(args.blocks), args.origins, args.dests, args.seed, "079")
    o.to_csv(args.out / "origins.csv", index=False)
    d.to_csv(args.out / "dests.csv", index=False)

    print("timeshed...")
    ours = run_timeshed(args.binary, args.bundle, str(args.out / "origins.csv"), str(args.out / "dests.csv"),
                        args.date, args.time, args.max, args.board_slack, str(args.out / "timeshed.csv"))
    ours["ts_min"] = ours["seconds"] / 60.0
    print(f"  {len(ours)} reachable pairs")

    print("r5...")
    if args.reuse_r5 and (args.out / "r5.csv").exists():
        theirs = pd.read_csv(args.out / "r5.csv", dtype={"origin": str, "dest": str})
    else:
        theirs = run_r5(args.osm, args.gtfs, o, d, args.date, args.time, args.max)
        theirs.to_csv(args.out / "r5.csv", index=False)
    print(f"  {len(theirs)} reachable pairs")

    both = ours.merge(theirs, on=["origin", "dest"], how="outer", indicator=True)
    pairs = len(o) * len(d)
    only_ours = int((both["_merge"] == "left_only").sum())
    only_r5 = int((both["_merge"] == "right_only").sum())
    m = both[both["_merge"] == "both"].copy()
    # r5 reports whole minutes (floor of the median over the window)
    m["diff"] = m["ts_min"] - m["r5_min"]
    absd = m["diff"].abs()
    stats = {
        "pairs": pairs, "reachable_both": int(len(m)), "only_timeshed": only_ours, "only_r5": only_r5,
        "median_diff_min": float(m["diff"].median()), "mean_diff_min": float(m["diff"].mean()),
        "within_1_min": float((absd <= 1).mean()), "within_2_min": float((absd <= 2).mean()),
        "within_5_min": float((absd <= 5).mean()), "p95_abs_diff_min": float(absd.quantile(0.95)),
        "date": args.date, "time": args.time, "max_min": args.max, "board_slack_secs": args.board_slack,
    }
    print(json.dumps(stats, indent=2))
    (args.out / "summary.json").write_text(json.dumps(stats, indent=2))
    m.to_csv(args.out / "pairs.csv", index=False)

    figs = args.docs / "figures"
    figs.mkdir(parents=True, exist_ok=True)
    fig, ax = plt.subplots(figsize=(5.6, 5.6))
    ax.scatter(m["r5_min"], m["ts_min"], s=4, alpha=0.25, color="#2a78d6", edgecolor="none")
    lim = max(m["r5_min"].max(), m["ts_min"].max())
    ax.plot([0, lim], [0, lim], color="#8a8a85", lw=1)
    ax.set_xlabel("r5 travel time (minutes)")
    ax.set_ylabel("Timeshed travel time (minutes)")
    ax.set_title(f"{len(m):,} origin–destination pairs, {args.date} {args.time}", fontsize=11, loc="left")
    for s in ["top", "right"]:
        ax.spines[s].set_visible(False)
    fig.savefig(figs / "crosscheck_r5.png", dpi=110, bbox_inches="tight")


if __name__ == "__main__":
    main()
