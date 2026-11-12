#!/usr/bin/env bash

# Exercise the complete Axum local config and secret handoff.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"

smoke_require_command cargo
smoke_require_command curl
smoke_require_command jq
smoke_require_command python3
smoke_require_command pgrep

WORKSPACE=$(smoke_make_workspace axum)
ORIGIN_PORT=${AXUM_SMOKE_ORIGIN_PORT:-18889}
BASE_PORT=${AXUM_SMOKE_PORT:-18890}
POSITIVE_PORT=$((BASE_PORT + 4))
ORIGIN_PID=""
APP_PID=""

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
AXUM_BINARY=${AXUM_BINARY_PATH:-$REPO_ROOT/target/debug/trusted-server-axum}
if [ ! -x "$AXUM_BINARY" ]; then
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p trusted-server-adapter-axum
fi
[ -x "$AXUM_BINARY" ] || smoke_die "Axum binary not executable: $AXUM_BINARY"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
# The positive case uses the final listener in the case sequence.
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT" "$POSITIVE_PORT"
sed \
    -e "s|= \"crates/|= \"$REPO_ROOT/crates/|g" \
    -e "s|manifest = \"fastly.toml\"|manifest = \"$REPO_ROOT/fastly.toml\"|" \
    "$REPO_ROOT/edgezero.toml" >"$WORKSPACE/edgezero.toml"
(
    cd "$WORKSPACE"
    "$SMOKE_TS_BIN" config push \
        --adapter axum \
        --local \
        --manifest "$WORKSPACE/edgezero.toml" \
        --app-config "$SMOKE_APP_CONFIG" \
        --yes \
        --no-diff
)
ENVELOPE=$(jq --exit-status --raw-output \
    '.trusted_server_config' \
    "$WORKSPACE/.edgezero/local-config-trusted_server_config.json")

# Starts Axum with the given config and secret values, or fails at startup.
#
# Without `expected_diagnostic` the publisher page must answer 200 through
# the stub origin. With it, startup must fail: Axum exits non-zero before it
# listens and logs the diagnostic.
run_case() {
    local case_name="$1"
    local port="$2"
    local config_value="$3"
    local handler_value="$4"
    local proxy_value="$5"
    local ec_value="$6"
    local expected_diagnostic="${7:-}"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"
    local env_args=(
        env
        -u "$SMOKE_CONFIG_ENV"
        -u "$SMOKE_HANDLER_ENV"
        -u "$SMOKE_PROXY_ENV"
        -u "$SMOKE_EC_ENV"
    )
    [ -z "$config_value" ] || env_args+=("$SMOKE_CONFIG_ENV=$config_value")
    [ -z "$handler_value" ] || env_args+=("$SMOKE_HANDLER_ENV=$handler_value")
    [ -z "$proxy_value" ] || env_args+=("$SMOKE_PROXY_ENV=$proxy_value")
    [ -z "$ec_value" ] || env_args+=("$SMOKE_EC_ENV=$ec_value")

    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"

    "${env_args[@]}" \
        PORT="$port" \
        "$AXUM_BINARY" >"$log_path" 2>&1 &
    APP_PID=$!

    if [ -n "$expected_diagnostic" ]; then
        assert_startup_failure "$case_name" "$log_path" "$expected_diagnostic"
        return
    fi

    smoke_wait_http "http://127.0.0.1:$port/" 200 "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Axum" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""
    smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
}

# Waits up to 15 seconds for Axum to exit, then requires a non-zero status and
# the diagnostic in its log.
assert_startup_failure() {
    local case_name="$1"
    local log_path="$2"
    local expected_diagnostic="$3"
    local attempt=0
    while kill -0 "$APP_PID" 2>/dev/null; do
        if [ "$attempt" -ge 60 ]; then
            sed -n '1,120p' "$log_path" >&2
            smoke_die "$case_name: Axum kept running after a startup failure"
        fi
        attempt=$((attempt + 1))
        sleep 0.25
    done
    local status=0
    wait "$APP_PID" || status=$?
    APP_PID=""
    if [ "$status" -eq 0 ]; then
        sed -n '1,120p' "$log_path" >&2
        smoke_die "$case_name: Axum exited 0 after a startup failure"
    fi
    smoke_wait_log "$log_path" "$expected_diagnostic"
}

run_case missing-config "$BASE_PORT" "" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" \
    "env var '$SMOKE_CONFIG_ENV' not set"
run_case missing-handler "$((BASE_PORT + 1))" "$ENVELOPE" \
    "" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" \
    "failed to resolve secret reference at \`handlers[0].password\`"
run_case missing-proxy "$((BASE_PORT + 2))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "" "$SMOKE_EC_VALUE" \
    "failed to resolve secret reference at \`publisher.proxy_secret\`"
run_case missing-ec "$((BASE_PORT + 3))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "" \
    "failed to resolve secret reference at \`ec.passphrase\`"
run_case positive "$POSITIVE_PORT" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE"

echo "Axum first-success smoke passed"
