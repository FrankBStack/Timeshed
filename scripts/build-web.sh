#!/bin/sh
# Build the browser engine and assemble the static live map under docs/live.
#   scripts/build-web.sh <bundle.bin> <bundle-url>
# <bundle.bin> is a bundle from `timeshed build`; it is gzipped next to the
# page for local testing, while the published page downloads <bundle-url>.
set -eu
cd "$(dirname "$0")/.."
BUNDLE=${1:?bundle path}
URL=${2:?public bundle url}

wasm-pack build --target web --release --out-dir web/pkg --out-name timeshed --no-pack --no-typescript . \
    -- --no-default-features --features wasm

rm -rf docs/live && mkdir -p docs/live/pkg
cp web/index.html web/style.css web/app.js web/worker.js docs/live/
cp web/pkg/timeshed.js web/pkg/timeshed_bg.wasm docs/live/pkg/
# the published page reads its bundle from the release asset
sed -i '' "s|<script src=\"app.js\"></script>|<script>window.TIMESHED_BUNDLE_URL = '$URL';</script>\n  <script src=\"app.js\"></script>|" docs/live/index.html
gzip -9 -k -f -c "$BUNDLE" > docs/live/bundle.bin.gz
ls -la docs/live docs/live/pkg
