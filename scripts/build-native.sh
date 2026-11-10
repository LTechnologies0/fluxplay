#!/usr/bin/env bash
# Build FluxPlay with native libmpv (optional static archive).
# Usage:
#   scripts/build-native.sh              # verify + shared libmpv + rpath
#   FLUXPLAY_STATIC_MPV=1 scripts/build-native.sh   # prefer libmpv.a
#   FLUXPLAY_SKIP_VERIFY=1 scripts/build-native.sh  # build only (emergency)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# shellcheck source=resource-budget.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/resource-budget.sh"
fluxplay_export_cargo_jobs

# Host-max codegen (znver3 / apple-m1 / …). Override: FLUXPLAY_CPU=v3|v2|baseline
# shellcheck source=rustc-flags.sh
# shellcheck disable=SC1091
export FLUXPLAY_CPU="${FLUXPLAY_CPU:-native}"
source "$ROOT/scripts/rustc-flags.sh"
fluxplay_export_rustflags

if [[ "${FLUXPLAY_SKIP_VERIFY:-0}" != "1" ]]; then
  echo "==> pre-build verify (TDD + fuzz; FLUXPLAY_SKIP_VERIFY=1 to skip)"
  "$ROOT/scripts/tdd-gate.sh"
fi

FEATURES="native-player"
if [[ "${FLUXPLAY_STATIC_MPV:-}" == "1" ]]; then
  FEATURES="static-mpv"
  export FLUXPLAY_STATIC_MPV=1
  echo "==> static-link mode (needs libmpv.a + optionally FLUXPLAY_MPV_STATIC_DEPS)"
fi

export FLUXPLAY_BUNDLE_RPATH=1

echo "==> cargo build -p fluxplay --release --locked --features $FEATURES (jobs=${CARGO_BUILD_JOBS} cpu=${FLUXPLAY_CPU})"
cargo build -p fluxplay --release --locked --features "$FEATURES"

BIN="$ROOT/target/release/fluxplay"
if [[ -f "$BIN" ]]; then
  if [[ "${FLUXPLAY_STATIC_MPV:-}" != "1" ]]; then
    "$ROOT/scripts/bundle-libmpv.sh" "$BIN" || true
  fi
  echo "==> OK: $BIN"
  ldd "$BIN" 2>/dev/null | grep -i mpv || true
fi
