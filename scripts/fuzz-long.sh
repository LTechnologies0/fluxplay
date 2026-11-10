#!/usr/bin/env bash
# Long fuzz / property stress. Exit non-zero on crash or failed property.
# Usage:
#   ./scripts/fuzz-long.sh [seconds_per_libfuzzer_target]
#   FLUXPLAY_FUZZ_JOBS=2 ./scripts/fuzz-long.sh 300
#   PROPTEST_CASES=20000 ./scripts/fuzz-long.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export FLUXPLAY_ROOT="$ROOT"
SECS="${1:-180}"

# shellcheck source=resource-budget.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/resource-budget.sh"
fluxplay_export_cargo_jobs

# shellcheck source=fuzz-common.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/fuzz-common.sh"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  cargo install cargo-fuzz --locked
fi

fluxplay_fuzz_run_all "$SECS" 16384 8

echo "==> proptest stress (PROPTEST_CASES=${PROPTEST_CASES:-20000})"
export PROPTEST_CASES="${PROPTEST_CASES:-20000}"
cargo test -p fluxplay-core --lib -- prop_ protocol::tests::stream m3u::tests::parse
cargo test -p fluxplay-providers --lib -- prop_xtream
cargo test -p fluxplay-player --lib -- prop_route
cargo test -p fluxplay --lib -- \
  prop_security prop_browser prop_names prop_surface prop_metadata prop_persist prop_skip invariants::tests

echo "==> fuzz-long OK"
