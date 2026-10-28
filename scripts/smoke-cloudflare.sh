#!/usr/bin/env bash

# Exercise the Cloudflare local config store and publisher proxy path.
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
smoke_require_command pgrep

EXPECTED_WRANGLER=$(awk '$1 == "wrangler" { print $2 }' "$REPO_ROOT/.tool-versions")
[ -n "$EXPECTED_WRANGLER" ] || smoke_die "missing Wrangler pin in .tool-versions"
ACTUAL_WRANGLER=$(wrangler --version | sed -nE 's/^([0-9]+\.[0-9]+\.[0-9]+)$/\1/p')
[ "$ACTUAL_WRANGLER" = "$EXPECTED_WRANGLER" ] ||
    smoke_die "Wrangler $EXPECTED_WRANGLER is required; found ${ACTUAL_WRANGLER:-unknown}"

WORKSPACE=$(smoke_make_workspace cloudflare)
ORIGIN_PORT=${CLOUDFLARE_SMOKE_ORIGIN_PORT:-19089}
BASE_PORT=${CLOUDFLARE_SMOKE_PORT:-19080}
ORIGIN_PID=""
APP_PID=""
# The config store's binding, its default platform name.
CONFIG_BINDING=trusted_server_config

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
# The push runs Wrangler in the manifest's folder with its default
# `wrangler.toml`, and every Worker below runs from a copy of
# `wrangler.ci.toml`, so both are the CI manifest and name one local
# config store namespace.
cp "$CLOUDFLARE_ROOT/wrangler.ci.toml" "$CLOUDFLARE_WORK/wrangler.toml"
cp "$CLOUDFLARE_ROOT/wrangler.ci.toml" "$CLOUDFLARE_WORK/wrangler.ci.toml"
ln -s "$CLOUDFLARE_ROOT/build" "$CLOUDFLARE_WORK/build"
sed \
    -e "s|= \"crates/|= \"$REPO_ROOT/crates/|g" \
    -e "s|manifest = \"fastly.toml\"|manifest = \"$REPO_ROOT/fastly.toml\"|" \
    -e "s|manifest = \"$REPO_ROOT/crates/trusted-server-adapter-cloudflare/wrangler.toml\"|manifest = \"$CLOUDFLARE_WORK/wrangler.toml\"|" \
    "$REPO_ROOT/edgezero.toml" >"$WORKSPACE/edgezero.toml"

write_runtime_config() {
    local destination="$1"
    local missing_key="${2:-}"
    # The secrets go at the end, under the manifest's last table, [vars].
    awk \
        -v missing_key="$missing_key" \
        -v proxy="$SMOKE_PROXY_VALUE" \
        -v ec="$SMOKE_EC_VALUE" '
        { print }
        END {
            if (missing_key != "publisher_proxy_secret") {
                print "publisher_proxy_secret = \"" proxy "\""
            }
            if (missing_key != "ec_passphrase") {
                print "ec_passphrase = \"" ec "\""
            }
        }
    ' "$CLOUDFLARE_WORK/wrangler.ci.toml" >"$destination"
}

run_case() {
    local case_name="$1"
    local port="$2"
    local missing_key="$3"
    local expected_status="$4"
    local missing_binding="${5:-}"
    local expected_diagnostic="${6:-}"
    local config_path="$CLOUDFLARE_WORK/wrangler.$case_name.toml"
    local log_path="$WORKSPACE/$case_name.log"
    local body_path="$WORKSPACE/$case_name.body"
    local headers_path="$WORKSPACE/$case_name.headers"

    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"

    write_runtime_config "$config_path" "$missing_key"
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

    if [ "$expected_status" != 200 ]; then
        [ -n "$expected_diagnostic" ] ||
            smoke_die "negative case $case_name names no log line to wait for"
        grep --fixed-strings --quiet 'Your Worker has access to the following bindings:' "$log_path" ||
            smoke_die "Wrangler did not report the runtime binding inventory"
        grep --fixed-strings --quiet "env.$CONFIG_BINDING" "$log_path" ||
            smoke_die "negative case did not bind the config store"
        if [ -n "$missing_binding" ] && grep --fixed-strings --quiet "$missing_binding" "$log_path"; then
            smoke_die "negative case unexpectedly exposed $missing_binding"
        fi
        # The Worker's log must name the missing input, so a failure of another
        # kind cannot pass the case. Wrangler prints the Worker's console lines
        # with its own.
        smoke_assert_failure "$expected_status" "$expected_diagnostic" "$log_path"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
    fi
}

# Before the push the config store holds no blob, so the Worker cannot load.
run_case missing-config "$BASE_PORT" "" 500 "" \
    "failed to read Trusted Server app config key \`trusted_server_config\` from config store \`$CONFIG_BINDING\`"

(
    cd "$WORKSPACE"
    "$SMOKE_TS_BIN" config push \
        --adapter cloudflare \
        --local \
        --manifest "$WORKSPACE/edgezero.toml" \
        --app-config "$SMOKE_APP_CONFIG" \
        --permissions "$SMOKE_POLICY_FILE" \
        --yes \
        --no-diff
)
ENVELOPE=$(
    cd "$CLOUDFLARE_WORK"
    wrangler kv key get trusted_server_config \
        --binding "$CONFIG_BINDING" \
        --local \
        --config wrangler.toml
)
[ -n "$ENVELOPE" ] || smoke_die "Wrangler KV read-back returned an empty envelope"

run_case missing-proxy "$((BASE_PORT + 1))" publisher_proxy_secret 500 \
    'env.publisher_proxy_secret' \
    "failed to resolve secret reference at \`publisher.proxy_secret\`"
run_case missing-ec "$((BASE_PORT + 2))" ec_passphrase 500 'env.ec_passphrase' \
    "failed to resolve secret reference at \`ec.hmac.passphrase\`"
run_case positive "$((BASE_PORT + 3))" "" 200 ""

echo "Cloudflare first-success smoke passed"
