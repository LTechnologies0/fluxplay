#!/usr/bin/env bash
# Long fuzz / property stress. Exit non-zero on crash or failed property.
# Usage: ./scripts/fuzz-long.sh [seconds_per_libfuzzer_target]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SECS="${1:-120}"

# cargo-fuzz replaces `.cargo/config.toml` [build].rustflags with its own
# RUSTFLAGS (sancov/ASAN). Prepend dalek serial so WireGuard deps still build.
export RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" ${RUSTFLAGS:-}"
export CARGO_BUILD_RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" ${CARGO_BUILD_RUSTFLAGS:-}"

TARGETS=(
  fuzz_m3u
  fuzz_stream_url
  fuzz_xmltv
  fuzz_titles
  fuzz_browse_tiles
  fuzz_wg_profile
  fuzz_sanitize
)

echo "==> libFuzzer ${SECS}s / target (${#TARGETS[@]} targets)"
if ! command -v cargo-fuzz >/dev/null 2>&1; then
  cargo install cargo-fuzz --locked
fi
mkdir -p /tmp/fluxplay-fuzz-artifacts
(
  cd fuzz
  for t in "${TARGETS[@]}"; do
    echo "—— $t (${SECS}s) ——"
    log="/tmp/fuzz_${t}.log"
    if ! cargo fuzz run "$t" -- \
        -max_total_time="$SECS" \
        -max_len=8192 \
        -timeout=5 \
        -rss_limit_mb=2048 \
        -artifact_prefix="/tmp/fluxplay-fuzz-artifacts/${t}_" \
        >"$log" 2>&1; then
      tail -80 "$log" >&2 || true
      echo "FAIL: $t crashed — artifacts under /tmp/fluxplay-fuzz-artifacts/" >&2
      exit 1
    fi
    # libFuzzer can exit 0 after timeout even with a prior crash — scan the log.
    if command grep -E -q 'ERROR:|ASSERTION|AddressSanitizer|SUMMARY: |Test unit written|crash-' "$log"; then
      if command grep -E -q 'Test unit written|crash-|AddressSanitizer|ASSERTION|SUMMARY: ' "$log"; then
        tail -80 "$log" >&2 || true
        echo "FAIL: $t reported a crash in log" >&2
        exit 1
      fi
    fi
    command grep -E 'Done |ERROR:|FAIL' "$log" | tail -5 || true
  done
)

echo "==> proptest stress (app / security / UI) PROPTEST_CASES=10000"
export PROPTEST_CASES="${PROPTEST_CASES:-10000}"
cargo test -p fluxplay-core --lib -- prop_ protocol::tests::stream m3u::tests::parse
cargo test -p fluxplay-providers --lib -- prop_xtream
cargo test -p fluxplay --lib -- \
  prop_security prop_browser prop_names prop_surface prop_metadata invariants::tests

echo "==> fuzz-long OK"
