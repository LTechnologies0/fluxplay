# Build & supply-chain security

FluxPlay treats `Cargo.lock` as the source of truth for every shippable build.

## Entrypoints

| Command | What it does |
|---------|----------------|
| `./scripts/verify.sh` | Full gate: supply-chain → `-D warnings` → clippy → tests → fuzz |
| `./scripts/supply-chain.sh` | `cargo metadata/fetch --locked` + **cargo-deny** (advisories/bans/licenses/sources) |
| `./scripts/build-native.sh` | Host-max rustc flags + `cargo build --release --locked` |
| `cargo deny …` | Policy in root `deny.toml` |

## Pins & policies

- **`rust-toolchain.toml`** — `stable` for the workspace (fuzz uses `rustup run nightly`).
- **`.cargo/config.toml`** — per-OS/arch rustflags + `git-fetch-with-cli`.
- **`deny.toml`** — crates.io only; OpenSSL banned; documented RUSTSEC ignores only.
- **CI** — `permissions: contents: read`, `--locked` everywhere, dedicated `supply-chain` job.
- **Dependabot** — weekly Cargo + Actions updates (`.github/dependabot.yml`).

## Updating dependencies

```bash
cargo update                 # within semver of Cargo.toml
./scripts/supply-chain.sh    # must stay green
./scripts/verify.sh          # before shipping
```

Never commit a lockfile that fails `cargo deny`. Prefer fixing/upgrading over `ignore = […]`.
