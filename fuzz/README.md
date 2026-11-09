# FluxPlay fuzz harnesses

libFuzzer targets for parsers and browse-tile invariants. The fuzz crate is a
**separate Cargo workspace** (not a root member) so `cargo test` stays light.

## Targets

| Binary | Surface |
|--------|---------|
| `fuzz_m3u` | `fluxplay_core::m3u::parse_m3u` |
| `fuzz_stream_url` | `StreamScheme` / `StreamUrl` |
| `fuzz_xmltv` | `parse_xmltv` |
| `fuzz_titles` | IPTV title / category / channel cleaners |
| `fuzz_browse_tiles` | `BrowseTileCache` ops + viewport math |
| `fuzz_wg_profile` | WireGuard `parse_profile` |
| `fuzz_sanitize` | download `sanitize_file_name` (asserts no `/\\%`) |

App-layer targets pull `fluxplay` (iced + WireGuard). `cargo-fuzz` replaces
`.cargo/config.toml` `[build].rustflags`, so always export:

```bash
export RUSTFLAGS='--cfg curve25519_dalek_backend="serial"'
```

(`scripts/fuzz-long.sh` and `FLUXPLAY_FUZZ=1 ./scripts/tdd-gate.sh` do this.)

## Run

```bash
cargo install cargo-fuzz
export RUSTFLAGS='--cfg curve25519_dalek_backend="serial"'
cd fuzz
cargo fuzz run fuzz_m3u
# long stress (all targets + high-case proptest):
../scripts/fuzz-long.sh 120
# short CI smoke:
FLUXPLAY_FUZZ=1 ../scripts/tdd-gate.sh
```

Requires nightly + clang (libFuzzer). Property tests in `crates/*/src` cover the
same contracts on stable via `proptest` (no clang needed).
