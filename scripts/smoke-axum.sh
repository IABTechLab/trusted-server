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
ORIGIN_PID=""
APP_PID=""

stop_app() {
    [ -n "$APP_PID" ] || return 0
    kill -TERM "$APP_PID" 2>/dev/null || true
    local attempt=0
    while kill -0 "$APP_PID" 2>/dev/null && [ "$attempt" -lt 30 ]; do
        sleep 0.1
        attempt=$((attempt + 1))
    done
    kill -KILL "$APP_PID" 2>/dev/null || true
    # Do not enter an unbounded wait if even SIGKILL has not completed.
    attempt=0
    while kill -0 "$APP_PID" 2>/dev/null && [ "$attempt" -lt 20 ]; do
        sleep 0.1
        attempt=$((attempt + 1))
    done
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        wait "$APP_PID" 2>/dev/null || true
    else
        echo "smoke: Axum did not exit within the teardown deadline" >&2
    fi
    APP_PID=""
}

cleanup() {
    stop_app
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
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT"
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

run_case() {
    local case_name="$1"
    local port="$2"
    local config_value="$3"
    local handler_value="$4"
    local proxy_value="$5"
    local ec_value="$6"
    local expected_outcome="$7"
    local expected_diagnostic="${8:-}"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"
    local config_dir="$WORKSPACE/$case_name-config"
    local data_dir="$WORKSPACE/$case_name-data"
    mkdir -p "$config_dir" "$data_dir"
    config_dir=$(cd "$config_dir" && pwd -P)
    data_dir=$(cd "$data_dir" && pwd -P)
    # Local config is an outer key-to-string map, not the envelope document.
    jq --null-input --arg envelope "$config_value" \
        'if $envelope == "" then {} else {trusted_server_config: $envelope} end' \
        >"$config_dir/local-config-trusted_server_config.json"
    local env_args=(
        env -i
        "PATH=$PATH"
        "RUST_LOG=info"
        "EDGEZERO__ADAPTER__HOST=127.0.0.1"
        "EDGEZERO__ADAPTER__PORT=$port"
        "EDGEZERO__ADAPTER__CONFIG_DIR=$config_dir"
        "EDGEZERO__ADAPTER__DATA_DIR=$data_dir"
        "EDGEZERO__ADAPTER__SHUTDOWN_GRACE_SECONDS=1"
        # Valid legacy values must never rescue a missing prepared input.
        "$SMOKE_CONFIG_ENV=$ENVELOPE"
        "$SMOKE_HANDLER_ENV=$SMOKE_HANDLER_VALUE"
        "$SMOKE_PROXY_ENV=$SMOKE_PROXY_VALUE"
        "$SMOKE_EC_ENV=$SMOKE_EC_VALUE"
        "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://poison.example.com"
    )
    [ -z "$handler_value" ] || env_args+=("EDGEZERO__SECRETS__HANDLER_PASSWORD=$handler_value")
    [ -z "$proxy_value" ] || env_args+=("EDGEZERO__SECRETS__PUBLISHER_PROXY_SECRET=$proxy_value")
    [ -z "$ec_value" ] || env_args+=("EDGEZERO__SECRETS__EC_PASSPHRASE=$ec_value")

    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"

    "${env_args[@]}" "$AXUM_BINARY" >"$log_path" 2>&1 &
    APP_PID=$!
    local attempt=0
    local status="000"
    if [ "$expected_outcome" = "exit" ]; then
        while kill -0 "$APP_PID" 2>/dev/null && [ "$attempt" -lt 100 ]; do
            status=$(curl --silent --max-time 0.2 --output /dev/null \
                --write-out '%{http_code}' "http://127.0.0.1:$port/health" || true)
            [ "$status" = "000" ] || smoke_die "$case_name listened before rejecting startup"
            attempt=$((attempt + 1))
            sleep 0.05
        done
        if kill -0 "$APP_PID" 2>/dev/null; then
            smoke_die "$case_name did not fail within the startup deadline"
        fi
        if wait "$APP_PID"; then
            smoke_die "$case_name exited successfully instead of rejecting startup"
        fi
        APP_PID=""
        smoke_wait_log "$log_path" "$expected_diagnostic"
    else
        while [ "$attempt" -lt 100 ]; do
            smoke_assert_process_alive "$APP_PID" "Axum" "$log_path"
            status=$(curl --silent --max-time 0.2 --output "$body_path" \
                --write-out '%{http_code}' "http://127.0.0.1:$port/health" || true)
            if [ "$status" = "200" ] && [ "$(<"$body_path")" = "ok" ] && \
                [ "$(wc -c <"$body_path")" -eq 2 ]; then
                break
            fi
            attempt=$((attempt + 1))
            sleep 0.1
        done
        [ "$attempt" -lt 100 ] || smoke_die "Axum health did not become exactly 200/ok"
        SMOKE_HTTP_STATUS=$(curl --silent --show-error --max-time 5 \
            --output "$body_path" --dump-header "$headers_path" \
            --write-out '%{http_code}' "http://127.0.0.1:$port/")
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
        smoke_assert_process_alive "$APP_PID" "Axum" "$log_path"
        stop_app
    fi
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
}

run_case missing-config "$BASE_PORT" "" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" exit \
    "Trusted Server config envelope is missing"
run_case missing-handler "$((BASE_PORT + 1))" "$ENVELOPE" \
    "" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" exit \
    "a required app-config secret is missing"
run_case missing-proxy "$((BASE_PORT + 2))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "" "$SMOKE_EC_VALUE" exit \
    "a required app-config secret is missing"
run_case missing-ec "$((BASE_PORT + 3))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "" exit \
    "a required app-config secret is missing"
run_case positive "$((BASE_PORT + 4))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 200

echo "Axum first-success smoke passed"
