#!/usr/bin/env bash
# Shared CPU / RAM budgets so verify + fuzz cannot OOM the host.
# shellcheck shell=bash

fluxplay_cpu_jobs() {
  local n j
  n="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"
  # Leave ≥ half the cores free for the desktop / compositor.
  j=$((n / 2))
  if [[ "$j" -lt 1 ]]; then j=1; fi
  if [[ "$j" -gt 4 ]]; then j=4; fi
  echo "${CARGO_BUILD_JOBS:-${FLUXPLAY_JOBS:-$j}}"
}

fluxplay_fuzz_parallel() {
  local j
  j="$(fluxplay_cpu_jobs)"
  # Fuzz binaries are fat (iced); never run more than 3 in parallel on a laptop.
  if [[ "$j" -gt 3 ]]; then j=3; fi
  echo "${FLUXPLAY_FUZZ_JOBS:-$j}"
}

# Per-process RSS for libFuzzer (MB). Leave ~3 GiB for OS + cargo.
fluxplay_fuzz_rss_mb() {
  local avail_kb rss
  avail_kb="$(awk '/MemAvailable:/ {print $2}' /proc/meminfo 2>/dev/null || echo 8388608)"
  # Use at most ~1/3 of available RAM per fuzz job, capped.
  rss=$((avail_kb / 1024 / 3))
  if [[ "$rss" -lt 512 ]]; then rss=512; fi
  if [[ "$rss" -gt 1536 ]]; then rss=1536; fi
  echo "${FLUXPLAY_FUZZ_RSS_MB:-$rss}"
}

fluxplay_export_cargo_jobs() {
  export CARGO_BUILD_JOBS
  CARGO_BUILD_JOBS="$(fluxplay_cpu_jobs)"
  # proptest / rayon style parallelism when crates honor it
  export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-$CARGO_BUILD_JOBS}"
}
