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
smoke_require_python
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
POSITIVE_PORT=$((BASE_PORT + 4))
ORIGIN_PID=""
APP_PID=""

cleanup() {
    smoke_stop_process "$APP_PID"
    smoke_stop_process "$ORIGIN_PID"
    smoke_preserve_artifacts "$WORKSPACE"
    smoke_remove_workspace "$WORKSPACE"
}
trap cleanup EXIT INT TERM

smoke_resolve_ts_binary "$REPO_ROOT"
CLOUDFLARE_ROOT="$REPO_ROOT/crates/trusted-server-adapter-cloudflare"
if [ -z "${CLOUDFLARE_BUILD_PATH:-}" ]; then
    (cd "$CLOUDFLARE_ROOT" && bash build.sh)
fi
CLOUDFLARE_BUILD_PATH=${CLOUDFLARE_BUILD_PATH:-$CLOUDFLARE_ROOT/build}
[ -f "$CLOUDFLARE_BUILD_PATH/index.js" ] ||
    smoke_die "Cloudflare Worker bundle not found: $CLOUDFLARE_ROOT/build/index.js"

smoke_start_origin "$WORKSPACE" "$ORIGIN_PORT"
ORIGIN_PID=$SMOKE_ORIGIN_PID
# The positive case uses the final listener in the case sequence.
smoke_initialize_config "$REPO_ROOT" "$WORKSPACE" "$ORIGIN_PORT" "$POSITIVE_PORT"

CLOUDFLARE_WORK="$WORKSPACE/cloudflare"
mkdir -p "$CLOUDFLARE_WORK"
cp "$CLOUDFLARE_ROOT/wrangler.toml" "$CLOUDFLARE_WORK/wrangler.toml"
cp "$CLOUDFLARE_ROOT/wrangler.ci.toml" "$CLOUDFLARE_WORK/wrangler.ci.toml"
ln -s "$CLOUDFLARE_BUILD_PATH" "$CLOUDFLARE_WORK/build"
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
    local config_line=""
    if [ -n "$config_json" ]; then
        config_line="TRUSTED_SERVER_CONFIG = '''$config_json'''"
    fi
    CLOUDFLARE_SMOKE_CONFIG_LINE="$config_line" awk \
        -v missing_key="$missing_key" \
        -v handler="$SMOKE_HANDLER_VALUE" \
        -v proxy="$SMOKE_PROXY_VALUE" \
        -v ec="$SMOKE_EC_VALUE" '
        BEGIN {
            config_line = ENVIRON["CLOUDFLARE_SMOKE_CONFIG_LINE"]
        }
        $0 == "TRUSTED_SERVER_CONFIG = \"{}\"" {
            placeholders++
            if (config_line != "") {
                print config_line
            }
            next
        }
        { print }
        END {
            if (placeholders != 1) {
                print "Cloudflare smoke template must contain one config placeholder" > "/dev/stderr"
                exit 1
            }
            if (missing_key != "handler_password") {
                print "handler_password = \"" handler "\""
            }
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
    smoke_assert_process_alive "$APP_PID" "Wrangler" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    smoke_stop_process "$APP_PID"
    APP_PID=""

    if [ -n "$expected_diagnostic" ]; then
        smoke_assert_unavailable_body "$body_path"
        grep --fixed-strings --quiet 'Your Worker has access to the following bindings:' "$log_path" ||
            smoke_die "Wrangler did not report the runtime binding inventory"
        if [ -n "$missing_binding" ] && grep --fixed-strings --quiet "$missing_binding" "$log_path"; then
            smoke_die "negative case unexpectedly exposed $missing_binding"
        fi
        if [ "$missing_binding" = "env.TRUSTED_SERVER_CONFIG" ]; then
            grep --fixed-strings --quiet 'env.handler_password' "$log_path" ||
                smoke_die "missing-config case did not retain the secret control binding"
        else
            grep --fixed-strings --quiet 'env.TRUSTED_SERVER_CONFIG' "$log_path" ||
                smoke_die "missing-secret case did not retain the config control binding"
        fi
        smoke_assert_failure \
            "$expected_status" \
            "$expected_diagnostic" \
            "$log_path"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
        [ "$(smoke_origin_count "$WORKSPACE")" -gt "$origin_before" ] || smoke_die "publisher control never reached origin"
    fi
    echo "cloudflare startup case passed: $case_name"
}

run_case missing-config "$BASE_PORT" "" "" 503 \
    'env.TRUSTED_SERVER_CONFIG' \
    'Cloudflare TRUSTED_SERVER_CONFIG is required'
run_case missing-handler "$((BASE_PORT + 1))" "$CONFIG_JSON" handler_password 503 \
    'env.handler_password' \
    "failed to resolve secret reference at \`handlers[0].password\` from secret store \`trusted_server_secrets\`"
run_case missing-proxy "$((BASE_PORT + 2))" "$CONFIG_JSON" publisher_proxy_secret 503 \
    'env.publisher_proxy_secret' \
    "failed to resolve secret reference at \`publisher.proxy_secret\` from secret store \`trusted_server_secrets\`"
run_case missing-ec "$((BASE_PORT + 3))" "$CONFIG_JSON" ec_passphrase 503 \
    'env.ec_passphrase' \
    "failed to resolve secret reference at \`ec.passphrase\` from secret store \`trusted_server_secrets\`"
INVALID_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" invalid)
HOSTILE_ENVELOPE=$(smoke_mutate_envelope "$ENVELOPE" hostile)
MALFORMED_JSON=$(jq -cn --arg app_config '{' '{app_config: $app_config}')
INVALID_JSON=$(jq -cn --arg app_config "$INVALID_ENVELOPE" '{app_config: $app_config}')
HOSTILE_JSON=$(jq -cn --arg app_config "$HOSTILE_ENVELOPE" '{app_config: $app_config}')
run_case malformed-envelope "$((BASE_PORT + 5))" "$MALFORMED_JSON" "" 503 "" \
    'failed to parse Trusted Server app-config blob envelope'
run_case invalid-settings "$((BASE_PORT + 6))" "$INVALID_JSON" "" 503 "" \
    'invalid_publisher_domain'
run_case hostile "$POSITIVE_PORT" "$HOSTILE_JSON" "" 200 "" ""
run_case positive "$POSITIVE_PORT" "$CONFIG_JSON" "" 200 "" ""

echo "Cloudflare first-success smoke passed"
