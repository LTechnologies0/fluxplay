#!/usr/bin/env bash
# Build FluxPlay with native libmpv (optional static archive).
# Usage:
#   scripts/build-native.sh              # shared libmpv + rpath
#   FLUXPLAY_STATIC_MPV=1 scripts/build-native.sh   # prefer libmpv.a
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FEATURES="native-player"
if [[ "${FLUXPLAY_STATIC_MPV:-}" == "1" ]]; then
  FEATURES="static-mpv"
  export FLUXPLAY_STATIC_MPV=1
  echo "==> static-link mode (needs libmpv.a + optionally FLUXPLAY_MPV_STATIC_DEPS)"
fi

export FLUXPLAY_BUNDLE_RPATH=1

echo "==> cargo build -p fluxplay --release --features $FEATURES"
cargo build -p fluxplay --release --features "$FEATURES"

BIN="$ROOT/target/release/fluxplay"
if [[ -f "$BIN" ]]; then
  if [[ "${FLUXPLAY_STATIC_MPV:-}" != "1" ]]; then
    "$ROOT/scripts/bundle-libmpv.sh" "$BIN" || true
  fi
  echo "==> OK: $BIN"
  ldd "$BIN" 2>/dev/null | grep -i mpv || true
fi
