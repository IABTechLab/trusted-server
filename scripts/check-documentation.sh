#!/usr/bin/env bash

# Run the repository's existing documentation gates without rewriting sources.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
HOST_TARGET=$(rustc -vV | sed -n 's/host: //p')

if [ -z "$HOST_TARGET" ]; then
    echo "documentation check: could not resolve the Rust host target" >&2
    exit 1
fi

(
    cd "$REPO_ROOT/docs"
    npm ci
    npm run lint
    npm run format
    npm run build
)

(
    cd "$REPO_ROOT/crates/trusted-server-js/lib"
    npm ci
    npm run lint
)

cargo test \
    --manifest-path "$REPO_ROOT/crates/trusted-server-integration-tests/Cargo.toml" \
    --test documentation_snippets
cargo test --manifest-path "$REPO_ROOT/Cargo.toml" --doc -p trusted-server-core

RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    --all-features \
    -p trusted-server-core \
    -p trusted-server-js \
    -p trusted-server-openrtb \
    --target wasm32-wasip1
RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    -p trusted-server-adapter-fastly \
    --target wasm32-wasip1
RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    -p trusted-server-adapter-cloudflare \
    --target wasm32-unknown-unknown \
    --features cloudflare
RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    -p trusted-server-adapter-spin \
    --target wasm32-wasip1 \
    --features spin
RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    --all-features \
    -p trusted-server-adapter-axum
RUSTDOCFLAGS="-D warnings" cargo doc \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --no-deps \
    --all-features \
    -p trusted-server-cli \
    -p trusted-server-openrtb-codegen \
    --target "$HOST_TARGET"

echo "Documentation checks passed"
