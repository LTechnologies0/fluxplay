#!/usr/bin/env bash
# Bundle FFmpeg shared libs (libavcodec/libavformat/libavutil/libswscale/libswresample)
# + their non-system transitive deps next to the FluxPlay binary, so the embedded
# native-ffmpeg backend works on machines without ffmpeg-libs installed.
# Usage: scripts/bundle-ffmpeg.sh [path-to-fluxplay-binary]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-"$ROOT/target/release/fluxplay"}"
if [[ ! -f "$BIN" ]]; then
  echo "binary not found: $BIN" >&2
  exit 1
fi

OUT_DIR="$(cd "$(dirname "$BIN")" && pwd)"
LIB_DIR="$OUT_DIR/lib"
mkdir -p "$LIB_DIR"

CORE_LIBS=(libavcodec libavformat libavutil libswscale libswresample)

# Never bundle these — they must come from the host (glibc, graphics stack, drivers).
EXCLUDE_RE='^(ld-linux[^/]*|ld-musl[^/]*|libc\.so|libm\.so|libmvec\.so|libdl\.so|librt\.so|libpthread\.so|libresolv\.so|libnsl\.so|libutil\.so|libgcc_s\.so|libstdc\+\+\.so|libGL\.so|libEGL\.so|libGLESv2\.so|libvulkan\.so|libwayland-|libX11\.so|libxcb\.so|libdrm\.so|libgbm\.so|libexpat\.so|libz\.so|libbz2\.so|liblzma\.so|libzstd\.so|libffi\.so|libselinux\.so|libpcre2?-8\.so|libmount\.so|libblkid\.so|libuuid\.so)'

find_lib() {
  local name="$1"
  if [[ -n "${FFMPEG_LIB_DIR:-}" ]]; then
    for f in "${FFMPEG_LIB_DIR}/${name}".so.*; do
      [[ -e "$f" ]] && { echo "$f"; return; }
    done
  fi
  if command -v ldconfig >/dev/null 2>&1; then
    ldconfig -p 2>/dev/null | awk -v n="${name}.so" '$1 ~ n {print $NF; exit}'
  fi
}

copy_real() {
  # Copy resolved file under its SONAME basename; idempotent.
  local src="$1"
  local real="$src"
  if readlink -f "$src" >/dev/null 2>&1; then
    real="$(readlink -f "$src")"
  fi
  local base
  base="$(basename "$src")"
  if [[ ! -e "$LIB_DIR/$base" ]]; then
    cp -L "$real" "$LIB_DIR/$base"
    echo "  + $base"
  fi
  echo "$LIB_DIR/$base"
}

echo "== core FFmpeg libs =="
declare -a QUEUE=()
for name in "${CORE_LIBS[@]}"; do
  src="$(find_lib "$name" || true)"
  if [[ -z "${src:-}" || ! -e "$src" ]]; then
    echo "missing $name — install ffmpeg dev libs (libavcodec-dev …) or set FFMPEG_LIB_DIR" >&2
    exit 1
  fi
  dest="$(copy_real "$src")"
  QUEUE+=("$dest")
done

# NOTE: libmpv is deliberately NOT crawled — its full dep tree (libavfilter →
# libarchive, libblas, …) balloons the bundle to 200MB+. The embedded backend
# only needs the 5 core libs + their codec deps; libmpv keeps its status quo
# (host ffmpeg libs, like any distro mpv package).

echo "== transitive deps (ldd crawl) =="
# Breadth-first: crawl NEEDED deps of everything we copy until fixpoint.
declare -A SEEN=()
for q in "${QUEUE[@]}"; do SEEN["$(basename "$q")"]=1; done
while ((${#QUEUE[@]})); do
  cur="${QUEUE[0]}"
  QUEUE=("${QUEUE[@]:1}")
  while IFS= read -r dep; do
    base="$(basename "$dep")"
    [[ "$base" =~ $EXCLUDE_RE ]] && continue
    [[ -n "${SEEN[$base]:-}" ]] && continue
    SEEN[$base]=1
    dest="$(copy_real "$dep")"
    QUEUE+=("$dest")
  done < <(ldd "$cur" 2>/dev/null | awk '/=> \// {print $3} /^\// {print $1}')
done

# Transitive deps are resolved via each lib's own RUNPATH — point bundled libs at $ORIGIN.
if command -v patchelf >/dev/null 2>&1; then
  for f in "$LIB_DIR"/libav*.so* "$LIB_DIR"/libsw*.so*; do
    [[ -e "$f" ]] || continue
    patchelf --set-rpath '$ORIGIN' "$f" 2>/dev/null || true
  done
  # Same for second-level deps (libvpx, libx264, …) so their own deps resolve in lib/.
  for f in "$LIB_DIR"/*.so*; do
    [[ -e "$f" ]] || continue
    patchelf --set-rpath '$ORIGIN' "$f" 2>/dev/null || true
  done
else
  echo "WARN: patchelf not found — transitive deps may not resolve from \$ORIGIN/lib" >&2
fi

echo "bundled FFmpeg libs → $LIB_DIR/ ($(du -sh "$LIB_DIR" | cut -f1) total)"
