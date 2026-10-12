#!/bin/sh
# Build the browser engine and assemble the published site under docs/.
#   scripts/build-web.sh <bundle.bin>
# <bundle.bin> is a bundle from `timeshed build`. It is gzipped next to the
# page: GitHub release assets are served without CORS headers, so the bundle
# has to come from the same origin as the page.
set -eu
cd "$(dirname "$0")/.."
BUNDLE=${1:?bundle path}
export PATH="$HOME/.cargo/bin:$PATH"
# rust-lld looks for libLLVM next to itself; some rustup installs keep it one
# level up. Harmless where the toolchain is fine. (macOS strips DYLD_* from a
# caller's environment, so it has to be set here, not before the script.)
export DYLD_FALLBACK_LIBRARY_PATH="$(rustc --print sysroot)/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"

wasm-pack build --target web --release --out-dir web/pkg --out-name timeshed --no-pack --no-typescript . \
    -- --no-default-features --features wasm

rm -rf docs/pkg docs/live docs/findings && mkdir -p docs/pkg
cp web/style.css web/app.js web/worker.js docs/
cp web/pkg/timeshed.js web/pkg/timeshed_bg.wasm docs/pkg/
gzip -9 -k -f -c "$BUNDLE" > docs/bundle.bin.gz
python3 scripts/render-pages.py
ls -la docs docs/pkg docs/findings docs/live
