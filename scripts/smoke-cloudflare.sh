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
smoke_require_command node
smoke_require_command setsid
smoke_require_command wrangler
smoke_require_command pgrep

EXPECTED_WRANGLER=$(awk '$1 == "wrangler" { print $2 }' "$REPO_ROOT/.tool-versions")
EXPECTED_WRANGLER=${EXPECTED_WRANGLER:-${WRANGLER_VERSION:-}}
[ -n "$EXPECTED_WRANGLER" ] || smoke_die "missing Wrangler pin in .tool-versions or WRANGLER_VERSION"

WORKSPACE=$(smoke_make_workspace cloudflare)
ORIGIN_PORT=${CLOUDFLARE_SMOKE_ORIGIN_PORT:-19089}
BASE_PORT=${CLOUDFLARE_SMOKE_PORT:-19080}
ORIGIN_PID=""
APP_PID=""

stop_worker() {
    [ -n "$APP_PID" ] || return 0
    kill -TERM -- "-$APP_PID" 2>/dev/null || true
    local attempt
    for attempt in {1..50}; do
        kill -0 -- "-$APP_PID" 2>/dev/null || break
        sleep 0.1
    done
    kill -KILL -- "-$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
    APP_PID=""
}

cleanup() {
    stop_worker
    smoke_stop_process "$ORIGIN_PID"
    if [ "${CLOUDFLARE_SMOKE_KEEP_WORKSPACE:-0}" = 1 ]; then
        echo "Cloudflare smoke artifacts: $WORKSPACE"
    else
        smoke_remove_workspace "$WORKSPACE"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Build a missing CLI with the caller's Cargo/rustup homes before isolating Wrangler.
smoke_resolve_ts_binary "$REPO_ROOT"

# Local-only Wrangler must not inherit operator credentials or configuration.
while IFS= read -r name; do
    case "$name" in
        CLOUDFLARE_*TOKEN*|CLOUDFLARE_*KEY*|CLOUDFLARE_ACCOUNT_ID|CLOUDFLARE_EMAIL|CF_*) unset "$name" ;;
    esac
done < <(compgen -e)
export HOME="$WORKSPACE/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export WRANGLER_HOME="$HOME/.wrangler"
export CLOUDFLARE_SEND_METRICS=false
mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$WRANGLER_HOME"
ACTUAL_WRANGLER=$(wrangler --version | sed -nE 's/^([0-9]+\.[0-9]+\.[0-9]+)$/\1/p')
[ "$ACTUAL_WRANGLER" = "$EXPECTED_WRANGLER" ] ||
    smoke_die "Wrangler $EXPECTED_WRANGLER is required; found ${ACTUAL_WRANGLER:-unknown}"

CLOUDFLARE_ROOT="$REPO_ROOT/crates/trusted-server-adapter-cloudflare"
[ -f "$CLOUDFLARE_ROOT/build/index.js" ] ||
    smoke_die "Cloudflare Worker bundle not found: $CLOUDFLARE_ROOT/build/index.js"

node "$REPO_ROOT/scripts/fixtures/smoke-origin.mjs" "$ORIGIN_PORT" \
    >"$WORKSPACE/origin.log" 2>&1 &
ORIGIN_PID=$!
smoke_wait_http "http://127.0.0.1:$ORIGIN_PORT/" 200 \
    "$WORKSPACE/origin-ready.body" "$WORKSPACE/origin-ready.headers"
smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
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
        exec setsid wrangler dev --local \
            --config "$config_path" \
            --port "$port" \
            --ip 127.0.0.1
    ) >"$log_path" 2>&1 &
    APP_PID=$!
    smoke_wait_http "http://127.0.0.1:$port/" "$expected_status" \
        "$body_path" "$headers_path"
    smoke_assert_process_alive "$APP_PID" "Wrangler" "$log_path"
    smoke_assert_process_alive "$ORIGIN_PID" "stub origin" "$WORKSPACE/origin.log"
    if [ -z "$expected_diagnostic" ]; then
        local upload_status
        upload_status=$(curl --silent --show-error --max-time 5 \
            --data-binary 'ts-lazy-upload-fixture' \
            --header 'Content-Type: application/octet-stream' \
            --dump-header "$WORKSPACE/upload.headers" \
            --output "$WORKSPACE/upload.body" --write-out '%{http_code}' \
            "http://127.0.0.1:$port/native-upload")
        [ "$upload_status" = 200 ] || smoke_die "publisher POST did not reach the origin"
        jq --exit-status '.received == "ts-lazy-upload-fixture"' \
            "$WORKSPACE/upload.body" >/dev/null || smoke_die "publisher POST body was lost"
        [ "$(grep -ic '^set-cookie:' "$WORKSPACE/upload.headers")" = 2 ] ||
            smoke_die "Cloudflare delivery lost duplicate Set-Cookie fields"
    fi
    stop_worker

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
        smoke_assert_failure \
            "$expected_status" \
            "$expected_diagnostic" \
            "$log_path"
    else
        smoke_assert_success "$body_path" "$ORIGIN_PORT" "$port"
    fi
}

run_case missing-config "$BASE_PORT" "" "" 500 \
    'env.TRUSTED_SERVER_CONFIG' \
    'Cloudflare TRUSTED_SERVER_CONFIG is required'
run_case missing-handler "$((BASE_PORT + 1))" "$CONFIG_JSON" handler_password 500 \
    'env.handler_password' \
    "failed to resolve secret reference at \`handlers[0].password\` from secret store \`trusted_server_secrets\`"
run_case missing-proxy "$((BASE_PORT + 2))" "$CONFIG_JSON" publisher_proxy_secret 500 \
    'env.publisher_proxy_secret' \
    "failed to resolve secret reference at \`publisher.proxy_secret\` from secret store \`trusted_server_secrets\`"
run_case missing-ec "$((BASE_PORT + 3))" "$CONFIG_JSON" ec_passphrase 500 \
    'env.ec_passphrase' \
    "failed to resolve secret reference at \`ec.passphrase\` from secret store \`trusted_server_secrets\`"
run_case positive "$((BASE_PORT + 4))" "$CONFIG_JSON" "" 200 "" ""

echo "Cloudflare first-success smoke passed"
