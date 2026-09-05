#!/usr/bin/env bash
# Bundle libmpv (+ soname) next to the FluxPlay binary for a portable runtree.
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
  if [[ -n "${MPV_LIB_DIR:-}" && -e "${MPV_LIB_DIR}/libmpv.so" ]]; then
    echo "${MPV_LIB_DIR}/libmpv.so"
    return
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
    if [[ -e "$d/libmpv.so" ]]; then
      echo "$d/libmpv.so"
      return
    fi
  done
  if command -v ldconfig >/dev/null 2>&1; then
    ldconfig -p 2>/dev/null | awk '/libmpv\.so/ {print $NF; exit}'
  fi
}

SRC="$(resolve_libmpv || true)"
if [[ -z "${SRC:-}" || ! -e "$SRC" ]]; then
  echo "libmpv.so not found — install libmpv or set MPV_LIB_DIR" >&2
  exit 1
fi

# Copy the real .so and preserve soname symlinks.
REAL="$(readlink -f "$SRC")"
SONAME="$(basename "$REAL")"
cp -L "$REAL" "$LIB_DIR/$SONAME"
ln -sfn "$SONAME" "$LIB_DIR/libmpv.so"
# Common major symlink
if [[ "$SONAME" =~ libmpv\.so\.([0-9]+) ]]; then
  ln -sfn "$SONAME" "$LIB_DIR/libmpv.so.${BASH_REMATCH[1]}"
fi

echo "bundled $REAL → $LIB_DIR/"
echo "run with: LD_LIBRARY_PATH=$LIB_DIR $BIN"
echo "(release builds with bundle-rpath already search \$ORIGIN/lib)"
