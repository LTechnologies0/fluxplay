#!/usr/bin/env bash
# Optimal rustc / Cargo flags per OS × arch × CPU.
#
# Usage:
#   source scripts/rustc-flags.sh          # export into current shell
#   FLUXPLAY_CPU=native source scripts/rustc-flags.sh
#   FLUXPLAY_CPU=v3     source scripts/rustc-flags.sh   # portable shipping floor
#   FLUXPLAY_CPU=v2     source scripts/rustc-flags.sh   # older x86_64
#   FLUXPLAY_CPU=baseline source scripts/rustc-flags.sh # rustc default CPU
#
# Sets (when sourced or run with --export):
#   RUSTFLAGS / CARGO_BUILD_RUSTFLAGS
#   CARGO_TARGET_<TRIPLE>_RUSTFLAGS (when FLUXPLAY_TARGET is set)
#
# Always keeps curve25519-dalek on the serial backend (nightly-safe).
# shellcheck shell=bash

fluxplay_curve25519_cfg() {
  echo '--cfg curve25519_dalek_backend="serial"'
}

# Host triple from rustc (falls back to uname heuristics).
fluxplay_host_triple() {
  local host=""
  if command -v rustc >/dev/null 2>&1; then
    host="$(rustc -vV 2>/dev/null | awk '/^host:/{print $2; exit}')"
  fi
  if [[ -n "$host" ]]; then
    printf '%s\n' "$host"
    return
  fi
  case "$(uname -s 2>/dev/null)-$(uname -m 2>/dev/null)" in
    Linux-x86_64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    Linux-aarch64|Linux-arm64) printf '%s\n' aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) printf '%s\n' x86_64-apple-darwin ;;
    Darwin-arm64) printf '%s\n' aarch64-apple-darwin ;;
    MINGW*|MSYS*|CYGWIN*) printf '%s\n' x86_64-pc-windows-msvc ;;
    *) printf '%s\n' "" ;;
  esac
}

# Microarch / feature set for a triple + policy.
# FLUXPLAY_CPU: native | v4 | v3 | v2 | baseline | (empty → native for local, v3 for release scripts)
fluxplay_cpu_flags() {
  local triple="${1:-}"
  local policy="${FLUXPLAY_CPU:-native}"
  local arch os
  arch="${triple%%-*}"
  case "$triple" in
    *-linux-android*|*-androideabi*) os=android ;;
    *-apple-darwin*|*-apple-ios*) os=apple ;;
    *-pc-windows-*) os=windows ;;
    *-unknown-linux-*|*-linux-gnu*|*-linux-musl*) os=linux ;;
    *) os=other ;;
  esac

  case "$policy" in
    native)
      # Host-only — never use for cross or published CI artifacts.
      echo "-C target-cpu=native"
      return
      ;;
  esac

  case "$arch" in
    x86_64|x86-64)
      case "$policy" in
        v4) echo "-C target-cpu=x86-64-v4" ;;
        v3|"") echo "-C target-cpu=x86-64-v3" ;;
        v2) echo "-C target-cpu=x86-64-v2" ;;
        baseline) ;; # rustc default
        *) echo "-C target-cpu=x86-64-v3" ;;
      esac
      ;;
    aarch64|arm64)
      case "$os" in
        android)
          # armv8.2-a +crc +dotprod: 2019+ phones/TVs; fat APK still runs on
          # older devices via separate ABIs if we only ship arm64-v8a (NDK
          # minimum). Prefer cortex-a76 codegen for HQ decode path cost.
          case "$policy" in
            baseline) echo "-C target-feature=+neon,+fp-armv8,+crc" ;;
            *) echo "-C target-cpu=cortex-a76" ;;
          esac
          ;;
        apple)
          # Apple Silicon floor: apple-a14 ≡ M1 / A14 Firestorm (rustc has no apple-m1).
          case "$policy" in
            baseline) ;;
            *) echo "-C target-cpu=apple-a14" ;;
          esac
          ;;
        *)
          # Server / Pi 4 class: Neoverse-N1 ≈ Cortex-A76.
          case "$policy" in
            baseline) echo "-C target-feature=+neon,+fp-armv8,+crc" ;;
            *) echo "-C target-cpu=neoverse-n1" ;;
          esac
          ;;
      esac
      ;;
    arm|armv7)
      echo "-C target-feature=+neon,+vfp4"
      ;;
    i686|i586)
      echo "-C target-cpu=pentium4"
      ;;
  esac
}

