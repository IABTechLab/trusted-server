#!/usr/bin/env bash

# Exercise Spin's local SQLite config handoff and encoded secret variables.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"
smoke_use_local_store_mappings default

smoke_require_command cargo
smoke_require_command curl
smoke_require_command python3
smoke_require_command spin
smoke_require_command jq
smoke_require_command pgrep
smoke_require_spin_version

WORKSPACE=$(smoke_make_workspace spin)
ORIGIN_PORT=${SPIN_SMOKE_ORIGIN_PORT:-19189}
BASE_PORT=${SPIN_SMOKE_PORT:-19180}
ORIGIN_PID=""
APP_PID=""
CONFIG_PUSHED=false

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
SPIN_WASM=${SPIN_WASM_PATH:-$REPO_ROOT/target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm}
if [ ! -f "$SPIN_WASM" ]; then
    cargo build \
        --manifest-path "$REPO_ROOT/Cargo.toml" \
        --package trusted-server-adapter-spin \
        --target wasm32-wasip1 \
        --features spin \
        --release
fi
[ -f "$SPIN_WASM" ] || smoke_die "Spin Wasm artifact not found: $SPIN_WASM"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT"
smoke_generate_bindings "$REPO_ROOT" "$WORKSPACE" spin

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

"$SMOKE_GENERATOR" --adapter spin --template "$SPIN_WORK/spin.toml" \
    --app-config "$SMOKE_APP_CONFIG" --output "$SPIN_WORK/spin.generated.toml"
# Only the isolated negative-test manifest permits empty defaults, so omission
# reaches core's native-value validation rather than failing at Spin startup.
sed 's/^required = true$/default = ""/' "$SPIN_WORK/spin.generated.toml" >"$SPIN_WORK/spin.toml"
SPIN_HANDLER_VAR=$SMOKE_HANDLER_ENV
SPIN_PROXY_VAR=$SMOKE_PROXY_ENV
SPIN_EC_VAR=$SMOKE_EC_ENV

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
    while IFS=$'\t' read -r name value; do
        [ "$name" = "$missing_variable" ] || spin_args+=(--variable "$name=$value")
    done < <(jq -r '.[] | [.native.native_name, .value] | @tsv' "$SMOKE_BINDINGS")

    "${spin_args[@]}" >"$log_path" 2>&1 &
    APP_PID=$!
    smoke_wait_http "http://127.0.0.1:$port/" "$expected_status" \
        "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Spin" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""

    if [ -n "$expected_diagnostic" ]; then
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
    fi
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
    "handlers[0].password: empty credential"
run_case missing-proxy "$((BASE_PORT + 2))" "$SPIN_PROXY_VAR" 503 \
    "publisher.proxy_secret: empty credential"
run_case missing-ec "$((BASE_PORT + 3))" "$SPIN_EC_VAR" 503 \
    "ec.passphrase: empty credential"
run_case positive "$((BASE_PORT + 4))" "" 200

echo "Spin first-success smoke passed"
