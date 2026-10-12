# Reproducing the analysis

Raw inputs (not committed; see `data/` in `.gitignore`):

- `data/raw/gtfs/mcts.zip` from <https://kamino.mcts.org/gtfs/google_transit.zip>
- `data/raw/osm/wisconsin-latest.osm.pbf` from <https://download.geofabrik.de/north-america/us/wisconsin.html>
- `data/raw/census/tl_2020_55_tabblock20.zip` from <https://www2.census.gov/geo/tiger/TIGER2020/TABBLOCK20/>
- `data/raw/census/wi_wac_S000_JT00_2023.csv.gz` and `wi_rac_S000_JT00_2023.csv.gz` from <https://lehd.ces.census.gov/data/lodes/LODES8/wi/>

```sh
python3 -m venv analysis/.venv && analysis/.venv/bin/pip install -r analysis/requirements.txt

# origin and destination tables: inhabited blocks in Milwaukee County, blocks with jobs in the walking bbox
analysis/.venv/bin/python analysis/prep_blocks.py \
    --blocks data/raw/census/tl_2020_55_tabblock20.zip \
    --wac data/raw/census/wi_wac_S000_JT00_2023.csv.gz \
    --rac data/raw/census/wi_rac_S000_JT00_2023.csv.gz \
    --county 079 --bbox=-88.1008,42.8483,-87.8187,43.2117 --out data/analysis

# one batch run per departure window (each about two minutes)
for w in "weekday_am 2026-10-08 07:00 09:00" "weekday_midday 2026-10-08 12:00 14:00" \
         "weekday_night 2026-10-08 22:00 24:00" "weekday_owl 2026-10-08 24:00 26:00" \
         "sunday_am 2026-10-11 07:00 09:00"; do
  set -- $w
  cargo run --release -- access --bundle data/bundles/milwaukee.bin \
      --origins data/analysis/origins.csv --dests data/analysis/dests.csv \
      --date $2 --from $3 --to $4 --every 10 --max 45 -o data/analysis/runs/$1.csv
done

# per-block access, headline numbers, figures, and the block GeoJSON behind the site
analysis/.venv/bin/python analysis/summarize.py

# who loses after midnight: by wage band of residents, and by job sector
analysis/.venv/bin/python analysis/night_shift.py
```

Outputs: `data/analysis/access_by_block.csv` (every block, every window), `data/analysis/summary.json`
and `night_shift.json` (the numbers quoted in the README), `docs/figures/*.png`, and
`docs/data/blocks.geojson` for the Findings map.

To add `--board-slack 60` to every `access` run and compare, point `summarize.py` at a
different `--runs` directory and `--out`.

## Cross-check against r5

Needs Java 21 and r5py (in the requirements). r5 cannot clip an extract, so clip one first:

```sh
analysis/.venv/bin/python analysis/clip_osm.py data/raw/osm/wisconsin-latest.osm.pbf \
    data/raw/osm/milwaukee.osm.pbf --bbox=-88.1008,42.8483,-87.8187,43.2117
analysis/.venv/bin/python analysis/crosscheck_r5.py --origins 150 --dests 150 --board-slack 60
```

The first r5 run downloads its jar and builds its own network, a few minutes. Results land in
`data/analysis/crosscheck/` and the scatter in `docs/figures/crosscheck_r5.png`.
