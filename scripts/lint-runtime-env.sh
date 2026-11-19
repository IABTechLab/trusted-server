#!/usr/bin/env bash
# Enforce serving process-environment policy using compiler name resolution.
# Like lint-wasm-clock.sh, use a dedicated config instead of a source parser.
# Select shared packages explicitly: Clippy does not lint dependency internals.
# Production targets let Rust exclude #[cfg(test)] helpers and poison fixtures.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CLIPPY_CONF_DIR="$root/scripts/clippy-runtime-env"
shared=(-p trusted-server-core -p trusted-server-js -p trusted-server-openrtb
    -p trusted-server-identity-providers)
lints=(-A clippy::all -D clippy::disallowed_methods -D clippy::disallowed_types)

cargo clippy --locked "${shared[@]}" -p trusted-server-adapter-axum \
    --lib --bins --all-features -- "${lints[@]}"
cargo clippy --locked "${shared[@]}" -p trusted-server-adapter-fastly \
    --target wasm32-wasip1 --lib --bins --all-features -- "${lints[@]}"
cargo clippy --locked "${shared[@]}" -p trusted-server-adapter-cloudflare \
    --target wasm32-unknown-unknown --lib --bins --features cloudflare -- "${lints[@]}"
cargo clippy --locked "${shared[@]}" -p trusted-server-adapter-spin \
    --target wasm32-wasip1 --lib --bins --features spin -- "${lints[@]}"