# Linker / codegen extras that are safe per OS (release-oriented).
fluxplay_os_link_flags() {
  local triple="${1:-}"
  case "$triple" in
    *-unknown-linux-gnu|*-linux-gnu)
      # Pack + drop unused DSOs; RELRO+NOW for hardened runtime.
      echo "-C link-arg=-Wl,-O1"
      echo "-C link-arg=-Wl,--as-needed"
      echo "-C link-arg=-Wl,-z,relro,-z,now"
      echo "-C link-arg=-Wl,--gc-sections"
      ;;
    *-unknown-linux-musl|*-linux-musl)
      echo "-C link-arg=-Wl,-O1"
      echo "-C link-arg=-Wl,--gc-sections"
      echo "-C target-feature=+crt-static"
      ;;
    *-apple-darwin)
      echo "-C link-arg=-Wl,-dead_strip"
      ;;
    *-pc-windows-msvc)
      echo "-C link-arg=/OPT:REF"
      echo "-C link-arg=/OPT:ICF"
      echo "-C control-flow-guard=yes"
      ;;
    *-linux-android*|*-androideabi*)
      # NDK lld: size + GC; PIC is default for Android.
      echo "-C link-arg=-Wl,--gc-sections"
      echo "-C link-arg=-Wl,-O1"
      echo "-C link-arg=-Wl,--as-needed"
      ;;
  esac
  # panic=abort profiles already strip unwind; keep frames off for size/speed.
  echo "-C force-frame-pointers=no"
}

# Build a single rustflags string for triple.
fluxplay_rustflags_for() {
  local triple="${1:-}"
  local parts=()
  local f
  # shellcheck disable=SC2207
  parts+=( $(fluxplay_curve25519_cfg) )
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    # Split "-C foo=bar" into two argv words for RUSTFLAGS.
    # shellcheck disable=SC2206
    parts+=( $f )
  done < <(fluxplay_cpu_flags "$triple"; fluxplay_os_link_flags "$triple")
  printf '%s' "${parts[*]}"
}

# Export into the environment for subsequent cargo invocations.
fluxplay_export_rustflags() {
  local triple="${FLUXPLAY_TARGET:-$(fluxplay_host_triple)}"
  local flags
  flags="$(fluxplay_rustflags_for "$triple")"

  export RUSTFLAGS="$flags${RUSTFLAGS:+ ${RUSTFLAGS}}"
  export CARGO_BUILD_RUSTFLAGS="$flags${CARGO_BUILD_RUSTFLAGS:+ ${CARGO_BUILD_RUSTFLAGS}}"

  if [[ -n "$triple" ]]; then
    # CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS
    local env_key
    env_key="$(printf '%s' "$triple" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"
    # shellcheck disable=SC2086
    export CARGO_TARGET_${env_key}_RUSTFLAGS="$flags"
  fi

  if [[ "${FLUXPLAY_RUSTFLAGS_QUIET:-0}" != "1" ]]; then
    echo "==> rustc flags (${FLUXPLAY_CPU:-native}) target=${triple:-host}"
    echo "    $flags"
  fi
}

# Print only (for debugging / CI logs).
fluxplay_print_rustflags() {
  local triple="${1:-${FLUXPLAY_TARGET:-$(fluxplay_host_triple)}}"
  fluxplay_rustflags_for "$triple"
  echo
}

# When executed (not sourced): print or export.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  case "${1:-}" in
    --export)
      fluxplay_export_rustflags
      ;;
    --print)
      shift || true
      fluxplay_print_rustflags "${1:-}"
      ;;
    -h|--help)
      sed -n '2,20p' "$0"
      ;;
    *)
      fluxplay_print_rustflags "$@"
      ;;
  esac
fi
