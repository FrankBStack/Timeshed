#!/usr/bin/env python3
"""Assemble the published site under docs/ from web/, the wasm package and
one bundle per city.

    scripts/render-pages.py milwaukee=data/bundles/milwaukee.bin cincinnati=data/bundles/cincinnati.bin

Everything but the pages goes under a content-hashed path (so the host can
cache it for a year); the pages are short-lived and name the current files.
Cities come from cities.json; a city gets a Findings tab when
docs/data/<city>/summary.json exists. Pages written:

    /                       first city, Explore first
    /<city>/                each city, Explore first
    /<city>/findings/       each city with findings, Findings first, own card
    /findings/              first city's findings (kept for old links)
    /live/                  redirect to / (kept for old links)
"""
import gzip
import hashlib
import json
import shutil
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
docs = root / "docs"
web = root / "web"
template = (web / "index.html").read_text()
cities = json.loads((root / "cities.json").read_text())
SITE = "https://timeshed.frankbs.dev/"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:12]


def hashed_copy(src: Path, subdir: str) -> str:
    """Copy src to docs/<subdir>/<stem>.<hash><suffix>; return the docs-relative path."""
    out_dir = docs / subdir
    out_dir.mkdir(parents=True, exist_ok=True)
    name = f"{src.stem}.{digest(src)}{src.suffix}"
    shutil.copy2(src, out_dir / name)
    return f"{subdir}/{name}"


# ---- bundles: one per city, gzipped deterministically, hashed --------------
bundles = {}
for arg in sys.argv[1:]:
    key, path = arg.split("=", 1)
    if key not in cities:
        sys.exit(f"{key} is not in cities.json")
    raw = Path(path).read_bytes()
    gz = gzip.compress(raw, compresslevel=9, mtime=0)
    h = hashlib.sha256(gz).hexdigest()[:12]
    (docs / "bundle").mkdir(parents=True, exist_ok=True)
    out = docs / "bundle" / f"{key}.{h}.bin.gz"
    out.write_bytes(gz)
    bundles[key] = (f"bundle/{key}.{h}.bin.gz", len(gz))
if not bundles:
    sys.exit("give at least one city=bundle.bin")

# ---- static assets ---------------------------------------------------------
for old in ["pkg", "assets", "live", "findings"] + [k for k in cities]:
    p = docs / old
    if p.is_dir():
        shutil.rmtree(p)
for stale in docs.glob("bundle/*.bin.gz"):
    if not any(stale.name == Path(b[0]).name for b in bundles.values()):
        stale.unlink()
app = hashed_copy(web / "app.js", "assets")
style = hashed_copy(web / "style.css", "assets")
worker = hashed_copy(web / "worker.js", "assets")
pkg_hash = digest(web / "pkg" / "timeshed_bg.wasm")
(docs / "pkg" / pkg_hash).mkdir(parents=True, exist_ok=True)
for f in ["timeshed.js", "timeshed_bg.wasm"]:
    shutil.copy2(web / "pkg" / f, docs / "pkg" / pkg_hash / f)
pkg = f"pkg/{pkg_hash}/timeshed.js"

# ---- per-city config -------------------------------------------------------
city_cfg = {}
for key, c in cities.items():
    if key not in bundles:
        continue
    findings = None
    if (docs / "data" / key / "summary.json").exists():
        f = c["findings"]
        findings = {"title": f["title"], "area": f["area"],
                    "blocks": f"data/{key}/blocks.geojson", "summary": f"data/{key}/summary.json"}
    city_cfg[key] = {"name": c["name"], "agency": c["agency"], "center": c["center"], "zoom": c["zoom"],
                     "bundleUrl": bundles[key][0], "bundleBytes": bundles[key][1], "findings": findings}
first = next(iter(city_cfg))


def og(title, description, url):
    return f'''  <meta property="og:type" content="website">
  <meta property="og:title" content="{title}">
  <meta property="og:description" content="{description}">
  <meta property="og:url" content="{url}">
  <meta property="og:image" content="{SITE}figures/card.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta name="twitter:card" content="summary_large_image">'''


def render(base, city, mode, title, og_tags):
    cfg = {"base": base, "mode": mode, "city": city, "pkgUrl": pkg, "workerUrl": worker, "cities": city_cfg}
    page = template.replace("  <!-- OG -->", og_tags)
    page = page.replace("  <!-- CONFIG -->", f"  <script>window.TIMESHED = {json.dumps(cfg)};</script>")
    page = page.replace("<title>Timeshed</title>", f"<title>{title}</title>")
    page = page.replace('"style.css"', f'"{base}{style}"').replace('"app.js"', f'"{base}{app}"')
    return page


def explore_og(key, url):
    c = cities[key]
    return og(f"Timeshed: everywhere you can reach by transit in {c['name']}, from any pin",
              f"Drag a pin anywhere {c['agency']} runs and the area reachable by walking plus transit redraws in your browser; click to see the trip. An own RAPTOR implementation in Rust, compiled to WebAssembly.",
              url)


def findings_og(key, url):
    f = cities[key]["findings"]
    return og(f["title"],
              f"Jobs reachable within 45 minutes by walking and transit from every census block in {f['area']}, by time of day.",
              url)


def write(rel, text):
    p = docs / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


write("index.html", render("", first, "explore", "Timeshed", explore_og(first, SITE)))
for key, c in city_cfg.items():
    name = cities[key]["name"]
    write(f"{key}/index.html", render("../", key, "explore", f"{name} · Timeshed", explore_og(key, f"{SITE}{key}/")))
    if c["findings"]:
        write(f"{key}/findings/index.html",
              render("../../", key, "findings", f"{c['findings']['title']} · Timeshed", findings_og(key, f"{SITE}{key}/findings/")))
if city_cfg[first]["findings"]:
    write("findings/index.html",
          render("../", first, "findings", f"{city_cfg[first]['findings']['title']} · Timeshed", findings_og(first, f"{SITE}findings/")))
write("live/index.html", '''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Timeshed</title>
<meta http-equiv="refresh" content="0; url=../"><link rel="canonical" href="https://timeshed.frankbs.dev/">
</head><body><p>The live map moved to <a href="../">timeshed.frankbs.dev</a>.</p></body></html>
''')
print("rendered:", ", ".join(f"/{k}/" + ("(+findings)" if c["findings"] else "") for k, c in city_cfg.items()), "| first city:", first)
