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

smoke_require_python() {
    smoke_require_command python3
    python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' ||
        smoke_die "Python 3.11 or newer is required for runtime config-store fixtures"
}

smoke_require_spin_version() {
    # `.tool-versions` is the single source of truth for the pinned version, so
    # the floor is read from there rather than restated here. spin-sdk 8 imports
    # WASI HTTP 0.3, which Spin 4.0.x cannot link.
    local versions="${BASH_SOURCE[0]%/*}/../.tool-versions"
    local minimum
    minimum=$(awk '$1 == "spin" { print $2; exit }' "$versions" 2>/dev/null)
    [ -n "$minimum" ] || smoke_die "no spin version pinned in .tool-versions"
    local raw
    raw=$(spin --version 2>/dev/null) || smoke_die "could not run: spin --version"
    local found
    found=$(printf '%s\n' "$raw" | sed -n '1s/^spin \([0-9][0-9.]*\).*/\1/p')
    [ -n "$found" ] || smoke_die "could not parse a version from: $raw"
    # The component still builds and its tests still pass, so only `spin up`
    # surfaces the mismatch; check the runtime before starting the app.
    local lowest
    lowest=$(printf '%s\n%s\n' "$minimum" "$found" | sort -V | head -1)
    if [ "$lowest" != "$minimum" ]; then
        smoke_die "spin $minimum or newer is required (found $found); spin-sdk 8 imports WASI HTTP 0.3, which earlier runtimes cannot link. Install from https://spinframework.dev/install"
    fi
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
    [ -n "$pid" ] || return 0
    # Capture child PIDs while the parent is alive; after the parent dies they
    # are reparented and can no longer be found through it.
    local children=""
    children=$(pgrep -P "$pid" 2>/dev/null || true)
    local child
    for child in $children; do
        kill -TERM "$child" 2>/dev/null || true
    done
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    # Only the direct parent is a shell job, so signaled children must be
    # polled until they release their sockets.
    local remaining
    for child in $children; do
        remaining=50
        while kill -0 "$child" 2>/dev/null && [ "$remaining" -gt 0 ]; do
            sleep 0.1
            remaining=$((remaining - 1))
        done
        kill -KILL "$child" 2>/dev/null || true
    done
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
    local credentials=()
    if [ "${SMOKE_AUTHENTICATE:-false}" = true ]; then
        credentials=(--user "admin:$SMOKE_HANDLER_VALUE")
    fi
    SMOKE_HTTP_STATUS="000"
    while [ "$attempt" -lt 60 ]; do
        # Bash 3.2 treats an empty array expansion as unbound under `set -u`,
        # so expand the credentials only when there are any.
        SMOKE_HTTP_STATUS=$(curl --silent --show-error --max-time 5 ${credentials[@]+"${credentials[@]}"} \
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
    if [ -z "${TS_BIN:-}" ]; then
        cargo build --locked --manifest-path "$repository/Cargo.toml" -p trusted-server-cli --bin ts
    fi
    [ -x "$candidate" ] || smoke_die "Trusted Server CLI not executable: $candidate"
    SMOKE_TS_BIN="$candidate"
}

smoke_initialize_config() {
    local repository="$1"
    local workspace="$2"
    local origin_port="$3"
    local listener_port="$4"
    SMOKE_APP_CONFIG="$workspace/trusted-server.toml"
    "$SMOKE_TS_BIN" config init --app-config "$SMOKE_APP_CONFIG"
    export TRUSTED_SERVER__PUBLISHER__DOMAIN="127.0.0.1"
    export TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN="127.0.0.1"
    export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$origin_port"
    export TRUSTED_SERVER__PUBLISHER__PUBLIC_ORIGIN="http://127.0.0.1:$listener_port"
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

# Preserve only synthetic logs and HTTP captures, never runtime config or secrets.
smoke_preserve_artifacts() {
    local workspace="$1"
    [ -n "${SMOKE_ARTIFACT_DIR:-}" ] || return 0
    mkdir -p "$SMOKE_ARTIFACT_DIR"
    python3 - "$workspace" "$SMOKE_ARTIFACT_DIR" <<'PY'
import pathlib, shutil, sys
source, destination = map(pathlib.Path, sys.argv[1:])
for path in list(source.rglob('*')):
    if path.resolve().is_relative_to(destination.resolve()):
        continue
    if path.is_file() and (path.suffix in ('.log', '.body', '.headers', '.wire') or path.name.endswith('_stderr.txt')):
        target = destination / path.relative_to(source)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
PY
}

# The generated fixture contains only ASCII and integer numeric values. Sorting
# keys and compact JSON therefore matches BlobEnvelope's canonical v1 encoding.
smoke_mutate_envelope() {
    local envelope="$1"
    local mode="$2"
    SMOKE_ENVELOPE="$envelope" python3 - "$mode" <<'PY'
import hashlib, json, os, sys
envelope = json.loads(os.environ['SMOKE_ENVELOPE'])
data = envelope['data']
canonical = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()
assert hashlib.sha256(canonical(data)).hexdigest() == envelope['sha256'], 'fixture canonicalization mismatch'
if sys.argv[1] == 'invalid':
    data['publisher']['domain'] = ''
elif sys.argv[1] == 'hostile':
    data['handlers'][0]['path'] = '^/'
    data['response_headers'] = {
        'cache-control': 'public, max-age=9999',
        'content-type': 'application/x-smoke-hostile',
        'set-cookie': 'smoke-private-sentinel=1',
        'www-authenticate': 'Basic realm="smoke-private-sentinel"',
    }
else:
    raise ValueError('unknown mutation')
envelope['sha256'] = hashlib.sha256(canonical(data)).hexdigest()
print(json.dumps(envelope, separators=(',', ':')))
PY
}

smoke_origin_count() {
    # Python's origin logs one line per request and flushes stderr immediately.
    grep -c '"[A-Z]* /' "$1/origin.log" || true
}

smoke_assert_operational() {
    local base_url="$1"
    local state="$2"
    local prefix="$3"
    local before after path status expected content_type
    local workspace
    workspace=$(dirname "$prefix")
    before=$(smoke_origin_count "$workspace")
    local paths=(health ready)
    [ "$state" != unavailable ] || paths+=(smoke/startup-unavailable)
    for path in "${paths[@]}"; do
        status=200
        expected=ok
        content_type=text/plain
        if [ "$path" != health ]; then
            expected=ready
            if [ "$state" = unavailable ]; then
                status=503
                expected=$'Service Unavailable\n'
                content_type='text/plain; charset=utf-8'
            fi
        fi
        # Capture arbitrary failure paths in the same flat response directory.
        local capture_path=${path//\//-}
        # Keep the publisher status in SMOKE_HTTP_STATUS untouched.
        local probe_status
        probe_status=$(curl --silent --show-error --max-time 10 \
            --output "$prefix-$capture_path.body" --dump-header "$prefix-$capture_path.headers" \
            --write-out '%{http_code}' "$base_url/$path?smoke=1")
        [ "$probe_status" = "$status" ] || smoke_die "$path returned $probe_status, expected $status"
        printf '%s' "$expected" | cmp -s - "$prefix-$capture_path.body" || smoke_die "$path body differs"
        python3 - "$base_url/$path?smoke=head" "$prefix-$capture_path" "$status" "$content_type" <<'PY'
import pathlib, socket, sys, urllib.parse
url, prefix, status, content_type = sys.argv[1:]
parsed = urllib.parse.urlsplit(url)

def check_headers(raw):
    lines = raw.decode('iso-8859-1').split('\r\n')
    assert lines[0].split()[1] == status, lines[0]
    headers = {}
    for line in lines[1:]:
        if ':' in line:
            name, value = line.split(':', 1)
            headers.setdefault(name.lower(), []).append(value.strip())
    assert headers.get('content-type') == [content_type], headers
    assert headers.get('cache-control') == ['no-store'], headers
    assert 'set-cookie' not in headers and 'www-authenticate' not in headers, headers

check_headers(pathlib.Path(prefix + '.headers').read_bytes().strip())
with socket.create_connection((parsed.hostname, parsed.port), timeout=10) as connection:
    connection.sendall(f'HEAD {parsed.path}?{parsed.query} HTTP/1.1\r\nHost: {parsed.netloc}\r\nConnection: close\r\n\r\n'.encode())
    wire = bytearray()
    while True:
        part = connection.recv(65536)
        if not part:
            break
        wire.extend(part)
pathlib.Path(prefix + '-head.wire').write_bytes(wire)
headers, body = bytes(wire).split(b'\r\n\r\n', 1)
check_headers(headers)
assert not body, f'HEAD returned {len(body)} bytes on the wire'
PY
    done
    after=$(smoke_origin_count "$workspace")
    [ "$before" = "$after" ] || smoke_die "probe forwarded to publisher origin"
}

smoke_assert_unavailable_body() {
    local body_path="$1"
    printf 'Service Unavailable\n' | cmp -s - "$body_path" ||
        smoke_die "startup failure must have the fixed public body"
}
