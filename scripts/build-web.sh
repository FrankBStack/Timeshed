#!/bin/sh
# Build the browser engine and assemble the published site under docs/.
#   scripts/build-web.sh milwaukee=data/bundles/milwaukee.bin [cincinnati=data/bundles/cincinnati.bin ...]
# Each argument names a city from cities.json and a bundle from `timeshed build`.
# render-pages.py gzips the bundles, hashes every asset and writes the pages.
set -eu
cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { echo "usage: $0 city=bundle.bin [city=bundle.bin ...]" >&2; exit 2; }
export PATH="$HOME/.cargo/bin:$PATH"
# rust-lld looks for libLLVM next to itself; some rustup installs keep it one
# level up. Harmless where the toolchain is fine. (macOS strips DYLD_* from a
# caller's environment, so it has to be set here, not before the script.)
export DYLD_FALLBACK_LIBRARY_PATH="$(rustc --print sysroot)/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"

wasm-pack build --target web --release --out-dir web/pkg --out-name timeshed --no-pack --no-typescript . \
    -- --no-default-features --features wasm

python3 scripts/render-pages.py "$@"
ls -la docs docs/assets docs/bundle
