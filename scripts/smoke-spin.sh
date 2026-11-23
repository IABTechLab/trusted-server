#!/usr/bin/env bash

# Exercise Spin's local SQLite config handoff and encoded secret variables.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"

smoke_require_command cargo
smoke_require_command curl
smoke_require_python
smoke_require_command spin
smoke_require_command pgrep
smoke_require_spin_version

WORKSPACE=$(smoke_make_workspace spin)
ORIGIN_PORT=${SPIN_SMOKE_ORIGIN_PORT:-19189}
BASE_PORT=${SPIN_SMOKE_PORT:-19180}
POSITIVE_PORT=$((BASE_PORT + 4))
ORIGIN_PID=""
APP_PID=""
CONFIG_PUSHED=false

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_preserve_artifacts "$WORKSPACE"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
SPIN_WASM=${SPIN_WASM_PATH:-$REPO_ROOT/target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm}
if [ -z "${SPIN_WASM_PATH:-}" ]; then
    cargo build --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" \
        --package trusted-server-adapter-spin \
        --target wasm32-wasip1 \
        --features spin \
        --release
fi
[ -f "$SPIN_WASM" ] || smoke_die "Spin Wasm artifact not found: $SPIN_WASM"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
# The positive case uses the final listener in the case sequence.
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT" "$POSITIVE_PORT"

SPIN_WORK="$WORKSPACE/spin"
mkdir -p "$SPIN_WORK"
sed \
    "s|source = \"../../target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm\"|source = \"$SPIN_WASM\"|" \
    "$REPO_ROOT/crates/trusted-server-adapter-spin/spin.toml" >"$SPIN_WORK/spin.toml"
sed \
    -e "s|= \"crates/|= \"$REPO_ROOT/crates/|g" \
    -e "s|manifest = \"fastly.toml\"|manifest = \"$REPO_ROOT/fastly.toml\"|" \
    -e "s|manifest = \"$REPO_ROOT/crates/trusted-server-adapter-spin/spin.toml\"|manifest = \"$SPIN_WORK/spin.toml\"|" \
    "$REPO_ROOT/edgezero.toml" >"$WORKSPACE/edgezero.toml"

SPIN_HANDLER_VAR=v_trusted_x5fserver_x5fsecrets_v_handler_x5fpassword
SPIN_PROXY_VAR=v_trusted_x5fserver_x5fsecrets_v_publisher_x5fproxy_x5fsecret
SPIN_EC_VAR=v_trusted_x5fserver_x5fsecrets_v_ec_x5fpassphrase

run_case() {
    local case_name="$1"
    local port="$2"
    local missing_variable="$3"
    local expected_status="$4"
    local expected_diagnostic="${5:-}"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"
    local component_log_dir="$WORKSPACE/$case_name-component-logs"
    local spin_args=(
        spin up
        --from "$SPIN_WORK"
        --listen "127.0.0.1:$port"
        --log-dir "$component_log_dir"
        --truncate-logs
    )
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    if [ "$missing_variable" != "$SPIN_HANDLER_VAR" ]; then
        spin_args+=(--variable "$SPIN_HANDLER_VAR=$SMOKE_HANDLER_VALUE")
    fi
    if [ "$missing_variable" != "$SPIN_PROXY_VAR" ]; then
        spin_args+=(--variable "$SPIN_PROXY_VAR=$SMOKE_PROXY_VALUE")
    fi
    if [ "$missing_variable" != "$SPIN_EC_VAR" ]; then
        spin_args+=(--variable "$SPIN_EC_VAR=$SMOKE_EC_VALUE")
    fi

    "${spin_args[@]}" >"$log_path" 2>&1 &
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
    smoke_assert_process_alive "$APP_PID" "Spin" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""

    if [ -n "$expected_diagnostic" ]; then
        smoke_assert_unavailable_body "$body_path"
        grep --fixed-strings --quiet "Serving http://127.0.0.1:$port" "$log_path" ||
            smoke_die "Spin launcher did not reach the requested listener"
        if [ "$case_name" = "missing-config" ]; then
            [ "$CONFIG_PUSHED" = false ] ||
                smoke_die "missing-config case ran after the config push"
        else
            [ "$CONFIG_PUSHED" = true ] ||
                smoke_die "missing-secret case ran before the config push"
        fi
        smoke_assert_failure \
            "$expected_status" \
            "$expected_diagnostic" \
            "$component_log_dir/trusted-server_stderr.txt"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
        [ "$(smoke_origin_count "$WORKSPACE")" -gt "$origin_before" ] || smoke_die "publisher control never reached origin"
    fi
    echo "spin startup case passed: $case_name"
}

run_case missing-config "$BASE_PORT" "" 503 \
    'failed to read Spin Trusted Server app-config blob'

EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default \
    "$SMOKE_TS_BIN" config push \
    --adapter spin \
    --local \
    --manifest "$WORKSPACE/edgezero.toml" \
    --app-config "$SMOKE_APP_CONFIG" \
    --yes \
    --no-diff
CONFIG_PUSHED=true

run_case missing-handler "$((BASE_PORT + 1))" "$SPIN_HANDLER_VAR" 503 \
    "resolved secret at \`handlers[0].password\` must not be empty"
run_case missing-proxy "$((BASE_PORT + 2))" "$SPIN_PROXY_VAR" 503 \
    "resolved secret at \`publisher.proxy_secret\` must not be empty"
run_case missing-ec "$((BASE_PORT + 3))" "$SPIN_EC_VAR" 503 \
    "resolved secret at \`ec.passphrase\` must not be empty"
# Preserve the CLI-produced blob and replace only its genuine local KV entry.
SPIN_DATABASE="$SPIN_WORK/.spin/sqlite_key_value.db"
ENVELOPE=$(python3 - "$SPIN_DATABASE" <<'PYREAD'
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as connection:
    value, = connection.execute('SELECT value FROM spin_key_value WHERE store=? AND key=?', ('default', 'trusted_server_config')).fetchone()
    print(value.decode() if isinstance(value, bytes) else value)
PYREAD
)
write_runtime_envelope() {
    SMOKE_ENVELOPE="$1" python3 - "$SPIN_DATABASE" <<'PYWRITE'
import os, sqlite3, sys
with sqlite3.connect(sys.argv[1]) as connection:
    cursor = connection.execute('UPDATE spin_key_value SET value=? WHERE store=? AND key=?', (os.environ['SMOKE_ENVELOPE'].encode(), 'default', 'trusted_server_config'))
    assert cursor.rowcount == 1, 'expected exactly one CLI-produced config entry'
PYWRITE
}
INVALID_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" invalid)
HOSTILE_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" hostile)
write_runtime_envelope '{'
run_case malformed-envelope "$((BASE_PORT + 5))" "" 503 \
    'failed to parse Trusted Server app-config blob envelope'
write_runtime_envelope "$INVALID_ENVELOPE"
run_case invalid-settings "$((BASE_PORT + 6))" "" 503 'invalid_publisher_domain'
write_runtime_envelope "$HOSTILE_ENVELOPE"
run_case hostile "$POSITIVE_PORT" "" 200
write_runtime_envelope "$ENVELOPE"
run_case positive "$POSITIVE_PORT" "" 200

echo "Spin first-success smoke passed"
