#!/usr/bin/env python3
"""Turn the batch runs into per-block accessibility, headline numbers,
figures, and the data file behind the published map.

Each run csv has one row per (origin block, departure). A scenario's value
for a block is the mean over its departures, the same way Access Across
America averages over a departure window.
"""
import argparse
import json
from pathlib import Path

import geopandas as gpd
import matplotlib
import numpy as np
import pandas as pd

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

SCENARIOS = {
    "weekday_am": "Weekday 7–9 am",
    "weekday_midday": "Weekday noon–2 pm",
    "weekday_night": "Weekday 10 pm–midnight",
    "weekday_owl": "Weekday midnight–2 am",
    "sunday_am": "Sunday 7–9 am",
}
# one hue, light -> dark, from the shared palette
BLUES = ["#cde2fb", "#b7d3f6", "#9ec5f4", "#86b6ef", "#6da7ec", "#5598e7",
         "#3987e5", "#2a78d6", "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b"]
# fixed categorical order for the scenario lines
CAT = {"weekday_am": "#2a78d6", "weekday_midday": "#eb6834", "weekday_night": "#1baf7a", "sunday_am": "#eda100",
       "weekday_owl": "#e87ba4"}
INK, INK_MUTED, GRID = "#1a1a19", "#5e5e5a", "#e6e5e1"


def weighted_median(values, weights):
    order = np.argsort(values)
    v, w = np.asarray(values)[order], np.asarray(weights)[order]
    cum = np.cumsum(w)
    return float(v[np.searchsorted(cum, cum[-1] / 2.0)])


