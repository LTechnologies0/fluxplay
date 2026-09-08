#!/usr/bin/env bash
# Compile FluxPlayNativeActivity.java → classes.dex and inject into a cargo-apk APK.
# Also patches android:supportsPictureInPicture="true" on the activity if missing.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JAVA_SRC="$ROOT/crates/fluxplay-android/java"
OUT="$ROOT/target/android-java"
APK="${1:?usage: inject-android-java.sh path/to/FluxPlay.apk}"
# Must be absolute: we cd into a temp dir before apksigner --out.
APK="$(cd "$(dirname "$APK")" && pwd)/$(basename "$APK")"
test -f "$APK" || { echo "apk not found: $APK" >&2; exit 1; }

ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
BUILD_TOOLS="$(ls -1d "$ANDROID_HOME/build-tools"/*/ 2>/dev/null | sort -V | tail -1)"
PLATFORM="$(ls -1d "$ANDROID_HOME/platforms"/android-3[4-9]* 2>/dev/null | sort -V | tail -1)"
JAVAC="$(command -v javac || true)"
[[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/javac" ]] && JAVAC="$JAVA_HOME/bin/javac"

if [[ -z "$BUILD_TOOLS" || -z "$PLATFORM" || -z "$JAVAC" ]]; then
  echo "need ANDROID_HOME build-tools + platforms + javac" >&2
  exit 1
fi

D8="$BUILD_TOOLS/d8"
ZIPALIGN="$BUILD_TOOLS/zipalign"
APKSIGNER="$BUILD_TOOLS/apksigner"
AAPT2="$(command -v aapt2 || true)"
[[ -x "$BUILD_TOOLS/aapt2" ]] && AAPT2="$BUILD_TOOLS/aapt2"
AAPT="$(command -v aapt || true)"
[[ -x "$BUILD_TOOLS/aapt" ]] && AAPT="$BUILD_TOOLS/aapt"
ANDROID_JAR="$PLATFORM/android.jar"

rm -rf "$OUT"
mkdir -p "$OUT/classes"
"$JAVAC" --release 11 -cp "$ANDROID_JAR" -d "$OUT/classes" \
  "$JAVA_SRC/app/fluxplay/android/FluxPlayNativeActivity.java"
find "$OUT/classes" -name '*.class' > "$OUT/classes.list"
"$D8" --release --lib "$ANDROID_JAR" --output "$OUT" @"$OUT/classes.list"
test -f "$OUT/classes.dex"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
unzip -q "$APK" -d unpacked
cp "$OUT/classes.dex" unpacked/classes.dex

# cargo-apk schema lacks supportsPictureInPicture — patch when missing.
MANIFEST="unpacked/AndroidManifest.xml"
patch_pip_manifest() {
  local m="$1"
  # Insert on the FluxPlay activity (or first <activity) if attribute absent.
  python3 - "$m" <<'PY'
import re, sys
path = sys.argv[1]
with open(path, "rb") as f:
    data = f.read()
if b"supportsPictureInPicture" in data:
    sys.exit(0)
# Text XML (rare for cargo-apk): edit in place.
try:
    text = data.decode("utf-8")
except UnicodeDecodeError:
    sys.exit(2)
attr = ' android:supportsPictureInPicture="true"'
pat = re.compile(
    r'(<activity\b[^>]*\bandroid:name="(?:app\.fluxplay\.android\.FluxPlayNativeActivity|android\.app\.NativeActivity)"[^>/]*)(/?>)',
    re.I,
)
m = pat.search(text)
if not m:
    pat = re.compile(r'(<activity\b[^>/]*)(/?>)', re.I)
    m = pat.search(text)
if not m:
    sys.exit(3)
if "supportsPictureInPicture" in m.group(1):
    sys.exit(0)
text = text[: m.start(1)] + m.group(1) + attr + m.group(2) + text[m.end(2) :]
with open(path, "wb") as f:
    f.write(text.encode("utf-8"))
print("patched text AndroidManifest.xml (supportsPictureInPicture)")
sys.exit(0)
PY
}

patch_pip_androguard() {
  local m="$1"
  python3 - "$m" <<'PY'
import sys
path = sys.argv[1]
try:
    from androguard.core.axml import AXMLPrinter
except ImportError:
    sys.exit(2)
with open(path, "rb") as f:
    raw = f.read()
if b"supportsPictureInPicture" in raw:
    sys.exit(0)
try:
    xml_bytes = AXMLPrinter(raw).get_xml()
    if isinstance(xml_bytes, bytes):
        text = xml_bytes.decode("utf-8", errors="replace")
    else:
        text = str(xml_bytes)
except Exception as e:
    print("androguard decode failed:", e, file=sys.stderr)
    sys.exit(3)
import re
attr = ' android:supportsPictureInPicture="true"'
if "supportsPictureInPicture" in text:
    sys.exit(0)
pat = re.compile(
    r'(<activity\b[^>]*\bandroid:name="(?:app\.fluxplay\.android\.FluxPlayNativeActivity|android\.app\.NativeActivity)"[^>/]*)(/?>)',
    re.I,
)
m = pat.search(text)
if not m:
    pat = re.compile(r'(<activity\b[^>/]*)(/?>)', re.I)
    m = pat.search(text)
if not m:
    sys.exit(4)
text = text[: m.start(1)] + m.group(1) + attr + m.group(2) + text[m.end(2) :]
# Re-encode requires aapt/apktool; write decoded XML beside for tooling, keep binary.
out_xml = path + ".decoded.xml"
with open(out_xml, "w", encoding="utf-8") as f:
    f.write(text)
print("androguard decoded activity; need aapt/apktool to re-pack binary AXML", file=sys.stderr)
sys.exit(5)
PY
}

if [[ -f "$MANIFEST" ]] && ! grep -a -q 'supportsPictureInPicture' "$MANIFEST" 2>/dev/null; then
  PIP_OK=0
  if command -v apktool >/dev/null 2>&1; then
    echo "patching supportsPictureInPicture via apktool..."
    if apktool d -f -o decoded "$APK" >/dev/null 2>&1; then
      DEC_MAN="decoded/AndroidManifest.xml"
      if [[ -f "$DEC_MAN" ]]; then
        if patch_pip_manifest "$DEC_MAN"; then
          if apktool b -o rebuilt.apk decoded >/dev/null 2>&1; then
            # Prefer rebuilt binary manifest; keep our injected classes.dex.
            unzip -qo rebuilt.apk AndroidManifest.xml -d unpacked
            if grep -a -q 'supportsPictureInPicture' "$MANIFEST" 2>/dev/null; then
              PIP_OK=1
              echo "apktool: supportsPictureInPicture set"
            else
              # Full rebuilt tree then re-inject dex
              rm -rf unpacked
              unzip -q rebuilt.apk -d unpacked
              cp "$OUT/classes.dex" unpacked/classes.dex
              PIP_OK=1
              echo "apktool: rebuilt APK + re-injected classes.dex"
            fi
          fi
        fi
      fi
    fi
    if [[ "$PIP_OK" -ne 1 ]]; then
      echo "WARNING: apktool PiP patch failed; continuing with dex only" >&2
    fi
  else
    # No apktool: try text/binary heuristics, aapt2 dump, androguard.
    if patch_pip_manifest "$MANIFEST" 2>/dev/null; then
      PIP_OK=1
    else
      if [[ -n "$AAPT2" ]]; then
        "$AAPT2" dump xmltree unpacked AndroidManifest.xml >/dev/null 2>&1 || true
      fi
      if patch_pip_androguard "$MANIFEST" 2>/dev/null; then
        PIP_OK=1
      elif [[ -n "$AAPT" ]] && [[ -f "${MANIFEST}.decoded.xml" ]]; then
        # Best-effort: aapt package stub → binary (often needs full resource tree; may fail).
        mkdir -p aapt_stub/res
        if "$AAPT" package -f -M "${MANIFEST}.decoded.xml" -I "$ANDROID_JAR" -F aapt_stub.apk >/dev/null 2>&1; then
          unzip -qo aapt_stub.apk AndroidManifest.xml -d unpacked && PIP_OK=1 || true
        fi
      fi
    fi
    if [[ "$PIP_OK" -ne 1 ]]; then
      echo "WARNING: could not patch supportsPictureInPicture (apktool/androguard/aapt missing or binary AXML). Dex injected; PiP may still work via enterPictureInPictureMode at runtime." >&2
    fi
  fi
fi

(
  cd unpacked
  # Android R+ requires resources.arsc stored uncompressed + 4-byte aligned.
  rm -f ../unsigned.apk
  if [[ -f resources.arsc ]]; then
    zip -q -r -X ../unsigned.apk . -x resources.arsc
    zip -q -0 -X ../unsigned.apk resources.arsc
  else
    zip -q -r -X ../unsigned.apk .
  fi
)
"$ZIPALIGN" -f -p 4 unsigned.apk aligned.apk

KS="${FLUXPLAY_ANDROID_KS:-$HOME/.android/debug.keystore}"
KS_PASS="${FLUXPLAY_ANDROID_KS_PASS:-android}"
KEY_ALIAS="${FLUXPLAY_ANDROID_KEY_ALIAS:-androiddebugkey}"
KEY_PASS="${FLUXPLAY_ANDROID_KEY_PASS:-android}"

if "$APKSIGNER" sign --help 2>&1 | grep -q -- '--ks-key-alias'; then
  ALIAS_FLAG=--ks-key-alias
else
  ALIAS_FLAG=--key-alias
fi
"$APKSIGNER" sign \
  --ks "$KS" \
  --ks-pass "pass:$KS_PASS" \
  $ALIAS_FLAG "$KEY_ALIAS" \
  --key-pass "pass:$KEY_PASS" \
  --out "$APK" aligned.apk

if ! unzip -l "$APK" | grep -E 'classes\.dex$' >/dev/null; then
  echo "ERROR: classes.dex missing from signed APK: $APK" >&2
  exit 1
fi

DEX_SIZE=$(wc -c < "$OUT/classes.dex")
echo "injected classes.dex → $APK (${DEX_SIZE} bytes dex)"
unzip -l "$APK" | grep -E 'classes\.dex$'
