#!/usr/bin/env bash
#
# Run integration tests locally.
#
# Builds the WASM binary with test-specific config overrides,
# Docker test images, and runs all integration tests.
#
# Prerequisites:
#   - Docker running
#   - Viceroy installed: cargo install viceroy
#   - wasm32-wasip1 target: rustup target add wasm32-wasip1
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Fixed origin port — must match the port baked into the WASM binary.
# Docker containers are mapped to this port so the trusted-server
# can proxy requests to them.
ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"

# Detect native target
case "$(uname -m)" in
    arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
    x86_64)        TARGET="x86_64-unknown-linux-gnu" ;;
    *)             echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

echo "==> Building WASM binary (origin=http://127.0.0.1:$ORIGIN_PORT)..."
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT" \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
    cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

echo "==> Building WordPress test container..."
docker build -t test-wordpress:latest \
    crates/integration-tests/fixtures/frameworks/wordpress/

echo "==> Building Next.js test container..."
docker build -t test-nextjs:latest \
    crates/integration-tests/fixtures/frameworks/nextjs/

echo "==> Running integration tests (target: $TARGET, origin port: $ORIGIN_PORT)..."
WASM_BINARY_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-fastly.wasm" \
INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" \
RUST_LOG=info \
    cargo test -p integration-tests --target "$TARGET" -- --include-ignored --test-threads=1 "$@"
