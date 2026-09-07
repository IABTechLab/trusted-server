#!/usr/bin/env bash
# shellcheck disable=SC2034

# Shared, source-only helpers for the adapter first-success smokes.

smoke_die() {
    echo "smoke: $*" >&2
    exit 1
}

smoke_require_command() {
    command -v "$1" >/dev/null 2>&1 || smoke_die "required command not found: $1"
}

smoke_make_workspace() {
    local adapter="$1"
    local base="${TMPDIR:-/tmp}"
    mktemp -d "$base/trusted-server-smoke-$adapter.XXXXXX"
}

smoke_remove_workspace() {
    local workspace="$1"
    case "$workspace" in
        "${TMPDIR:-/tmp}"/trusted-server-smoke-*) rm -rf -- "$workspace" ;;
        *) echo "smoke: refusing to remove unexpected workspace: $workspace" >&2 ;;
    esac
}

smoke_stop_process() {
    local pid="${1:-}"
    if [ -n "$pid" ]; then
        if command -v pkill >/dev/null 2>&1; then
            pkill -TERM -P "$pid" 2>/dev/null || true
        fi
    fi
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}

smoke_assert_process_alive() {
    local pid="$1"
    local label="$2"
    local log_path="${3:-}"
    if ! kill -0 "$pid" 2>/dev/null; then
        [ -z "$log_path" ] || sed -n '1,120p' "$log_path" >&2
        smoke_die "$label exited before its assertion completed"
    fi
}

smoke_wait_http() {
    local url="$1"
    local expected_status="$2"
    local body_path="$3"
    local headers_path="$4"
    local attempt=0
    SMOKE_HTTP_STATUS="000"
    while [ "$attempt" -lt 60 ]; do
        SMOKE_HTTP_STATUS=$(curl --silent --show-error \
            --output "$body_path" \
            --dump-header "$headers_path" \
            --write-out '%{http_code}' \
            "$url" 2>/dev/null || true)
        if [ "$SMOKE_HTTP_STATUS" = "$expected_status" ]; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 0.25
    done
    smoke_die "expected HTTP $expected_status from $url; received $SMOKE_HTTP_STATUS"
}

smoke_wait_log() {
    local log_path="$1"
    local expected="$2"
    local attempt=0
    while [ "$attempt" -lt 40 ]; do
        if grep --fixed-strings --quiet -- "$expected" "$log_path"; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 0.1
    done
    echo "smoke: expected diagnostic not found: $expected" >&2
    sed -n '1,120p' "$log_path" >&2
    exit 1
}

smoke_assert_failure() {
    local expected_status="$1"
    local expected_diagnostic="$2"
    local log_path="$3"
    [ "$SMOKE_HTTP_STATUS" = "$expected_status" ] ||
        smoke_die "negative case returned HTTP $SMOKE_HTTP_STATUS, expected $expected_status"
    smoke_wait_log "$log_path" "$expected_diagnostic"
}

smoke_assert_success() {
    local body_path="$1"
    local origin_port="$2"
    local adapter_port="$3"
    [ "$SMOKE_HTTP_STATUS" = "200" ] ||
        smoke_die "publisher request returned HTTP $SMOKE_HTTP_STATUS, expected 200"
    grep --fixed-strings --quiet 'SMOKE_ORIGIN_SENTINEL' "$body_path" ||
        smoke_die "publisher response omitted the stub-origin sentinel"
    grep --fixed-strings --quiet \
        "href=\"http://127.0.0.1:$adapter_port/asset\"" "$body_path" ||
        smoke_die "publisher response omitted the Trusted Server URL rewrite"
    if grep --fixed-strings --quiet \
        "href=\"http://127.0.0.1:$origin_port/asset\"" "$body_path"; then
        smoke_die "publisher response retained the origin URL"
    fi
}

smoke_start_origin() {
    local workspace="$1"
    local origin_port="$2"
    local origin_dir="$workspace/origin"
    mkdir -p "$origin_dir"
    printf '%s\n' \
        "<!doctype html><html><body>SMOKE_ORIGIN_SENTINEL<a href=\"http://127.0.0.1:$origin_port/asset\">asset</a></body></html>" \
        >"$origin_dir/index.html"
    python3 -m http.server "$origin_port" --bind 127.0.0.1 --directory "$origin_dir" \
        >"$workspace/origin.log" 2>&1 &
    SMOKE_ORIGIN_PID=$!
    smoke_wait_http "http://127.0.0.1:$origin_port/" "200" \
        "$workspace/origin-ready.body" "$workspace/origin-ready.headers"
    smoke_assert_process_alive "$SMOKE_ORIGIN_PID" "stub origin" "$workspace/origin.log"
    grep --fixed-strings --quiet 'SMOKE_ORIGIN_SENTINEL' "$workspace/origin-ready.body" ||
        smoke_die "stub origin did not return its sentinel"
}

smoke_resolve_ts_binary() {
    local repository="$1"
    local candidate="${TS_BIN:-$repository/target/debug/ts}"
    if [ ! -x "$candidate" ]; then
        cargo build --manifest-path "$repository/Cargo.toml" -p trusted-server-cli --bin ts
    fi
    [ -x "$candidate" ] || smoke_die "Trusted Server CLI not executable: $candidate"
    SMOKE_TS_BIN="$candidate"
}

smoke_initialize_config() {
    local repository="$1"
    local workspace="$2"
    local origin_port="$3"
    SMOKE_APP_CONFIG="$workspace/trusted-server.toml"
    "$SMOKE_TS_BIN" config init --app-config "$SMOKE_APP_CONFIG"
    export TRUSTED_SERVER__PUBLISHER__DOMAIN="127.0.0.1"
    export TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN="127.0.0.1"
    export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$origin_port"
    "$SMOKE_TS_BIN" config validate \
        --manifest "$repository/edgezero.toml" \
        --app-config "$SMOKE_APP_CONFIG" \
        --strict
}

SMOKE_CONFIG_ENV=TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG
SMOKE_HANDLER_ENV=TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD
SMOKE_PROXY_ENV=TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET
SMOKE_EC_ENV=TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE
SMOKE_HANDLER_VALUE=smoke-admin-password-32-bytes-ok
SMOKE_PROXY_VALUE=smoke-publisher-proxy-secret-32-bytes
SMOKE_EC_VALUE=smoke-edge-cookie-passphrase-32-bytes
