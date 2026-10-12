#!/usr/bin/env python3
"""Render the published pages from web/index.html.

The dev server serves web/ as-is (HTTP backend, Explore only). The published
site gets the same markup three times: / starts in Explore, /findings/ starts
in Findings with its own social card, and /live/ redirects to / for old links.
"""
import json
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
template = (root / "web" / "index.html").read_text()
SITE = "https://timeshed.frankbs.dev/"
# build-web.sh passes the hashed asset paths it just wrote under docs/
BUNDLE, PKG = sys.argv[1], sys.argv[2]
BUNDLE_BYTES = (root / "docs" / BUNDLE).stat().st_size


def og(title, description, url):
    return f'''  <meta property="og:type" content="website">
  <meta property="og:title" content="{title}">
  <meta property="og:description" content="{description}">
  <meta property="og:url" content="{url}">
  <meta property="og:image" content="{SITE}figures/card.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta name="twitter:card" content="summary_large_image">'''


def render(base, mode, title, og_tags):
    cfg = {"base": base, "mode": mode, "bundleUrl": BUNDLE, "pkgUrl": PKG, "bundleBytes": BUNDLE_BYTES,
           "findings": {"blocks": "data/blocks.geojson", "summary": "data/summary.json"}}
    page = template.replace("  <!-- OG -->", og_tags)
    page = page.replace("  <!-- CONFIG -->", f"  <script>window.TIMESHED = {json.dumps(cfg)};</script>")
    page = page.replace("<title>Timeshed</title>", f"<title>{title}</title>")
    for asset in ["style.css", "app.js"]:
        page = page.replace(f'"{asset}"', f'"{base}{asset}"')
    return page


(root / "docs" / "index.html").write_text(render(
    "", "explore", "Timeshed",
    og("Timeshed: everywhere you can reach by transit, from any pin",
       "Drop a pin anywhere in Milwaukee County and the area reachable by walking plus transit redraws in your browser. An own RAPTOR implementation in Rust, compiled to WebAssembly.",
       SITE)))
(root / "docs" / "findings").mkdir(exist_ok=True)
(root / "docs" / "findings" / "index.html").write_text(render(
    "../", "findings", "Where can Milwaukee get to work? · Timeshed",
    og("Where can Milwaukee get to work?",
       "Jobs reachable within 45 minutes by walking and transit from every census block in Milwaukee County, by time of day. The night network keeps 79% of morning access until midnight, 41% after.",
       SITE + "findings/")))
(root / "docs" / "live").mkdir(exist_ok=True)
(root / "docs" / "live" / "index.html").write_text('''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Timeshed</title>
<meta http-equiv="refresh" content="0; url=../"><link rel="canonical" href="https://timeshed.frankbs.dev/">
</head><body><p>The live map moved to <a href="../">timeshed.frankbs.dev</a>.</p></body></html>
''')
print("rendered docs/index.html, docs/findings/index.html, docs/live/index.html")