def load_runs(runs_dir: Path) -> pd.DataFrame:
    frames = []
    for key in SCENARIOS:
        path = runs_dir / f"{key}.csv"
        if not path.exists():
            print(f"  (no {path.name}, skipping)")
            continue
        df = pd.read_csv(path, dtype={"origin": str})
        df["scenario"] = key
        frames.append(df)
    return pd.concat(frames, ignore_index=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=Path, default=Path("data/analysis/runs"))
    ap.add_argument("--blocks", type=Path, default=Path("data/analysis/blocks.parquet"))
    ap.add_argument("--out", type=Path, default=Path("data/analysis"))
    ap.add_argument("--docs", type=Path, default=Path("docs"))
    ap.add_argument("--budget", type=int, default=45)
    args = ap.parse_args()

    print("loading runs...")
    runs = load_runs(args.runs)
    scenarios = [k for k in SCENARIOS if k in set(runs["scenario"])]
    blocks = gpd.read_parquet(args.blocks)
    county_jobs = int(blocks.loc[blocks["COUNTYFP20"] == "079", "jobs"].sum())
    all_jobs = int(blocks["jobs"].sum())

    # per block x scenario: mean over departures (and the spread, for the map)
    g = runs.groupby(["origin", "scenario"])
    per = g.agg(jobs=("jobs", "mean"), jobs_min=("jobs", "min"), jobs_max=("jobs", "max"),
                low_wage=("jobs_low_wage", "mean"), on_network=("on_network", "all")).reset_index()
    wide = per.pivot(index="origin", columns="scenario")
    wide.columns = [f"{a}_{b}" for a, b in wide.columns]
    wide = wide.reset_index().rename(columns={"origin": "GEOID20"})
    data = blocks.merge(wide, on="GEOID20", how="inner")
    data = data[data[f"on_network_{scenarios[0]}"]]
    pop = data["POP20"].to_numpy(dtype=float)

    # headline numbers, population weighted
    stats = {"budget_min": args.budget, "county_jobs": county_jobs, "jobs_in_bbox": all_jobs,
             "blocks": int(len(data)), "population": int(pop.sum()), "scenarios": {}}
    for k in scenarios:
        v = data[f"jobs_{k}"].to_numpy()
        s = {
            "label": SCENARIOS[k],
            "mean": float(np.average(v, weights=pop)),
            "median": weighted_median(v, pop),
            "share_reaching_100k": float(pop[v >= 100_000].sum() / pop.sum()),
            "share_reaching_25k": float(pop[v >= 25_000].sum() / pop.sum()),
            "share_under_5k": float(pop[v < 5_000].sum() / pop.sum()),
            "p10": float(np.percentile(v, 10)), "p90": float(np.percentile(v, 90)),
        }
        stats["scenarios"][k] = s
    for later, name in [("weekday_night", "night_vs_am"), ("weekday_owl", "owl_vs_am")]:
        if "weekday_am" not in scenarios or later not in scenarios:
            continue
        am, v = data["jobs_weekday_am"].to_numpy(), data[f"jobs_{later}"].to_numpy()
        ratio = np.where(am > 0, v / np.maximum(am, 1), np.nan)
        data[f"{later}_over_am"] = ratio
        stats[name] = {
            "median_ratio": weighted_median(np.nan_to_num(ratio), pop),
            "share_losing_half": float(pop[ratio < 0.5].sum() / pop.sum()),
            "share_losing_three_quarters": float(pop[ratio < 0.25].sum() / pop.sum()),
            "share_under_5k": float(pop[v < 5_000].sum() / pop.sum()),
            "mean_ratio_of_means": stats["scenarios"][later]["mean"] / stats["scenarios"]["weekday_am"]["mean"],
        }

    args.out.mkdir(parents=True, exist_ok=True)
    cols = ["GEOID20", "POP20", "workers", "jobs"] + [c for c in data.columns if c.startswith("jobs_") or c.startswith("low_wage_") or c.endswith("_over_am")]
    data[cols].to_csv(args.out / "access_by_block.csv", index=False)
    (args.out / "summary.json").write_text(json.dumps(stats, indent=2))
    (args.docs / "data").mkdir(parents=True, exist_ok=True)
    (args.docs / "data" / "summary.json").write_text(json.dumps(stats, indent=2))
    print(json.dumps(stats, indent=2))

    # ---- figures -------------------------------------------------------
    figs = args.docs / "figures"
    figs.mkdir(parents=True, exist_ok=True)
    plt.rcParams.update({"font.family": "sans-serif", "font.size": 10, "axes.edgecolor": GRID,
                         "axes.labelcolor": INK_MUTED, "xtick.color": INK_MUTED, "ytick.color": INK_MUTED,
                         "text.color": INK, "figure.facecolor": "#fcfcfb", "axes.facecolor": "#fcfcfb"})

    # 1. side-by-side choropleths, same scale
    proj = data.to_crs(epsg=32016)  # Wisconsin South state plane, meters-ish
    vmax = float(np.percentile(data[f"jobs_{scenarios[0]}"], 99))
    show = [k for k in ["weekday_am", "weekday_night", "weekday_owl"] if k in scenarios] or scenarios[:2]
    fig, axes = plt.subplots(1, len(show), figsize=(4.6 * len(show), 7.0))
    cmap = matplotlib.colors.LinearSegmentedColormap.from_list("blues", BLUES)
    for ax, k in zip(np.atleast_1d(axes), show):
        proj.plot(column=f"jobs_{k}", cmap=cmap, vmin=0, vmax=vmax, ax=ax, linewidth=0)
        ax.set_title(SCENARIOS[k], fontsize=12, loc="left", color=INK)
        ax.set_axis_off()
    sm = plt.cm.ScalarMappable(cmap=cmap, norm=matplotlib.colors.Normalize(0, vmax))
    cb = fig.colorbar(sm, ax=axes, orientation="horizontal", fraction=0.035, pad=0.02, aspect=50)
    cb.set_label(f"Jobs reachable within {args.budget} minutes by walking and transit (mean over departures)")
    cb.outline.set_visible(False)
    cb.ax.xaxis.set_major_formatter(matplotlib.ticker.FuncFormatter(lambda x, _: f"{x/1000:.0f}k"))
    fig.savefig(figs / "jobs_by_time_of_day.png", dpi=110, bbox_inches="tight")
    plt.close(fig)

    # 2. population-weighted cumulative distribution per scenario
    fig, ax = plt.subplots(figsize=(8, 4.6))
    for k in scenarios:
        v = data[f"jobs_{k}"].to_numpy()
        order = np.argsort(v)
        x, w = v[order], pop[order]
        y = np.cumsum(w) / w.sum()
        ax.plot(x, 1 - y, color=CAT[k], lw=2, label=SCENARIOS[k])
    ax.set_xlabel(f"Jobs reachable within {args.budget} minutes")
    ax.set_ylabel("Share of residents with at least this many")
    ax.xaxis.set_major_formatter(matplotlib.ticker.FuncFormatter(lambda x, _: f"{x/1000:.0f}k"))
    ax.yaxis.set_major_formatter(matplotlib.ticker.PercentFormatter(1.0, decimals=0))
    ax.grid(axis="y", color=GRID, lw=0.8)
    for s in ["top", "right"]:
        ax.spines[s].set_visible(False)
    ax.legend(frameon=False, loc="upper right")
    fig.savefig(figs / "residents_by_access.png", dpi=110, bbox_inches="tight")
    plt.close(fig)

    # ---- data behind the web map ----------------------------------------
    ratios = [c for c in data.columns if c.endswith("_over_am")]
    web = data[["GEOID20", "POP20", "jobs"] + [f"jobs_{k}" for k in scenarios] + ratios + ["geometry"]].copy()
    web = web.rename(columns={"GEOID20": "id", "POP20": "pop", "jobs": "jobs_here"})
    for k in scenarios:
        web[f"jobs_{k}"] = web[f"jobs_{k}"].round().astype(int)
    for c in ratios:
        web[c] = web[c].round(3)
    web["geometry"] = web.to_crs(epsg=32016).simplify(8, preserve_topology=True).to_crs(epsg=4326)
    web = web.set_geometry("geometry").set_crs(epsg=4326)
    (args.docs / "data").mkdir(parents=True, exist_ok=True)
    out = args.docs / "data" / "blocks.geojson"
    web.to_file(out, driver="GeoJSON", COORDINATE_PRECISION=5)
    print(f"wrote {out} ({out.stat().st_size / 1e6:.1f} MB), figures in {figs}")


if __name__ == "__main__":
    main()
