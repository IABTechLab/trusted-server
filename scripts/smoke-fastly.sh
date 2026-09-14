#!/usr/bin/env bash

# Exercise Fastly local config push, secret provisioning, and publisher proxying.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"

smoke_require_command cargo
smoke_require_command curl
smoke_require_command fastly
smoke_require_command python3

WORKSPACE=$(smoke_make_workspace fastly)
FASTLY_PROJECT="$WORKSPACE/project"
FASTLY_MANIFEST="$FASTLY_PROJECT/fastly.toml"
EDGEZERO_MANIFEST="$FASTLY_PROJECT/edgezero.toml"
ORIGIN_PORT=${FASTLY_SMOKE_ORIGIN_PORT:-18880}
BASE_PORT=${FASTLY_SMOKE_PORT:-18980}
ORIGIN_PID=""
APP_PID=""

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

mkdir -p "$FASTLY_PROJECT"
cp "$REPO_ROOT/edgezero.toml" "$EDGEZERO_MANIFEST"
cp "$REPO_ROOT/fastly.toml" "$FASTLY_MANIFEST"
ln -s "$REPO_ROOT/crates" "$FASTLY_PROJECT/crates"

smoke_resolve_ts_binary "$REPO_ROOT"
WASM_BINARY=${WASM_BINARY_PATH:-$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm}
if [ ! -f "$WASM_BINARY" ]; then
    cargo build \
        --manifest-path "$REPO_ROOT/Cargo.toml" \
        --package trusted-server-adapter-fastly \
        --release \
        --target wasm32-wasip1
fi
[ -f "$WASM_BINARY" ] || smoke_die "Fastly Wasm artifact not found: $WASM_BINARY"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
smoke_initialize_config "$FASTLY_PROJECT" "$WORKSPACE" "$ORIGIN_PORT"

run_case() {
    local case_name="$1"
    local port="$2"
    local expected_status="$3"
    local expected_diagnostic="${4:-}"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"

    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"

    fastly compute serve \
        --dir "$FASTLY_PROJECT" \
        --file "$WASM_BINARY" \
        --addr "127.0.0.1:$port" >"$log_path" 2>&1 &
    APP_PID=$!

    if [ "$case_name" = "missing-config" ]; then
        smoke_wait_http "http://127.0.0.1:$port/health" "200" \
            "$WORKSPACE/$case_name-health.body" \
            "$WORKSPACE/$case_name-health.headers"
    fi
    smoke_wait_http "http://127.0.0.1:$port/" "$expected_status" \
        "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Fastly CLI" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""

    if [ -n "$expected_diagnostic" ]; then
        smoke_assert_failure "$expected_status" "$expected_diagnostic" "$log_path"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
    fi
}

write_without_secret() {
    local source="$1"
    local destination="$2"
    local missing_key="$3"
    awk -v missing_key="$missing_key" '
        $0 == "[[local_server.secret_stores.ts_secrets]]" {
            header = $0
            if ((getline key_line) <= 0 || (getline data_line) <= 0) {
                exit 2
            }
            if (key_line == "key = \"" missing_key "\"" &&
                data_line ~ /^data = "[^"]*"$/) {
                removed++
                next
            }
            print header
            print key_line
            print data_line
            next
        }
        { print }
        END {
            if (removed != 1) {
                print "expected one Fastly secret block for " missing_key \
                    "; found " (removed + 0) > "/dev/stderr"
                exit 1
            }
        }
    ' "$source" >"$destination"
}

run_case missing-config "$BASE_PORT" 500 \
    "key 'trusted_server_config' not found in config store 'trusted_server_config'"

(
    cd "$FASTLY_PROJECT"
    "$SMOKE_TS_BIN" config push \
        --adapter fastly \
        --local \
        --manifest "$EDGEZERO_MANIFEST" \
        --app-config "$SMOKE_APP_CONFIG" \
        --yes \
        --no-diff
)
cat >>"$FASTLY_MANIFEST" <<EOF

[[local_server.secret_stores.ts_secrets]]
key = "handler_password"
data = "$SMOKE_HANDLER_VALUE"

[[local_server.secret_stores.ts_secrets]]
key = "publisher_proxy_secret"
data = "$SMOKE_PROXY_VALUE"

[[local_server.secret_stores.ts_secrets]]
key = "ec_passphrase"
data = "$SMOKE_EC_VALUE"
EOF
CONFIGURED_FASTLY="$WORKSPACE/fastly.toml.configured"
cp "$FASTLY_MANIFEST" "$CONFIGURED_FASTLY"

write_without_secret "$CONFIGURED_FASTLY" "$FASTLY_MANIFEST" handler_password
run_case missing-handler "$((BASE_PORT + 1))" 500 \
    "failed to resolve secret reference at \`handlers[0].password\`"
write_without_secret "$CONFIGURED_FASTLY" "$FASTLY_MANIFEST" publisher_proxy_secret
run_case missing-proxy "$((BASE_PORT + 2))" 500 \
    "failed to resolve secret reference at \`publisher.proxy_secret\`"
write_without_secret "$CONFIGURED_FASTLY" "$FASTLY_MANIFEST" ec_passphrase
run_case missing-ec "$((BASE_PORT + 3))" 500 \
    "failed to resolve secret reference at \`ec.passphrase\`"

cp "$CONFIGURED_FASTLY" "$FASTLY_MANIFEST"
run_case positive "$((BASE_PORT + 4))" 200

echo "Fastly first-success smoke passed"
