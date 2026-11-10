#!/usr/bin/env bash
# Shared helpers for tdd-gate / fuzz-long (sourced, not executed alone).
# shellcheck shell=bash

# Always resolve against the FluxPlay repo root — callers may `cd fuzz`.
fluxplay_fuzz_init_root() {
  if [[ -z "${FLUXPLAY_ROOT:-}" ]]; then
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    export FLUXPLAY_ROOT="$here"
  fi
  export FLUXPLAY_FUZZ_DIR="${FLUXPLAY_FUZZ_DIR:-$FLUXPLAY_ROOT/fuzz}"
}

fluxplay_fuzz_list_targets() {
  fluxplay_fuzz_init_root
  local toml="$FLUXPLAY_FUZZ_DIR/Cargo.toml"
  # Prefer Cargo.toml [[bin]] names — stays in sync when targets are added.
  if [[ -f "$toml" ]]; then
    awk '
      /^\[\[bin\]\]/ { inbin=1; next }
      inbin && /^name[[:space:]]*=/ {
        gsub(/"/, "", $3);
        if ($3 != "") print $3;
        inbin=0;
      }
    ' "$toml"
    return 0
  fi
  # Fallback: scan sources (never invoke basename with an empty operand).
  local f base
  shopt -s nullglob
  for f in "$FLUXPLAY_FUZZ_DIR"/fuzz_targets/fuzz_*.rs; do
    base="$(basename "$f" .rs)"
    [[ -n "$base" ]] && printf '%s\n' "$base"
  done
  shopt -u nullglob
}

fluxplay_fuzz_bootstrap_seeds() {
  # Copy curated seeds into libFuzzer corpus dirs (corpus/ is gitignored).
  fluxplay_fuzz_init_root
  local src dst t f
  [[ -d "$FLUXPLAY_FUZZ_DIR/seeds" ]] || return 0
  shopt -s nullglob
  for src in "$FLUXPLAY_FUZZ_DIR"/seeds/*/; do
    [[ -d "$src" ]] || continue
    t="$(basename "${src%/}")"
    [[ -n "$t" ]] || continue
    dst="$FLUXPLAY_FUZZ_DIR/corpus/$t"
    mkdir -p "$dst"
    # Seed empty corpora; never clobber evolved non-seed units.
    if [[ -z "$(find "$dst" -maxdepth 1 -type f 2>/dev/null | head -1)" ]]; then
      cp -a "$src". "$dst/" 2>/dev/null || cp -a "$src"* "$dst/" 2>/dev/null || true
    else
      for f in "$src"seed*; do
        [[ -f "$f" ]] || continue
        cp -a "$f" "$dst/"
      done
    fi
  done
  shopt -u nullglob
}

fluxplay_fuzz_dict_for() {
  # Absolute path (or empty). Safe to use after `cd fuzz`.
  fluxplay_fuzz_init_root
  local t="$1"
  local d=""
  case "$t" in
    fuzz_m3u|fuzz_hls_body) d="m3u.dict" ;;
    fuzz_xmltv) d="xmltv.dict" ;;
    fuzz_wg_profile) d="wg.dict" ;;
    fuzz_stream_url|fuzz_xtream_url|fuzz_image_url|fuzz_downloads|fuzz_redact|fuzz_route)
      d="url.dict"
      ;;
    fuzz_titles|fuzz_metadata|fuzz_chapters) d="titles.dict" ;;
    fuzz_sanitize) d="sanitize.dict" ;;
    fuzz_persist) d="json.dict" ;;
    *) d="" ;;
  esac
  if [[ -n "$d" && -f "$FLUXPLAY_FUZZ_DIR/dictionaries/$d" ]]; then
    echo "$FLUXPLAY_FUZZ_DIR/dictionaries/$d"
  else
    echo ""
  fi
}

# Shared libFuzzer knobs that hunt deeper bugs (value profile + cross-over).
# Args: secs [max_len] [timeout_s] [rss_mb]
fluxplay_fuzz_engine_args() {
  local secs="$1"
  local max_len="${2:-8192}"
  local timeout_s="${3:-5}"
  local rss_mb="${4:-}"
  if [[ -z "$rss_mb" ]]; then
    # shellcheck source=resource-budget.sh
    # shellcheck disable=SC1091
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/resource-budget.sh"
    rss_mb="$(fluxplay_fuzz_rss_mb)"
  fi
  printf '%s\n' \
    "-max_total_time=${secs}" \
    "-max_len=${max_len}" \
    "-timeout=${timeout_s}" \
    "-rss_limit_mb=${rss_mb}" \
    "-use_value_profile=1" \
    "-use_cmp=1" \
    "-entropic=1" \
    "-cross_over=1" \
    "-mutate_depth=10"
}

# Parallel runner: runs up to N fuzz targets at once, fails on first crash.
# Args: secs [max_len] [timeout_s]
fluxplay_fuzz_run_all() {
  local secs="$1"
  local max_len="${2:-8192}"
  local timeout_s="${3:-5}"
  # shellcheck source=resource-budget.sh
  # shellcheck disable=SC1091
  source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/resource-budget.sh"
  local jobs rss
  jobs="$(fluxplay_fuzz_parallel)"
  rss="$(fluxplay_fuzz_rss_mb)"

  fluxplay_fuzz_init_root
  fluxplay_fuzz_bootstrap_seeds
  mapfile -t TARGETS < <(fluxplay_fuzz_list_targets)
  if [[ "${#TARGETS[@]}" -eq 0 ]]; then
    echo "FAIL: no fuzz targets discovered" >&2
    return 1
  fi

  export RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" ${RUSTFLAGS:-}"
  export CARGO_BUILD_RUSTFLAGS="--cfg curve25519_dalek_backend=\"serial\" ${CARGO_BUILD_RUSTFLAGS:-}"
  export CARGO_BUILD_JOBS
  CARGO_BUILD_JOBS="$(fluxplay_cpu_jobs)"

  mkdir -p /tmp/fluxplay-fuzz-artifacts
  rm -f /tmp/fluxplay-fuzz-fail-*

  local cargo_fuzz=(cargo)
  if rustup run nightly rustc --version >/dev/null 2>&1; then
    cargo_fuzz=(cargo +nightly)
  fi

  echo "==> fuzz ${secs}s × ${#TARGETS[@]} targets (parallel=${jobs}, rss=${rss}MB, cargo_jobs=${CARGO_BUILD_JOBS}, ${cargo_fuzz[*]} fuzz)"

  run_one() {
    local t="$1"
    local log="/tmp/fuzz_smoke_${t}.log"
    local dict_args=()
    local d
    d="$(fluxplay_fuzz_dict_for "$t")"
    if [[ -n "$d" ]]; then
      dict_args=( -dict="$d" )
    fi
    mapfile -t engine < <(fluxplay_fuzz_engine_args "$secs" "$max_len" "$timeout_s" "$rss")
    echo "— $t"
    (
      cd "$FLUXPLAY_FUZZ_DIR"
      if ! "${cargo_fuzz[@]}" fuzz run "$t" -- \
        "${engine[@]}" \
        -artifact_prefix="/tmp/fluxplay-fuzz-artifacts/${t}_" \
        "${dict_args[@]}" \
        >"$log" 2>&1; then
        command grep -a -E 'panicked at|ParseDictionary|ERROR:' "$log" | head -20 >&2 || true
        tail -40 "$log" >&2 || true
        echo "FAIL: $t crashed" >&2
        touch "/tmp/fluxplay-fuzz-fail-$t"
        return 1
      fi
    )
    if command grep -a -E -q 'ERROR: libFuzzer|AddressSanitizer|ASSERTION FAILED|SUMMARY: AddressSanitizer|Test unit written to|LeakSanitizer' "$log"; then
      command grep -a -E 'panicked at|Test unit written' "$log" | head -20 >&2 || true
      tail -40 "$log" >&2 || true
      echo "FAIL: $t sanitizer/crash signal" >&2
      touch "/tmp/fluxplay-fuzz-fail-$t"
      return 1
    fi
    command grep -a -E 'DONE|Done |cov: |feat: |exec/s:' "$log" | tail -2 || true
    return 0
  }

  local t pids=()
  for t in "${TARGETS[@]}"; do
    [[ -n "$t" ]] || continue
    while [[ "$(jobs -rp | wc -l)" -ge "$jobs" ]]; do
      sleep 1
    done
    ( run_one "$t" ) &
    pids+=( "$!" )
  done
  local pid
  for pid in "${pids[@]}"; do
    wait "$pid" || true
  done
  if compgen -G "/tmp/fluxplay-fuzz-fail-*" > /dev/null; then
    echo "FAIL: one or more fuzz targets crashed" >&2
    return 1
  fi
  return 0
}
