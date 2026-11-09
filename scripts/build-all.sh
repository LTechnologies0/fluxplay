#!/usr/bin/env bash
# One-shot multi-target build for FluxPlay (Linux desktop + Android APK).
# Windows / macOS artifacts: use CI (release-desktop.yml) or native hosts.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PROFILE="${FLUXPLAY_PROFILE:-release}"
DO_ANDROID="${FLUXPLAY_BUILD_ANDROID:-1}"
DO_DESKTOP="${FLUXPLAY_BUILD_DESKTOP:-1}"

echo "==> FluxPlay build-all (profile=$PROFILE android=$DO_ANDROID desktop=$DO_DESKTOP)"

if [[ "$DO_DESKTOP" == "1" ]]; then
  echo "==> Desktop (native-player)"
  if [[ "$(uname -s)" == "Linux" ]]; then
    "$ROOT/scripts/build-native.sh"
  else
    cargo build -p fluxplay --profile "$PROFILE" --features native-player
  fi
  DESKTOP_BIN="$ROOT/target/${PROFILE}/fluxplay"
  if [[ -f "$ROOT/target/release/fluxplay" && "$PROFILE" == "release" ]]; then
    DESKTOP_BIN="$ROOT/target/release/fluxplay"
  fi
  if [[ -f "$DESKTOP_BIN" ]]; then
    mkdir -p "$ROOT/dist"
    cp -a "$DESKTOP_BIN" "$ROOT/dist/fluxplay"
    if [[ -f "$ROOT/packaging/linux/fluxplay.desktop" ]]; then
      cp -a "$ROOT/packaging/linux/fluxplay.desktop" "$ROOT/dist/"
    fi
    echo "==> Desktop artifact: $ROOT/dist/fluxplay"
  fi
fi

if [[ "$DO_ANDROID" == "1" ]]; then
  if [[ -z "${ANDROID_HOME:-}" && -z "${ANDROID_SDK_ROOT:-}" ]]; then
    echo "WARN: ANDROID_HOME unset — skip Android (set FLUXPLAY_BUILD_ANDROID=0 to silence)" >&2
  else
    echo "==> Android APK (phone + leanback TV)"
    "$ROOT/scripts/build-android-apk.sh"
    APK="$(find "$ROOT/target" -type f -path '*/apk/FluxPlay.apk' -printf '%T@\t%p\n' 2>/dev/null | sort -nr | head -n 1 | cut -f2- || true)"
    if [[ -n "${APK:-}" && -f "$APK" ]]; then
      mkdir -p "$ROOT/dist"
      cp -a "$APK" "$ROOT/dist/FluxPlay.apk"
      echo "==> Android artifact: $ROOT/dist/FluxPlay.apk"
    fi
  fi
fi

echo "==> Done. Artifacts under $ROOT/dist/ (when built)."
