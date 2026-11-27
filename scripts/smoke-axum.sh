#!/usr/bin/env bash

# Exercise the complete Axum local config and secret handoff.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"

smoke_require_command cargo
smoke_require_command curl
smoke_require_command jq
smoke_require_python
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
    smoke_preserve_artifacts "$WORKSPACE"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
AXUM_BINARY=${AXUM_BINARY_PATH:-$REPO_ROOT/target/debug/trusted-server-axum}
if [ -z "${AXUM_BINARY_PATH:-}" ]; then
    cargo build --locked --manifest-path "$REPO_ROOT/Cargo.toml" -p trusted-server-adapter-axum
fi
[ -x "$AXUM_BINARY" ] || smoke_die "Axum binary not executable: $AXUM_BINARY"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT" "$((BASE_PORT + 4))"
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
    local expected_status="$7"
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
        "EDGEZERO__LOGGING__LEVEL=info"
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
    local startup_state=ready
    [ "$expected_status" != 503 ] || startup_state=unavailable
    smoke_wait_http "http://127.0.0.1:$port/health" 200 \
        "$WORKSPACE/$case_name-live.body" "$WORKSPACE/$case_name-live.headers"
    if [ "$startup_state" = ready ]; then
        smoke_wait_http "http://127.0.0.1:$port/ready" 200 \
            "$WORKSPACE/$case_name-ready-wait.body" "$WORKSPACE/$case_name-ready-wait.headers"
    fi
    smoke_assert_operational "http://127.0.0.1:$port" "$startup_state" "$WORKSPACE/$case_name"
    local origin_before
    origin_before=$(smoke_origin_count "$WORKSPACE")
    local SMOKE_AUTHENTICATE=false
    if [ "$case_name" = hostile ]; then
        smoke_wait_http "http://127.0.0.1:$port/private" 401 \
            "$WORKSPACE/$case_name-auth.body" "$WORKSPACE/$case_name-auth.headers"
        SMOKE_AUTHENTICATE=true
    fi
    smoke_wait_http "http://127.0.0.1:$port/" "$expected_status" \
        "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Axum" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    stop_app

    if [ -n "$expected_diagnostic" ]; then
        smoke_assert_unavailable_body "$body_path"
        smoke_assert_failure "$expected_status" "$expected_diagnostic" "$log_path"
        # Diagnostics name config paths, never reference keys: a key is an
        # operator-supplied string that could hold a misplaced secret.
        local reference
        for reference in handler_password publisher_proxy_secret ec_passphrase; do
            if grep -qF "$reference" "$log_path"; then
                smoke_die "$case_name printed the reference key $reference"
            fi
        done
        # A diagnostic that lists every path would hide which secret is absent.
        local path
        for path in 'handlers[0].password' publisher.proxy_secret ec.passphrase; do
            case "$expected_diagnostic" in
                *"\`$path\`"*) ;;
                *)
                    if grep -qF "\`$path\`" "$log_path"; then
                        smoke_die "$case_name named the present secret path $path"
                    fi
                    ;;
            esac
        done
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
        [ "$(smoke_origin_count "$WORKSPACE")" -gt "$origin_before" ] || smoke_die "publisher control never reached origin"
    fi
    echo "axum startup case passed: $case_name"
}

run_case missing-config "$BASE_PORT" "" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 503 \
    "Trusted Server config envelope is missing"
run_case missing-handler "$((BASE_PORT + 1))" "$ENVELOPE" \
    "" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 503 \
    "required app-config secret at \`handlers[0].password\` is missing"
run_case missing-proxy "$((BASE_PORT + 2))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "" "$SMOKE_EC_VALUE" 503 \
    "required app-config secret at \`publisher.proxy_secret\` is missing"
run_case missing-ec "$((BASE_PORT + 3))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "" 503 \
    "required app-config secret at \`ec.passphrase\` is missing"
INVALID_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" invalid)
HOSTILE_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" hostile)
run_case malformed-envelope "$((BASE_PORT + 5))" '{' \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 503 \
    'failed to parse Trusted Server app-config blob envelope'
run_case invalid-settings "$((BASE_PORT + 6))" "$INVALID_ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 503 \
    'invalid_publisher_domain'
run_case hostile "$((BASE_PORT + 4))" "$HOSTILE_ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 200
run_case positive "$((BASE_PORT + 4))" "$ENVELOPE" \
    "$SMOKE_HANDLER_VALUE" "$SMOKE_PROXY_VALUE" "$SMOKE_EC_VALUE" 200

echo "Axum first-success smoke passed"
