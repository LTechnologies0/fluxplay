#!/usr/bin/env bash
# Build FluxPlay Android APK (cargo-apk) then inject Java bridge (SAF / PiP / insets).
# Default: fat APK for all build_targets (arm64-v8a + x86_64). Pass --target to narrow.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
# Prefer ANDROID_HOME; some toolchains still read ANDROID_SDK_ROOT.
export ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-$ANDROID_HOME}"
# User-local apktool (PiP manifest patch) when not on system PATH.
export PATH="${HOME}/.local/bin:${PATH}"

# cargo-apk signing path is crate-relative (see fluxplay-android Cargo.toml).
KS_CRATE="$ROOT/crates/fluxplay-android/debug.keystore"
if [[ ! -f "$KS_CRATE" ]]; then
  mkdir -p "$HOME/.android"
  if [[ -f "$HOME/.android/debug.keystore" ]]; then
    cp -a "$HOME/.android/debug.keystore" "$KS_CRATE"
  else
    keytool -genkeypair -v \
      -keystore "$KS_CRATE" \
      -storepass android -alias androiddebugkey \
      -keypass android -keyalg RSA -keysize 2048 -validity 10000 \
      -dname "CN=Android Debug,O=Android,C=US"
    cp -a "$KS_CRATE" "$HOME/.android/debug.keystore"
  fi
fi
# Keep inject-script default ($HOME/.android/...) aligned when present.
export FLUXPLAY_ANDROID_KS="${FLUXPLAY_ANDROID_KS:-$KS_CRATE}"

echo "building APK (Cargo.toml build_targets unless --target passed)…"
cargo apk build -p fluxplay-android --profile release-ci "$@"

APK=""
# Prefer newest FluxPlay.apk under target/*/apk/ (profile or triple layout).
APK="$(find "$ROOT/target" -type f -path '*/apk/FluxPlay.apk' -printf '%T@\t%p\n' 2>/dev/null | sort -nr | head -n 1 | cut -f2- || true)"

if [[ -z "$APK" || ! -f "$APK" ]]; then
  echo "ERROR: FluxPlay.apk not found under target/*/apk/" >&2
  exit 1
fi

echo "built: $APK"
unzip -l "$APK" | grep -E 'lib/.*/lib(fluxplay_android|mpv)\.so' || true
"$ROOT/scripts/inject-android-java.sh" "$APK"

if ! unzip -l "$APK" | grep -E 'classes\.dex$' >/dev/null; then
  echo "ERROR: classes.dex missing after inject: $APK" >&2
  exit 1
fi

echo "ready: $APK"
