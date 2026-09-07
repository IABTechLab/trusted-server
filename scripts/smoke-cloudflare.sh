#!/usr/bin/env bash

# Exercise the Cloudflare local KV-to-binding bridge and publisher proxy path.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
# shellcheck source=scripts/smoke-common.sh
. "$REPO_ROOT/scripts/smoke-common.sh"

smoke_require_command bash
smoke_require_command cargo
smoke_require_command curl
smoke_require_command jq
smoke_require_command python3
smoke_require_command wrangler

EXPECTED_WRANGLER=$(awk '$1 == "wrangler" { print $2 }' "$REPO_ROOT/.tool-versions")
[ -n "$EXPECTED_WRANGLER" ] || smoke_die "missing Wrangler pin in .tool-versions"
ACTUAL_WRANGLER=$(wrangler --version | sed -nE 's/^([0-9]+\.[0-9]+\.[0-9]+)$/\1/p')
[ "$ACTUAL_WRANGLER" = "$EXPECTED_WRANGLER" ] ||
    smoke_die "Wrangler $EXPECTED_WRANGLER is required; found ${ACTUAL_WRANGLER:-unknown}"

WORKSPACE=$(smoke_make_workspace cloudflare)
ORIGIN_PORT=${CLOUDFLARE_SMOKE_ORIGIN_PORT:-18880}
BASE_PORT=${CLOUDFLARE_SMOKE_PORT:-19080}
ORIGIN_PID=""
APP_PID=""

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
CLOUDFLARE_ROOT="$REPO_ROOT/crates/trusted-server-adapter-cloudflare"
if [ ! -f "$CLOUDFLARE_ROOT/build/index.js" ]; then
    (cd "$CLOUDFLARE_ROOT" && bash build.sh)
fi
[ -f "$CLOUDFLARE_ROOT/build/index.js" ] ||
    smoke_die "Cloudflare Worker bundle not found: $CLOUDFLARE_ROOT/build/index.js"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT"

CLOUDFLARE_WORK="$WORKSPACE/cloudflare"
mkdir -p "$CLOUDFLARE_WORK"
cp "$CLOUDFLARE_ROOT/wrangler.toml" "$CLOUDFLARE_WORK/wrangler.toml"
cp "$CLOUDFLARE_ROOT/wrangler.ci.toml" "$CLOUDFLARE_WORK/wrangler.ci.toml"
ln -s "$CLOUDFLARE_ROOT/build" "$CLOUDFLARE_WORK/build"
sed \
    -e "s|= \"crates/|= \"$REPO_ROOT/crates/|g" \
    -e "s|manifest = \"fastly.toml\"|manifest = \"$REPO_ROOT/fastly.toml\"|" \
    -e "s|manifest = \"$REPO_ROOT/crates/trusted-server-adapter-cloudflare/wrangler.toml\"|manifest = \"$CLOUDFLARE_WORK/wrangler.toml\"|" \
    "$REPO_ROOT/edgezero.toml" >"$WORKSPACE/edgezero.toml"

(
    cd "$WORKSPACE"
    EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=TRUSTED_SERVER_KV \
        "$SMOKE_TS_BIN" config push \
        --adapter cloudflare \
        --local \
        --manifest "$WORKSPACE/edgezero.toml" \
        --app-config "$SMOKE_APP_CONFIG" \
        --yes \
        --no-diff
)
ENVELOPE=$(
    cd "$CLOUDFLARE_WORK"
    wrangler kv key get trusted_server_config \
        --binding TRUSTED_SERVER_KV \
        --local \
        --config wrangler.toml
)
[ -n "$ENVELOPE" ] || smoke_die "Wrangler KV read-back returned an empty envelope"
CONFIG_JSON=$(jq --compact-output --null-input \
    --arg app_config "$ENVELOPE" \
    '{app_config: $app_config}')

