# Timeshed

A door-to-door transit travel time engine, and what it says about Milwaukee after dark.

Drop a pin anywhere in Milwaukee County and the area you can reach within N minutes by
walking plus public transit redraws in about a tenth of a second. The routing is an
own implementation of [RAPTOR](https://www.microsoft.com/en-us/research/wp-content/uploads/2012/01/raptor_alenex.pdf)
over the agency's published GTFS schedule, joined to a walking graph built from
OpenStreetMap, so a query covers walking to the stop, riding, transferring and walking
to the door. Because the schedule is real, 8 am and 11 pm give different shapes.

![Live map: 45 minutes from downtown Milwaukee at 8 am on a weekday](docs/figures/live_map.png)

Everything is built from two free sources: the [MCTS GTFS feed](https://kamino.mcts.org/gtfs/google_transit.zip)
and the [Geofabrik Wisconsin extract](https://download.geofabrik.de/north-america/us/wisconsin.html).

## The finding: Milwaukee's transit holds up until midnight, then falls off a cliff

The usual accessibility number, jobs reachable by transit in the morning peak, already
exists for every large US metro (the University of Minnesota's Access Across America
series). So this asks a question that series does not: **how much of that access
survives late at night?** For every inhabited 2020 census block in Milwaukee County
(10,685 blocks, 938,501 residents) the engine computed the number of jobs reachable
within 45 minutes door to door, leaving every 10 minutes across a two-hour window and
averaging. Jobs are 2023 LODES workplace counts by block (547,164 jobs inside the
walking network's bounding box, 496,457 of them in the county).

| Leaving | Median resident can reach | Mean | Residents reaching 100k+ | Residents under 5k |
|---|---:|---:|---:|---:|
| Weekday 7–9 am | **104,226** jobs | 112,169 | 51% | 5.0% |
| Weekday noon–2 pm | 87,064 | 96,218 | 44% | 5.1% |
| Weekday 10 pm–midnight | **78,129** | 88,938 | 41% | 6.5% |
| Weekday midnight–2 am | **33,932** | 48,648 | 15% | 7.0% |
| Sunday 7–9 am | 92,406 | 101,587 | 48% | 5.2% |

All figures are population weighted.

- Between 10 pm and midnight the median block still has **79%** of its morning job
  access. Only 4% of residents lose more than half. Sunday morning is nearly a weekday.
- After midnight the number drops to **41%** of the morning value. Two thirds of
  residents lose more than half their access, and one in ten loses three quarters.
- That matches the schedule: a dozen or so routes still have departures after
  midnight, and after 1 am it is mostly the Green Line (Bayshore to the airport through
  downtown), the Purple Line (27th Street), the Blue Line and route 30. The map of what
  survives is downtown, the east side and the Green Line corridor; the south and
  northwest edges of the county fall back to walking distance. (In the ratio view of
  the published map, blocks at the far edges show ratios near 1 only because they had
  little transit access in the morning to begin with.)

![Jobs reachable within 45 minutes, by departure window](docs/figures/jobs_by_time_of_day.png)

![Share of residents by jobs reachable, one curve per departure window](docs/figures/residents_by_access.png)

The interactive version, with every block hoverable and each window selectable, is in
[`docs/`](docs/) and published at <https://frankbstack.github.io/Timeshed/>.
The per-block table is written to `data/analysis/access_by_block.csv` by the analysis
script.

### Caveats

- Schedules, not real time. A 45-minute budget on paper is a 45-minute budget on a day
  when every bus is on time. The feed is MCTS's September 2026 schedule.
- Walking is 1.3 m/s on the OSM pedestrian network, with footpaths between stops up to
  1 km. Stops and blocks are snapped to the nearest walkable node.
- Only trips on the queried service day count. After-midnight departures use the
  previous day's late trips (GTFS times past 24:00), which is what matters for the
  midnight–2 am window; no next-morning trips are needed before 4 am.
- Jobs are counted at the block they are in, reached if the block's internal point is
  reachable. Jobs outside the walking bounding box, and transit run by neighbouring
  counties' systems, are not included. The Hop streetcar is a separate feed and is not
  included.
- LODES 2023 jobs against a 2026 schedule.

## How it works

```
GTFS zip ──> gtfs.rs ──> timetable.rs ──┐
                                        ├──> engine.rs (snap stops, build footpaths) ──> bundle.bin
OSM .pbf ──> osm.rs  ──> walk.rs ───────┘
                                                          │
         query:  walk (Dijkstra) ──> raptor.rs ──> walk (multi-source Dijkstra)
                                                          │
         isochrone.rs: node labels ──> 100 m grid ──> marching squares ──> GeoJSON bands
         access.rs:    node labels ──> destination blocks ──> weighted sums, in parallel
```

- **Timetable.** GTFS routes are regrouped into RAPTOR routes: trips that share an
  exact stop sequence and never overtake each other. The MCTS feed needs 322 of them
  for its 52 routes; 101 of those exist only because June and September variants of
  the same trip differ by a minute at some stop. Frequency-based trips are expanded.
  Service is resolved per date from `calendar.txt` and `calendar_dates.txt`.
- **Walking graph.** Two passes over the state extract: node coordinates inside the
  bounding box (the stops' box plus 2.5 km), then ways a pedestrian can use. Ways
  leaving the box are cut at its edge; only the largest connected component is kept.
  Milwaukee County comes out at 593k nodes and 1.5M directed edges.
- **RAPTOR.** Standard round-based scan with per-round labels, local pruning against the
  best-known arrival and a time budget, binary search for the first catchable trip
  (valid because routes are non-overtaking), and footpath relaxation after each round.
- **Door to door.** A bounded Dijkstra from the origin seeds every stop in walking
  reach; RAPTOR runs; a multi-source Dijkstra from every reached stop labels the walking
  nodes. Isochrones splat node labels onto a 100 m grid and cut it into 5-minute bands.
- **Batch.** Destinations are snapped once; each origin's query is read at each
  destination's node. Rayon runs origins in parallel with one query workspace per
  thread.

### Numbers on a 12-core laptop

| Step | Time |
|---|---:|
| Read the feed (1.0M stop_times) | 0.5 s |
| Walking graph from the 294 MB state extract | 1.9 s |
| Full bundle build, including 106k footpaths | 2.6 s |
| Door-to-door query, 45-minute budget | ~20 ms |
| 60-minute isochrone with bands, as GeoJSON | ~90 ms |
| Batch: 10,690 origins × 12 departures | ~110 s |

## Running it

```sh
# build the bundle (any GTFS feed plus an OSM extract that covers it)
cargo run --release -- build --gtfs data/raw/gtfs/mcts.zip \
    --osm data/raw/osm/wisconsin-latest.osm.pbf \
    --name "Milwaukee County Transit" -o data/bundles/milwaukee.bin

# one query
cargo run --release -- query --bundle data/bundles/milwaukee.bin \
    --lat 43.0389 --lon -87.9100 --date 2026-10-08 --time 08:00 --max 45 --geojson iso.geojson

# the live map on http://127.0.0.1:8080
cargo run --release -- serve --bundle data/bundles/milwaukee.bin
```

To reproduce the analysis:

```sh
python3 -m venv analysis/.venv && analysis/.venv/bin/pip install -r analysis/requirements.txt
analysis/.venv/bin/python analysis/prep_blocks.py \
    --blocks data/raw/census/tl_2020_55_tabblock20.zip \
    --wac data/raw/census/wi_wac_S000_JT00_2023.csv.gz \
    --rac data/raw/census/wi_rac_S000_JT00_2023.csv.gz \
    --county 079 --bbox=-88.1008,42.8483,-87.8187,43.2117 --out data/analysis

cargo run --release -- access --bundle data/bundles/milwaukee.bin \
    --origins data/analysis/origins.csv --dests data/analysis/dests.csv \
    --date 2026-10-08 --from 07:00 --to 09:00 --every 10 --max 45 \
    -o data/analysis/runs/weekday_am.csv
# ... the other windows: 12:00–14:00, 22:00–24:00, 24:00–26:00, and 2026-10-11 07:00–09:00

analysis/.venv/bin/python analysis/summarize.py
```

Census blocks come from [TIGER/Line 2020](https://www2.census.gov/geo/tiger/TIGER2020/TABBLOCK20/)
and jobs from [LODES 8](https://lehd.ces.census.gov/data/lodes/LODES8/wi/). The `data/`
directory is not committed.

## Layout

```
src/gtfs.rs        GTFS reader                 src/engine.rs     bundle + door-to-door query
src/timetable.rs   RAPTOR route grouping       src/isochrone.rs  grid + marching squares
src/osm.rs         walking graph from PBF      src/access.rs     batch accessibility
src/walk.rs        r-tree + bounded Dijkstra   src/server.rs     axum API
src/raptor.rs      the algorithm               web/              live map (MapLibre)
analysis/          census + LODES prep, summary and figures
docs/              published results map and figures
```
