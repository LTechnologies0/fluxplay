# FluxPlay fuzz harnesses

libFuzzer targets for parsers, protocol routing, iced UI math, soft
FFmpeg/libmpv present budgets, persistence, and skip heuristics. The fuzz crate
is a **separate Cargo workspace** (not a root member) so `cargo test` stays light.

**Default path:** `./scripts/tdd-gate.sh` and `./scripts/build-native.sh` run the
full property suite **and** a short libFuzzer pass automatically (resource-
capped). Opt out with `FLUXPLAY_FUZZ=0` or `FLUXPLAY_SKIP_VERIFY=1` (build only).

## Resource budget

| Knob | Default | Purpose |
|------|---------|---------|
| `CARGO_BUILD_JOBS` / `FLUXPLAY_JOBS` | `min(4, nproc/2)` | leave cores for the desktop |
| `FLUXPLAY_FUZZ_JOBS` | `min(3, nproc/2)` | parallel fuzz targets |
| `FLUXPLAY_FUZZ_RSS_MB` | ~½–1½ GiB / process | libFuzzer RSS cap |
| `FLUXPLAY_FUZZ_SECS` | `8` (gate) / arg (long) | wall time per target |

`.cargo/config.toml` sets `build.jobs = 4` so a bare `cargo build` cannot eat
every core either.

## Targets

| Binary | Surface | Asserts beyond “no panic” |
|--------|---------|---------------------------|
| `fuzz_m3u` | `parse_m3u` | amplification + NUL-free fields |
| `fuzz_stream_url` | `StreamScheme` / `StreamUrl` | Debug size + host bounds |
| `fuzz_xmltv` | `parse_xmltv` | fan-out, field sizes, start≤stop |
| `fuzz_titles` | IPTV title / category / channel | UTF-8 / size / channel==display |
| `fuzz_browse_tiles` | `BrowseTileCache` | viewport math + cache cap |
| `fuzz_wg_profile` | WireGuard `parse_profile` | MTU range on Ok |
| `fuzz_sanitize` | `sanitize_file_name` | `/\\%:*?"<>\|`, `..`, reserved `CON`/`NUL` |
| `fuzz_virtual_slice` | mosaic/list virtualization | mount window, height monotonicity |
| `fuzz_soft_present` | Auto 720p envelope | 1080p rejected when UHD off |
| `fuzz_downloads` | url_key / episode / progress | opaque hex key, no path seps |
| `fuzz_image_url` | `is_fetchable_image_url` | file/data/js/svg rejected |
| `fuzz_xtream_url` | portal get.php helpers | path_segment safety |
| `fuzz_metadata` | title query / cleaners / fmt_* | bounded UI text, no NUL |
| `fuzz_redact` | `redact_urls` | no leftover `http(s)://` (any case) |
| `fuzz_ui_surface` | `UiSurface::detect` | finite touch/overscan/scale |
| `fuzz_route` | player `route()` | every scheme/URL blob |
| `fuzz_hls_body` | `parse_hls_manifest_body` | master/variant consistency |
| `fuzz_chapters` | chapter classify + spans | OP/ED invariants |
| `fuzz_persist` | `state.json` + TOSD JSON | corrupt → no panic |
| `fuzz_video_frame` | PixelLayout / YUV coeffs | rows ≥ height, finite coeffs |
| `fuzz_android_quality` | SoftBudget / present ladder | SoftRgba always available |

Curated **seeds** live in `fuzz/seeds/<target>/` (copied into gitignored
`fuzz/corpus/` by the scripts). **Dictionaries** in `fuzz/dictionaries/` bias
mutation toward real IPTV / WG / URL / JSON tokens (ASCII-only — libFuzzer’s
dict parser rejects non-ASCII).

## Run

```bash
# default verify (props + fuzz) — also invoked by build-native.sh
./scripts/verify.sh
# or:
./scripts/tdd-gate.sh

# skip fuzz only:
FLUXPLAY_FUZZ=0 ./scripts/tdd-gate.sh

# long stress (all targets + high-case proptest):
./scripts/fuzz-long.sh 180

# single target:
cd fuzz && cargo +nightly fuzz run fuzz_route -- \
  -dict=dictionaries/url.dict -use_value_profile=1
```

Requires nightly + clang (libFuzzer). Property tests in `crates/*/src` cover the
same contracts on stable via `proptest` (no clang needed).

## Finding crashes

Artifacts land in `/tmp/fluxplay-fuzz-artifacts/` (prefix per target). Repro:

```bash
cd fuzz
cargo +nightly fuzz run fuzz_m3u /tmp/fluxplay-fuzz-artifacts/fuzz_m3u_crash-*
```
