#!/usr/bin/env bash
# Bundle libmpv (+ soname / dylib) next to the FluxPlay binary for a portable runtree.
# Usage: scripts/bundle-libmpv.sh [path-to-fluxplay-binary]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-"$ROOT/target/release/fluxplay"}"
if [[ ! -f "$BIN" ]]; then
  echo "binary not found: $BIN" >&2
  echo "build first: cargo build -p fluxplay --release" >&2
  exit 1
fi

OUT_DIR="$(cd "$(dirname "$BIN")" && pwd)"
LIB_DIR="$OUT_DIR/lib"
mkdir -p "$LIB_DIR"

resolve_libmpv() {
  if [[ -n "${MPV_LIB_DIR:-}" ]]; then
    for n in libmpv.so libmpv.dylib; do
      if [[ -e "${MPV_LIB_DIR}/$n" ]]; then
        echo "${MPV_LIB_DIR}/$n"
        return
      fi
    done
  fi
  for d in \
    /home/linuxbrew/.linuxbrew/lib \
    /opt/homebrew/lib \
    /usr/local/lib \
    /usr/lib64 \
    /usr/lib/x86_64-linux-gnu \
    /usr/lib/aarch64-linux-gnu \
    /usr/lib
  do
    for n in libmpv.so libmpv.dylib; do
      if [[ -e "$d/$n" ]]; then
        echo "$d/$n"
        return
      fi
    done
  done
  if command -v ldconfig >/dev/null 2>&1; then
    ldconfig -p 2>/dev/null | awk '/libmpv\.so/ {print $NF; exit}'
  fi
}

SRC="$(resolve_libmpv || true)"
if [[ -z "${SRC:-}" || ! -e "$SRC" ]]; then
  echo "libmpv not found — install libmpv/mpv or set MPV_LIB_DIR" >&2
  exit 1
fi

# Resolve symlink without GNU readlink -f (macOS).
REAL="$SRC"
if command -v readlink >/dev/null 2>&1; then
  if readlink -f "$SRC" >/dev/null 2>&1; then
    REAL="$(readlink -f "$SRC")"
  elif [[ -L "$SRC" ]]; then
    REAL="$(cd "$(dirname "$SRC")" && pwd)/$(readlink "$SRC")"
  fi
fi
BASE="$(basename "$REAL")"
cp -L "$REAL" "$LIB_DIR/$BASE"

if [[ "$BASE" == *.dylib ]]; then
  # libmpv.2.dylib → also expose libmpv.dylib
  ln -sfn "$BASE" "$LIB_DIR/libmpv.dylib"
elif [[ "$BASE" == libmpv.so* ]]; then
  ln -sfn "$BASE" "$LIB_DIR/libmpv.so"
  if [[ "$BASE" =~ libmpv\.so\.([0-9]+) ]]; then
    ln -sfn "$BASE" "$LIB_DIR/libmpv.so.${BASH_REMATCH[1]}"
  fi
fi

if [[ -z "$(ls -A "$LIB_DIR" 2>/dev/null)" ]]; then
  echo "bundle empty after copy" >&2
  exit 1
fi

echo "bundled $REAL → $LIB_DIR/"
echo "run with: LD_LIBRARY_PATH=$LIB_DIR (or DYLD_LIBRARY_PATH) $BIN"
echo "(release builds with bundle-rpath already search \$ORIGIN/lib or @loader_path/lib)"