write_runtime_config() {
    local destination="$1"
    local config_json="$2"
    local missing_key="${3:-}"
    python3 - \
        "$CLOUDFLARE_WORK/wrangler.ci.toml" \
        "$destination" \
        "$config_json" \
        "$missing_key" \
        "$SMOKE_HANDLER_VALUE" \
        "$SMOKE_PROXY_VALUE" \
        "$SMOKE_EC_VALUE" <<'PY'
from pathlib import Path
import sys

template_path, destination, config_json, missing_key, handler, proxy, ec = sys.argv[1:]
placeholder = 'TRUSTED_SERVER_CONFIG = "{}"'
text = Path(template_path).read_text(encoding="utf-8")
if text.count(placeholder) != 1:
    raise SystemExit("Cloudflare smoke template must contain one config placeholder")
if config_json:
    text = text.replace(placeholder, f"TRUSTED_SERVER_CONFIG = '''{config_json}'''")
else:
    text = text.replace(placeholder, "")
values = {
    "handler_password": handler,
    "publisher_proxy_secret": proxy,
    "ec_passphrase": ec,
}
for key, value in values.items():
    if key != missing_key:
        text += f'{key} = "{value}"\n'
Path(destination).write_text(text, encoding="utf-8")
PY
}

run_case() {
    local case_name="$1"
    local port="$2"
    local config_json="$3"
    local missing_key="$4"
    local expected_status="$5"
    local missing_binding="${6:-}"
    local expected_diagnostic="${7:-}"
    local config_path="$CLOUDFLARE_WORK/wrangler.$case_name.toml"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"

    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"

    write_runtime_config "$config_path" "$config_json" "$missing_key"
    (
        cd "$CLOUDFLARE_WORK"
        exec wrangler dev \
            --config "$config_path" \
            --port "$port" \
            --ip 127.0.0.1
    ) >"$log_path" 2>&1 &
    APP_PID=$!
    smoke_wait_http "http://127.0.0.1:$port/" "$expected_status" \
        "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Wrangler" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""

    if [ -n "$expected_diagnostic" ]; then
        grep --fixed-strings --quiet 'Your Worker has access to the following bindings:' "$log_path" ||
            smoke_die "Wrangler did not report the runtime binding inventory"
        if grep --fixed-strings --quiet "$missing_binding" "$log_path"; then
            smoke_die "negative case unexpectedly exposed $missing_binding"
        fi
        if [ "$missing_binding" = "env.TRUSTED_SERVER_CONFIG" ]; then
            grep --fixed-strings --quiet 'env.handler_password' "$log_path" ||
                smoke_die "missing-config case did not retain the secret control binding"
        else
            grep --fixed-strings --quiet 'env.TRUSTED_SERVER_CONFIG' "$log_path" ||
                smoke_die "missing-secret case did not retain the config control binding"
        fi
        printf '%s\n' "$expected_diagnostic" >"$WORKSPACE/$case_name.diagnostic"
        smoke_assert_failure \
            "$expected_status" \
            "$expected_diagnostic" \
            "$WORKSPACE/$case_name.diagnostic"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
    fi
}

run_case missing-config "$BASE_PORT" "" "" 500 \
    'env.TRUSTED_SERVER_CONFIG' \
    'Cloudflare config binding missing: env.TRUSTED_SERVER_CONFIG'
run_case missing-handler "$((BASE_PORT + 1))" "$CONFIG_JSON" handler_password 500 \
    'env.handler_password' \
    'Cloudflare secret binding missing: env.handler_password -> handlers[0].password'
run_case missing-proxy "$((BASE_PORT + 2))" "$CONFIG_JSON" publisher_proxy_secret 500 \
    'env.publisher_proxy_secret' \
    'Cloudflare secret binding missing: env.publisher_proxy_secret -> publisher.proxy_secret'
run_case missing-ec "$((BASE_PORT + 3))" "$CONFIG_JSON" ec_passphrase 500 \
    'env.ec_passphrase' \
    'Cloudflare secret binding missing: env.ec_passphrase -> ec.passphrase'
run_case positive "$((BASE_PORT + 4))" "$CONFIG_JSON" "" 200 "" ""

echo "Cloudflare first-success smoke passed"
