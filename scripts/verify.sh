#!/usr/bin/env bash
# Default verification entrypoint — always run before shipping a build.
# Prefer this (or `scripts/build-native.sh`) over bare `cargo build`.
# Runs supply-chain (deny) + -D warnings + clippy + tests + fuzz (see tdd-gate).
exec "$(cd "$(dirname "$0")" && pwd)/tdd-gate.sh" "$@"
