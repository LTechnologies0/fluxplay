#!/usr/bin/env bash
# TDD gate: compile-time denies + property tests (+ optional libFuzzer smoke).
# Usage:
#   ./scripts/tdd-gate.sh           # warnings-as-errors + proptest suite
#   FLUXPLAY_FUZZ=1 ./scripts/tdd-gate.sh   # also short fuzz runs (needs clang)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> rustc -D warnings (core + providers + fluxplay)"
cargo rustc -p fluxplay-core --lib -- -D warnings
cargo rustc -p fluxplay-providers --lib -- -D warnings
cargo rustc -p fluxplay --lib -- -D warnings

echo "==> unit + property tests"
cargo test -p fluxplay-core --lib
cargo test -p fluxplay-providers --lib
cargo test -p fluxplay --lib

if [[ "${FLUXPLAY_FUZZ:-0}" == "1" ]]; then
  if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "installing cargo-fuzz…"
    cargo install cargo-fuzz --locked
  fi
  # cargo-fuzz overrides `.cargo/config.toml` rustflags — keep dalek serial.
  export RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" ${RUSTFLAGS:-}"
  echo "==> fuzz smoke (10s / target)"
  (
    cd fuzz
    for t in fuzz_m3u fuzz_stream_url fuzz_xmltv fuzz_titles fuzz_browse_tiles fuzz_wg_profile fuzz_sanitize; do
      echo "— $t"
      cargo fuzz run "$t" -- -max_total_time=10 -max_len=4096
    done
  )
fi

echo "==> TDD gate OK"
