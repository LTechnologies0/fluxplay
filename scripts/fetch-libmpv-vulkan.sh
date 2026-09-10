#!/usr/bin/env bash
# Phase D — optional Vulkan / libplacebo libmpv drop-in for Android HDR gpu-next.
#
# FluxPlay ships media-kit libmpv built with -Dvulkan=disabled -Dlibplacebo=disabled.
# Native HDR via Surface already works with vo=mediacodec_embed (Phase A) without Vulkan.
# This script documents how to replace vendor/android-native/*/libmpv.so with a
# Vulkan-enabled build when you need vo=gpu-next + androidvk (experimental).
#
# Upstream reference (CI that produced our prebuilts):
#   https://github.com/media-kit/libmpv-android-video-build
#
# Usage:
#   1. Build or download arm64-v8a (+ optional x86_64) libmpv.so with:
#        meson: -Dvulkan=enabled -Dlibplacebo=enabled
#        FFmpeg: --enable-vulkan --enable-mediacodec
#   2. ./scripts/fetch-libmpv-vulkan.sh /path/to/arm64/libmpv.so [/path/to/x86_64/libmpv.so]
#   3. Rebuild APK: ./scripts/build-android-apk.sh --target aarch64-linux-android
#   4. Runtime: FLUXPLAY_ANDROID_VO=gpu  (uses vo=gpu / egl; gpu-next if present)
#
# Without a Vulkan rebuild, Phase A SurfaceEmbed remains the supported 4K/HDR path.

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST_ARM="$ROOT/vendor/android-native/arm64-v8a/libmpv.so"
DEST_X86="$ROOT/vendor/android-native/x86_64/libmpv.so"

if [[ $# -lt 1 ]]; then
  cat <<'EOF'
Usage: fetch-libmpv-vulkan.sh <arm64-libmpv.so> [x86_64-libmpv.so]

Replaces vendored libmpv.so for optional Vulkan/libplacebo experiments (Phase D).
Keep a backup; Surface mediacodec_embed does not require this.
EOF
  exit 1
fi

ARM_SRC="$1"
X86_SRC="${2:-}"

if [[ ! -f "$ARM_SRC" ]]; then
  echo "missing arm64 libmpv: $ARM_SRC" >&2
  exit 1
fi

ts="$(date +%Y%m%d%H%M%S)"
if [[ -f "$DEST_ARM" ]]; then
  cp -a "$DEST_ARM" "${DEST_ARM}.bak.${ts}"
  echo "backed up → ${DEST_ARM}.bak.${ts}"
fi
cp -a "$ARM_SRC" "$DEST_ARM"
echo "installed arm64 → $DEST_ARM"
file "$DEST_ARM" || true
strings "$DEST_ARM" | rg -i 'vulkan|libplacebo|gpu-next|mediacodec_embed' | sort -u | head -20 || true

if [[ -n "$X86_SRC" ]]; then
  if [[ ! -f "$X86_SRC" ]]; then
    echo "missing x86_64 libmpv: $X86_SRC" >&2
    exit 1
  fi
  if [[ -f "$DEST_X86" ]]; then
    cp -a "$DEST_X86" "${DEST_X86}.bak.${ts}"
  fi
  cp -a "$X86_SRC" "$DEST_X86"
  echo "installed x86_64 → $DEST_X86"
fi

echo
echo "Next: ./scripts/build-android-apk.sh --target aarch64-linux-android"
echo "Run with: FLUXPLAY_ANDROID_VO=gpu  (or present=surface for mediacodec_embed)"
