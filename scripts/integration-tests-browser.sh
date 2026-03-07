#!/usr/bin/env bash
#
# Run browser-level integration tests using Playwright.
#
# Builds the WASM binary, Docker test images, and runs Playwright tests
# against both Next.js and WordPress frontends.
#
# Prerequisites:
#   - Docker running
#   - Viceroy installed: cargo install viceroy
#   - wasm32-wasip1 target: rustup target add wasm32-wasip1
#   - Node.js with npx available
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
BROWSER_DIR="crates/integration-tests/browser"

# --- Build WASM binary ---
echo "==> Building WASM binary (origin=http://127.0.0.1:$ORIGIN_PORT)..."
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT" \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
    cargo build --bin trusted-server-fastly --release --target wasm32-wasip1

# --- Build Docker images ---
echo "==> Building WordPress test container..."
docker build -t test-wordpress:latest \
    crates/integration-tests/fixtures/frameworks/wordpress/

echo "==> Building Next.js test container..."
docker build -t test-nextjs:latest \
    crates/integration-tests/fixtures/frameworks/nextjs/

# --- Install Playwright ---
echo "==> Installing Playwright dependencies..."
cd "$REPO_ROOT/$BROWSER_DIR"
npm ci
npx playwright install chromium

# --- Export env vars for global-setup.ts ---
export WASM_BINARY_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-fastly.wasm"
export INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT"
export VICEROY_CONFIG_PATH="$REPO_ROOT/crates/integration-tests/fixtures/configs/viceroy-template.toml"

# Cleanup trap: stop any leftover containers on failure
cleanup() {
    docker ps -q \
        --filter ancestor=test-nextjs:latest \
        --filter ancestor=test-wordpress:latest \
        2>/dev/null | xargs -r docker stop 2>/dev/null || true
}
trap cleanup EXIT

# --- Run tests for each framework ---
for framework in nextjs wordpress; do
    echo "==> Running Playwright tests for $framework..."
    TEST_FRAMEWORK="$framework" npx playwright test "$@"
done

echo "==> All browser tests passed."
