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
ORIGIN_PORT=${FASTLY_SMOKE_ORIGIN_PORT:-18880}
BASE_PORT=${FASTLY_SMOKE_PORT:-18980}
ORIGIN_PID=""
APP_PID=""
FASTLY_BACKUP="$WORKSPACE/fastly.toml.original"
FASTLY_LOCK="$REPO_ROOT/.fastly.toml.edgezero-lock"
FASTLY_LOCK_BACKUP="$WORKSPACE/fastly.toml.edgezero-lock.original"
FASTLY_LOCK_EXISTED=false
cp "$REPO_ROOT/fastly.toml" "$FASTLY_BACKUP"
if [ -f "$FASTLY_LOCK" ]; then
    cp "$FASTLY_LOCK" "$FASTLY_LOCK_BACKUP"
    FASTLY_LOCK_EXISTED=true
fi

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    cp "$FASTLY_BACKUP" "$REPO_ROOT/fastly.toml"
    if [ "$FASTLY_LOCK_EXISTED" = true ]; then
        cp "$FASTLY_LOCK_BACKUP" "$FASTLY_LOCK"
    else
        rm -f -- "$FASTLY_LOCK"
    fi
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

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
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT"

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
        --dir "$REPO_ROOT" \
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
    python3 - "$source" "$destination" "$missing_key" <<'PY'
from pathlib import Path
import re
import sys

source, destination, missing_key = sys.argv[1:]
text = Path(source).read_text(encoding="utf-8")
pattern = (
    r'\n\[\[local_server\.secret_stores\.ts_secrets\]\]\n'
    r'key = "' + re.escape(missing_key) + r'"\n'
    r'data = "[^"]*"\n'
)
updated, count = re.subn(pattern, "\n", text, count=1)
if count != 1:
    raise SystemExit(f"expected one Fastly secret block for {missing_key}; found {count}")
Path(destination).write_text(updated, encoding="utf-8")
PY
}

run_case missing-config "$BASE_PORT" 500 \
    "key 'trusted_server_config' not found in config store 'trusted_server_config'"

"$SMOKE_TS_BIN" config push \
    --adapter fastly \
    --local \
    --manifest "$REPO_ROOT/edgezero.toml" \
    --app-config "$SMOKE_APP_CONFIG" \
    --yes \
    --no-diff
cat >>"$REPO_ROOT/fastly.toml" <<EOF

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
cp "$REPO_ROOT/fastly.toml" "$CONFIGURED_FASTLY"

write_without_secret "$CONFIGURED_FASTLY" "$REPO_ROOT/fastly.toml" handler_password
run_case missing-handler "$((BASE_PORT + 1))" 500 \
    "failed to resolve secret reference at \`handlers[0].password\`"
write_without_secret "$CONFIGURED_FASTLY" "$REPO_ROOT/fastly.toml" publisher_proxy_secret
run_case missing-proxy "$((BASE_PORT + 2))" 500 \
    "failed to resolve secret reference at \`publisher.proxy_secret\`"
write_without_secret "$CONFIGURED_FASTLY" "$REPO_ROOT/fastly.toml" ec_passphrase
run_case missing-ec "$((BASE_PORT + 3))" 500 \
    "failed to resolve secret reference at \`ec.passphrase\`"

cp "$CONFIGURED_FASTLY" "$REPO_ROOT/fastly.toml"
run_case positive "$((BASE_PORT + 4))" 200

echo "Fastly first-success smoke passed"
