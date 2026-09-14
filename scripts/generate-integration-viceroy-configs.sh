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
ENABLE_AUCTION="${INTEGRATION_ENABLE_AUCTION:-false}"

if [ -z "$HOST_TARGET" ]; then
    echo "Failed to detect host target from rustc -vV" >&2
    exit 1
fi

case "$ENABLE_AUCTION" in
    true|false) ;;
    *)
        echo "INTEGRATION_ENABLE_AUCTION must be true or false" >&2
        exit 1
        ;;
esac

mkdir -p "$CONFIG_DIR"

cargo build \
    --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
    --target-dir "$INTEGRATION_TARGET_DIR" \
    --target "$HOST_TARGET" \
    --bin generate-viceroy-config

GENERATOR_BIN="$INTEGRATION_TARGET_DIR/$HOST_TARGET/debug/generate-viceroy-config"
if [ ! -x "$GENERATOR_BIN" ]; then
    echo "Generator binary not found or not executable at $GENERATOR_BIN" >&2
    exit 1
fi

GENERATOR_ARGS=(
    --template "$TEMPLATE_PATH"
    --app-config "$APP_CONFIG_PATH"
    --output "$CONFIG_DIR/viceroy.toml"
    --origin-url "$ORIGIN_URL"
)
if [ "$ENABLE_AUCTION" = true ]; then
    GENERATOR_ARGS+=(--enable-auction)
fi

"$GENERATOR_BIN" "${GENERATOR_ARGS[@]}"
