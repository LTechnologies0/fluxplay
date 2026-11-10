#!/usr/bin/env bash
# TDD gate: compile-time denies + property tests + libFuzzer (ON by default).
# Usage:
#   ./scripts/tdd-gate.sh                 # full gate (fuzz included when possible)
#   FLUXPLAY_FUZZ=0 ./scripts/tdd-gate.sh # skip libFuzzer (proptest still runs)
#   PROPTEST_CASES=2048 ./scripts/tdd-gate.sh
#   FLUXPLAY_FUZZ_SECS=30 ./scripts/tdd-gate.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export FLUXPLAY_ROOT="$ROOT"

# shellcheck source=resource-budget.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/resource-budget.sh"
fluxplay_export_cargo_jobs
echo "==> resource budget: cargo_jobs=${CARGO_BUILD_JOBS} rayon=${RAYON_NUM_THREADS} fuzz_parallel=$(fluxplay_fuzz_parallel) fuzz_rss=$(fluxplay_fuzz_rss_mb)MB"

# Supply-chain first (lockfile + advisories/bans/licenses/sources).
# Opt out: FLUXPLAY_SUPPLY_CHAIN=0 ./scripts/tdd-gate.sh
if [[ "${FLUXPLAY_SUPPLY_CHAIN:-1}" == "1" ]]; then
  "$ROOT/scripts/supply-chain.sh"
else
  echo "==> supply-chain skipped (FLUXPLAY_SUPPLY_CHAIN=0)"
fi

# Dead-code from WIP modules (browse_tiles / props) must not hide real bugs —
# still deny unused_must_use, unsafe_code surprises, etc.
DENY=( -D warnings -A dead_code )

echo "==> rustc -D warnings -A dead_code --locked (core + providers + player + fluxplay)"
cargo rustc -p fluxplay-core --lib --locked -- "${DENY[@]}"
cargo rustc -p fluxplay-providers --lib --locked -- "${DENY[@]}"
cargo rustc -p fluxplay-player --lib --locked -- "${DENY[@]}"
cargo rustc -p fluxplay --lib --locked -- "${DENY[@]}"

# Clippy: on by default; strict (-D warnings) when FLUXPLAY_CLIPPY_STRICT=1 (CI).
if [[ "${FLUXPLAY_CLIPPY:-1}" == "1" ]]; then
  CLIPPY_DENY=( -A dead_code )
  if [[ "${FLUXPLAY_CLIPPY_STRICT:-0}" == "1" ]]; then
    CLIPPY_DENY=( -D warnings -A dead_code )
  fi
  echo "==> clippy --workspace --locked (${CLIPPY_DENY[*]})"
  cargo clippy --workspace --locked --lib --exclude fluxplay-android -- \
    "${CLIPPY_DENY[@]}"
fi

echo "==> unit + property tests (PROPTEST_CASES=${PROPTEST_CASES:-512})"
export PROPTEST_CASES="${PROPTEST_CASES:-512}"
cargo test -p fluxplay-core --lib --locked
cargo test -p fluxplay-providers --lib --locked
cargo test -p fluxplay-player --lib --locked
cargo test -p fluxplay --lib --locked

# Fuzz ON by default. Opt out with FLUXPLAY_FUZZ=0.
# Needs nightly + cargo-fuzz + clang (libFuzzer).
if [[ "${FLUXPLAY_FUZZ:-1}" == "1" ]]; then
  # shellcheck source=fuzz-common.sh
  # shellcheck disable=SC1091
  source "$ROOT/scripts/fuzz-common.sh"

  if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "installing cargo-fuzz…"
    cargo install cargo-fuzz --locked
  fi

  if ! rustup run nightly rustc --version >/dev/null 2>&1; then
    echo "WARN: nightly toolchain missing — fuzz skipped (rustup toolchain install nightly)" >&2
  else
    SECS="${FLUXPLAY_FUZZ_SECS:-8}"
    fluxplay_fuzz_run_all "$SECS" 8192 5
  fi
else
  echo "==> fuzz skipped (FLUXPLAY_FUZZ=0)"
fi

echo "==> TDD gate OK"
