#!/usr/bin/env bash
# Supply-chain gate: lockfile integrity + cargo-deny (+ optional cargo-audit).
# Usage:
#   ./scripts/supply-chain.sh           # deny (advisories/bans/licenses/sources)
#   FLUXPLAY_AUDIT=1 ./scripts/supply-chain.sh  # also cargo-audit
#   FLUXPLAY_DENY_OFFLINE=1 ./scripts/supply-chain.sh  # use cached advisory DB
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# shellcheck source=resource-budget.sh
# shellcheck disable=SC1091
source "$ROOT/scripts/resource-budget.sh"
fluxplay_export_cargo_jobs

echo "==> cargo metadata --locked (Cargo.lock must match manifests)"
cargo metadata --locked --format-version 1 >/dev/null

echo "==> cargo fetch --locked (no resolve drift)"
cargo fetch --locked

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "installing cargo-deny (locked)…"
  cargo install cargo-deny --locked
fi

# Global flags before `check` (cargo-deny ≥0.18 CLI).
DENY_GLOBAL=(--config deny.toml --workspace --all-features --locked)
if [[ "${FLUXPLAY_DENY_OFFLINE:-0}" == "1" ]]; then
  DENY_GLOBAL+=(--offline)
fi

echo "==> cargo deny ${DENY_GLOBAL[*]} check"
cargo deny "${DENY_GLOBAL[@]}" check

if [[ "${FLUXPLAY_AUDIT:-0}" == "1" ]]; then
  if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "installing cargo-audit (locked)…"
    cargo install cargo-audit --locked
  fi
  echo "==> cargo audit"
  cargo audit
fi

echo "==> supply-chain OK"
