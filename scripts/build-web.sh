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

# Everything but the pages lives under a content-hashed path so the host can
# cache it for a year; the pages are short-lived and always name the current files.
rm -rf docs/pkg docs/bundle docs/assets docs/live docs/findings docs/app.js docs/style.css docs/worker.js docs/bundle.bin.gz
mkdir -p docs/assets
hashed() { h=$(shasum -a 256 "$1" | cut -c1-12); name=$(basename "$1"); out="assets/${name%.*}.$h.${name##*.}"; cp "$1" "docs/$out"; echo "$out"; }
APP=$(hashed web/app.js); STYLE=$(hashed web/style.css); WORKER=$(hashed web/worker.js)
PKG_HASH=$(shasum -a 256 web/pkg/timeshed_bg.wasm | cut -c1-12)
mkdir -p "docs/pkg/$PKG_HASH"
cp web/pkg/timeshed.js web/pkg/timeshed_bg.wasm "docs/pkg/$PKG_HASH/"
mkdir -p docs/bundle
gzip -9 -k -f -c "$BUNDLE" > docs/bundle/tmp.bin.gz
BUNDLE_HASH=$(shasum -a 256 docs/bundle/tmp.bin.gz | cut -c1-12)
mv docs/bundle/tmp.bin.gz "docs/bundle/$BUNDLE_HASH.bin.gz"
python3 scripts/render-pages.py "bundle/$BUNDLE_HASH.bin.gz" "pkg/$PKG_HASH/timeshed.js" "$APP" "$STYLE" "$WORKER"
ls -la docs docs/assets "docs/pkg/$PKG_HASH" docs/bundle docs/findings docs/live
