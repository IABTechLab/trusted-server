#!/usr/bin/env bash
#
# Run browser-level integration tests using Playwright.
#
# Builds the WASM binary, Docker test images, and runs Playwright tests
# against both Next.js and WordPress frontends.
#
# Prerequisites:
#   - Docker running
#   - Viceroy installed: cargo install viceroy --version 0.19.0 --locked --force
#   - wasm32-wasip1 target: rustup target add wasm32-wasip1
#   - Node.js with npm available
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
BROWSER_DIR="crates/trusted-server-integration-tests/browser"
TSJS_LIB_DIR="crates/trusted-server-js/lib"
NODE_VERSION="$(grep '^nodejs ' .tool-versions | awk '{print $2}')"
FRAMEWORKS_VALUE="${TS_BROWSER_FRAMEWORKS:-nextjs wordpress}"
FRAMEWORKS_VALUE="${FRAMEWORKS_VALUE//,/ }"
read -r -a FRAMEWORKS <<< "$FRAMEWORKS_VALUE"
PROJECTS_VALUE="${TS_BROWSER_PROJECTS:-chromium}"
PROJECTS_VALUE="${PROJECTS_VALUE//,/ }"
read -r -a BROWSER_PROJECTS <<< "$PROJECTS_VALUE"

if [ -z "$NODE_VERSION" ]; then
    echo "Failed to detect Node.js version from .tool-versions" >&2
    exit 1
fi

if [ "${#FRAMEWORKS[@]}" -eq 0 ]; then
    echo "TS_BROWSER_FRAMEWORKS must select at least one framework" >&2
    exit 1
fi

if [ "${#BROWSER_PROJECTS[@]}" -eq 0 ]; then
    echo "TS_BROWSER_PROJECTS must select at least one browser" >&2
    exit 1
fi

for framework in "${FRAMEWORKS[@]}"; do
    case "$framework" in
        nextjs|wordpress) ;;
        *)
            echo "Unsupported browser framework: $framework" >&2
            exit 1
            ;;
    esac
done

for project in "${BROWSER_PROJECTS[@]}"; do
    case "$project" in
        chromium|firefox|webkit) ;;
        *)
            echo "Unsupported browser project: $project" >&2
            exit 1
            ;;
    esac
done

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

# --- Build Docker images ---
for framework in "${FRAMEWORKS[@]}"; do
    if [ "$framework" = "wordpress" ]; then
        echo "==> Building WordPress test container..."
        docker build -t test-wordpress:latest \
            crates/trusted-server-integration-tests/fixtures/frameworks/wordpress/
    else
        echo "==> Building Next.js test container..."
        docker build \
            --build-arg NODE_VERSION="$NODE_VERSION" \
            -t test-nextjs:latest \
            crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/
    fi
done

# --- Install Playwright ---
echo "==> Installing Playwright dependencies..."
npm --prefix "$BROWSER_DIR" ci
PLAYWRIGHT_INSTALL_ARGS=(install)
if [ "${CI:-}" = "true" ]; then
    PLAYWRIGHT_INSTALL_ARGS+=(--with-deps)
fi
npm --prefix "$BROWSER_DIR" exec -- playwright \
    "${PLAYWRIGHT_INSTALL_ARGS[@]}" "${BROWSER_PROJECTS[@]}"

# --- Build browser-side Trusted Server and external Prebid fixtures ---
echo "==> Building TSJS browser fixtures..."
npm --prefix "$TSJS_LIB_DIR" ci
npm --prefix "$TSJS_LIB_DIR" run build
npm --prefix "$TSJS_LIB_DIR" run build:prebid-external

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
    for framework in "${FRAMEWORKS[@]}"; do
        stop_matching_containers "test-$framework:latest"
    done
}
trap cleanup EXIT

# --- Run tests for each framework ---
for framework in "${FRAMEWORKS[@]}"; do
    echo "==> Running Playwright tests for $framework..."
    TEST_FRAMEWORK="$framework" npm --prefix "$BROWSER_DIR" exec -- \
        playwright test --config "$REPO_ROOT/$BROWSER_DIR/playwright.config.ts" "$@"
done

echo "==> All browser tests passed."
