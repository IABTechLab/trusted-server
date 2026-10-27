#!/usr/bin/env bash
#
# Generate Viceroy configs for integration tests from the readable Trusted Server
# integration app config fixture.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
ARTIFACTS_DIR="${ARTIFACTS_DIR:-$REPO_ROOT/target/integration-test-artifacts}"
CONFIG_DIR="$ARTIFACTS_DIR/configs"
TEMPLATE_PATH="crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml"
APP_CONFIG_PATH="crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
INTEGRATION_TARGET_DIR="crates/trusted-server-integration-tests/target"
ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT"
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"

if [ -z "$HOST_TARGET" ]; then
    echo "Failed to detect host target from rustc -vV" >&2
    exit 1
fi

mkdir -p "$CONFIG_DIR"

# Cargo checks freshness for the default generator; explicit overrides are caller-owned.
if [ -z "${GENERATOR_BIN:-}" ]; then
    cargo build \
        --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
        --target-dir "$INTEGRATION_TARGET_DIR" \
        --target "$HOST_TARGET" \
        --bin generate-viceroy-config
fi
GENERATOR_BIN="${GENERATOR_BIN:-$INTEGRATION_TARGET_DIR/$HOST_TARGET/debug/generate-viceroy-config}"
if [ ! -x "$GENERATOR_BIN" ]; then
    echo "Generator binary not found or not executable at $GENERATOR_BIN" >&2
    exit 1
fi

if [ "${1:-}" = "--local-manifests" ]; then
    "$GENERATOR_BIN" --adapter fastly --template fastly.toml \
        --app-config trusted-server.example.toml --output fastly.toml
    "$GENERATOR_BIN" --adapter spin \
        --template crates/trusted-server-adapter-spin/spin.toml \
        --app-config trusted-server.example.toml \
        --output crates/trusted-server-adapter-spin/spin.toml
    "$GENERATOR_BIN" --adapter cloudflare \
        --template crates/trusted-server-adapter-cloudflare/wrangler.ci.toml \
        --app-config "$APP_CONFIG_PATH" \
        --output crates/trusted-server-adapter-cloudflare/wrangler.ci.toml
    exit 0
fi

"$GENERATOR_BIN" \
    --template "$TEMPLATE_PATH" \
    --app-config "$APP_CONFIG_PATH" \
    --output "$CONFIG_DIR/viceroy.toml" \
    --origin-url "$ORIGIN_URL"
