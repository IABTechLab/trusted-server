#!/usr/bin/env bash
#
# Run browser-level integration tests using Playwright.
#
# Builds the WASM binary, Docker test images, and runs Playwright tests
# against both Next.js and WordPress frontends.
#
# Prerequisites:
#   - Docker running
#   - Viceroy installed: cargo install viceroy --version 0.17.0 --locked --force
#   - wasm32-wasip1 target: rustup target add wasm32-wasip1
#   - Node.js with npx available
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
BROWSER_DIR="crates/trusted-server-integration-tests/browser"
TSJS_LIB_DIR="crates/trusted-server-js/lib"
NODE_VERSION="$(grep '^nodejs ' .tool-versions | awk '{print $2}')"

if [ -z "$NODE_VERSION" ]; then
    echo "Failed to detect Node.js version from .tool-versions" >&2
    exit 1
fi

# --- Build WASM binary ---
echo "==> Building WASM binary (origin=http://127.0.0.1:$ORIGIN_PORT)..."
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT" \
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="integration-test-proxy-secret" \
TRUSTED_SERVER__EC__PASSPHRASE="integration-test-ec-secret-padded-32" \
TRUSTED_SERVER__EC__PARTNERS='[{"name":"Integration Test Partner","source_domain":"inttest.example.com","bidstream_enabled":true,"api_token":"integration-test-token-alpha-32-bytes-ok"},{"name":"Integration Test Partner 2","source_domain":"inttest2.example.com","bidstream_enabled":true,"api_token":"integration-test-token-bravo-32-bytes-ok"}]' \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1

echo "==> Generating Viceroy configs..."
INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" ./scripts/generate-integration-viceroy-configs.sh
GENERATED_VICEROY_CONFIG_PATH="$REPO_ROOT/target/integration-test-artifacts/configs/viceroy.toml"
GENERATED_AD_TRACE_CONFIG_PATH="$REPO_ROOT/target/integration-test-artifacts/configs/viceroy-ad-trace.toml"

# Build the actual external Prebid bundle consumed by the isolated ad-trace
# fixture. The browser routes its first-party managed URL to this local asset;
# no public ad network is contacted.
echo "==> Building deterministic external Prebid fixture bundle..."
rm -rf "$REPO_ROOT/target/integration-test-artifacts/prebid"
mkdir -p "$REPO_ROOT/target/integration-test-artifacts/prebid"
npm ci --prefix crates/trusted-server-js/lib
npm run --prefix crates/trusted-server-js/lib build:prebid-external -- \
    --adapters=rubicon \
    --user-id-modules=sharedIdSystem \
    --out "$REPO_ROOT/target/integration-test-artifacts/prebid"

# --- Build Docker images ---
echo "==> Building WordPress test container..."
docker build -t test-wordpress:latest \
    crates/trusted-server-integration-tests/fixtures/frameworks/wordpress/

echo "==> Building Next.js test container..."
docker build \
    --build-arg NODE_VERSION="$NODE_VERSION" \
    -t test-nextjs:latest \
    crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/

echo "==> Building ad-trace test container..."
docker build \
    -f crates/trusted-server-integration-tests/fixtures/frameworks/ad-trace/Dockerfile \
    -t test-ad-trace:latest \
    .

# --- Install Playwright ---
echo "==> Installing Playwright dependencies..."
cd "$REPO_ROOT/$BROWSER_DIR"
npm ci
npx playwright install chromium

# --- Build browser-side Trusted Server and external Prebid fixtures ---
echo "==> Building TSJS browser fixtures..."
cd "$REPO_ROOT/$TSJS_LIB_DIR"
npm ci
npm run build
npm run build:prebid-external
cd "$REPO_ROOT/$BROWSER_DIR"

# --- Export env vars for global-setup.ts ---
export WASM_BINARY_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"
export INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT"
export VICEROY_CONFIG_PATH="$GENERATED_VICEROY_CONFIG_PATH"

# Cleanup trap: stop any leftover containers on failure
stop_matching_containers() {
    local image="$1"
    local ids
    ids="$(docker ps -q --filter "ancestor=$image" 2>/dev/null || true)"
    if [ -n "$ids" ]; then
        printf '%s\n' "$ids" | xargs docker stop 2>/dev/null || true
    fi
}

cleanup() {
    stop_matching_containers test-ad-trace:latest
    stop_matching_containers test-nextjs:latest
    stop_matching_containers test-wordpress:latest
}
trap cleanup EXIT

# --- Run tests for each framework ---
for framework in nextjs wordpress ad-trace; do
    echo "==> Running Playwright tests for $framework..."
    if [ "$framework" = "ad-trace" ]; then
        TEST_FRAMEWORK="$framework" VICEROY_CONFIG_PATH="$GENERATED_AD_TRACE_CONFIG_PATH" \
            npx playwright test "$@"
    else
        TEST_FRAMEWORK="$framework" VICEROY_CONFIG_PATH="$GENERATED_VICEROY_CONFIG_PATH" \
            npx playwright test "$@"
    fi
done

echo "==> All browser tests passed."
