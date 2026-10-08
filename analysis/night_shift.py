#!/usr/bin/env python3
"""Who loses when the buses stop?

Reuses the batch runs: each carries, per origin block and departure, the
jobs reachable within the budget broken out by wage band and by two
night-shift sectors (health care, accommodation and food service). This
script weights those by who lives in each block, from LODES RAC, and asks:

  1. Do low-wage workers' home blocks keep more or less of their access
     after midnight than everyone else's?
  2. Do the night-shift sectors stay reachable at night?
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

WINDOWS = {"weekday_am": "7–9 am", "weekday_night": "10 pm–midnight", "weekday_owl": "midnight–2 am"}
MEASURES = {"jobs": "All jobs", "jobs_low_wage": "Low-wage jobs", "jobs_health": "Health care jobs", "jobs_food": "Food service jobs"}
# fixed categorical order from the shared palette
COLORS = {"jobs": "#2a78d6", "jobs_low_wage": "#eb6834", "jobs_health": "#1baf7a", "jobs_food": "#eda100"}
INK, INK_MUTED, GRID = "#1a1a19", "#5e5e5a", "#e6e5e1"


def weighted_median(values, weights):
    v, w = np.asarray(values, dtype=float), np.asarray(weights, dtype=float)
    ok = w > 0
    v, w = v[ok], w[ok]
    order = np.argsort(v)
    v, w = v[order], w[order]
    cum = np.cumsum(w)
    return float(v[np.searchsorted(cum, cum[-1] / 2.0)])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=Path, default=Path("data/analysis/runs"))
    ap.add_argument("--blocks", type=Path, default=Path("data/analysis/blocks.parquet"))
    ap.add_argument("--out", type=Path, default=Path("data/analysis"))
    ap.add_argument("--docs", type=Path, default=Path("docs"))
    args = ap.parse_args()

    per = {}
    for w in WINDOWS:
        df = pd.read_csv(args.runs / f"{w}.csv", dtype={"origin": str})
        per[w] = df.groupby("origin")[list(MEASURES)].mean()
    blocks = gpd.read_parquet(args.blocks).set_index("GEOID20")
    access = pd.concat({w: per[w] for w in WINDOWS}, axis=1)
    access.columns = [f"{w}:{m}" for w, m in access.columns]
    data = blocks[["POP20", "workers", "workers_low_wage"]].join(access, how="inner")
    data["workers_other"] = data["workers"] - data["workers_low_wage"]
    groups = {"low_wage": ("Low-wage workers", data["workers_low_wage"]), "other": ("Other workers", data["workers_other"]), "residents": ("All residents", data["POP20"])}

    out = {"windows": WINDOWS, "measures": MEASURES, "groups": {}, "sectors": {}}

    # 1. by who lives there: low-wage jobs reachable, weighted by workers of each kind
    for g, (label, wts) in groups.items():
        entry = {"label": label, "weight_total": int(wts.sum())}
        for m in ["jobs", "jobs_low_wage"]:
            med = {w: weighted_median(data[f"{w}:{m}"], wts) for w in WINDOWS}
            owl_ratio = data[f"weekday_owl:{m}"] / data[f"weekday_am:{m}"].clip(lower=1)
            night_ratio = data[f"weekday_night:{m}"] / data[f"weekday_am:{m}"].clip(lower=1)
            entry[m] = {
                "median": med,
                "retained_night": weighted_median(night_ratio, wts),
                "retained_owl": weighted_median(owl_ratio, wts),
                "share_losing_half_owl": float(wts[owl_ratio < 0.5].sum() / wts.sum()),
                "share_under_5k_owl": float(wts[data[f"weekday_owl:{m}"] < 5000].sum() / wts.sum()),
            }
        out["groups"][g] = entry

    # 2. by sector: everyone, population weighted. "Retained" is the median
    # block's later value divided by its own morning value, like the groups.
    pop = data["POP20"]
    for m, label in MEASURES.items():
        med = {w: weighted_median(data[f"{w}:{m}"], pop) for w in WINDOWS}
        am = data[f"weekday_am:{m}"].clip(lower=1)
        out["sectors"][m] = {
            "label": label,
            "median": med,
            "retained_night": weighted_median(data[f"weekday_night:{m}"] / am, pop),
            "retained_owl": weighted_median(data[f"weekday_owl:{m}"] / am, pop),
            "share_under_5k_owl": float(pop[data[f"weekday_owl:{m}"] < 5000].sum() / pop.sum()),
            "total_in_bbox": int(blocks[m].sum()),
        }

    # where do low-wage workers live relative to the night network?
    lw_share = data["workers_low_wage"] / data["workers"].clip(lower=1)
    owl_all = data["weekday_owl:jobs"] / data["weekday_am:jobs"].clip(lower=1)
    hi = data["workers"] >= 20
    corr = float(np.corrcoef(lw_share[hi], owl_all[hi])[0, 1])
    q = pd.qcut(lw_share[hi], 4, labels=["lowest quarter", "second", "third", "highest quarter"])
    by_q = pd.DataFrame({"q": q, "retained_owl": owl_all[hi], "w": data["workers"][hi]}).groupby("q", observed=True).apply(
        lambda d: weighted_median(d["retained_owl"], d["w"])
    )
    out["low_wage_share_vs_retained_owl"] = {"corr_blocks_20plus_workers": corr, "retained_owl_by_low_wage_share_quartile": {str(k): float(v) for k, v in by_q.items()}}

    (args.out / "night_shift.json").write_text(json.dumps(out, indent=2))
    print(json.dumps(out, indent=2))

    # ---- figures ---------------------------------------------------------
    figs = args.docs / "figures"
    figs.mkdir(parents=True, exist_ok=True)
    plt.rcParams.update({"font.family": "sans-serif", "font.size": 10, "axes.edgecolor": GRID,
                         "axes.labelcolor": INK_MUTED, "xtick.color": INK_MUTED, "ytick.color": INK_MUTED,
                         "text.color": INK, "figure.facecolor": "#fcfcfb", "axes.facecolor": "#fcfcfb"})

    # Fig 1: share of morning reach kept, by sector, for the two night windows.
    # The point is that the rows are the same, so a dot plot with the four
    # rows aligned shows it better than four overlapping lines.
    fig, ax = plt.subplots(figsize=(7.2, 3.0))
    rows = list(MEASURES)
    y = np.arange(len(rows))[::-1]
    series = [("retained_night", "10 pm–midnight", "#2a78d6"), ("retained_owl", "midnight–2 am", "#eb6834")]
    for key, label, color in series:
        vals = [out["sectors"][m][key] for m in rows]
        ax.scatter(vals, y, s=70, color=color, zorder=3, label=label, edgecolor="#fcfcfb", linewidth=1)
        for v, yy in zip(vals, y):
            ax.annotate(f"{v:.0%}", (v, yy), xytext=(0, 9), textcoords="offset points", ha="center", fontsize=9, color=INK)
    for yy in y:
        ax.axhline(yy, color=GRID, lw=0.8, zorder=1)
    ax.set_yticks(y, [MEASURES[m] for m in rows])
    ax.set_xlim(0, 1.0)
    ax.xaxis.set_major_formatter(matplotlib.ticker.PercentFormatter(1.0, decimals=0))
    ax.set_xlabel("Share of the block's 7–9 am reach kept (median resident)")
    for sp in ["top", "right", "left"]:
        ax.spines[sp].set_visible(False)
    ax.tick_params(axis="y", length=0)
    ax.legend(frameon=False, loc="lower center", bbox_to_anchor=(0.5, 1.0), ncol=2)
    fig.savefig(figs / "night_by_sector.png", dpi=110, bbox_inches="tight")
    plt.close(fig)

    # Fig 2: low-wage jobs reachable, low-wage workers vs other workers, by window
    fig, ax = plt.subplots(figsize=(7.2, 3.6))
    x = np.arange(len(WINDOWS))
    width = 0.36
    for i, (g, color) in enumerate([("low_wage", "#eb6834"), ("other", "#2a78d6")]):
        med = out["groups"][g]["jobs_low_wage"]["median"]
        vals = [med[w] for w in WINDOWS]
        bars = ax.bar(x + (i - 0.5) * width, vals, width * 0.94, color=color, label=out["groups"][g]["label"])
        for b, v in zip(bars, vals):
            ax.annotate(f"{v/1000:.0f}k", (b.get_x() + b.get_width() / 2, v), xytext=(0, 3), textcoords="offset points", ha="center", fontsize=9, color=INK)
    ax.set_xticks(x, list(WINDOWS.values()))
    ax.set_ylabel("Low-wage jobs within 45 min, median worker")
    ax.yaxis.set_major_formatter(matplotlib.ticker.FuncFormatter(lambda v, _: f"{v/1000:.0f}k"))
    ax.grid(axis="y", color=GRID, lw=0.8)
    ax.set_axisbelow(True)
    for sp in ["top", "right"]:
        ax.spines[sp].set_visible(False)
    ax.legend(frameon=False, loc="upper right")
    fig.savefig(figs / "night_low_wage_workers.png", dpi=110, bbox_inches="tight")
    plt.close(fig)
    print(f"figures in {figs}")


if __name__ == "__main__":
    main()
