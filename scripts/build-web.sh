#!/bin/sh
# Build the browser engine and assemble the static live map under docs/live.
#   scripts/build-web.sh <bundle.bin>
# <bundle.bin> is a bundle from `timeshed build`. It is gzipped next to the
# page: GitHub release assets are served without CORS headers, so the bundle
# has to come from the same origin as the page.
set -eu
cd "$(dirname "$0")/.."
BUNDLE=${1:?bundle path}
URL=bundle.bin.gz
export PATH="$HOME/.cargo/bin:$PATH"
# rust-lld looks for libLLVM next to itself; some rustup installs keep it one
# level up. Harmless where the toolchain is fine. (macOS strips DYLD_* from a
# caller's environment, so it has to be set here, not before the script.)
export DYLD_FALLBACK_LIBRARY_PATH="$(rustc --print sysroot)/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"

wasm-pack build --target web --release --out-dir web/pkg --out-name timeshed --no-pack --no-typescript . \
    -- --no-default-features --features wasm

rm -rf docs/live && mkdir -p docs/live/pkg
cp web/index.html web/style.css web/app.js web/worker.js docs/live/
cp web/pkg/timeshed.js web/pkg/timeshed_bg.wasm docs/live/pkg/
# the published page links back to the results map, which the dev server does not have
sed -i '' 's|door to door.</p>|door to door.</p>\n    <p class="hint"><a href="../">See the finding: how job access changes by time of day \&rarr;</a></p>|' docs/live/index.html
# the published page reads its bundle from next to itself
sed -i '' "s|<script src=\"app.js\"></script>|<script>window.TIMESHED_BUNDLE_URL = '$URL';</script>\n  <script src=\"app.js\"></script>|" docs/live/index.html
gzip -9 -k -f -c "$BUNDLE" > docs/live/bundle.bin.gz
ls -la docs/live docs/live/pkg
