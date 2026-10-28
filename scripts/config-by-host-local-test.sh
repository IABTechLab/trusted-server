#!/usr/bin/env bash
#
# Local harness for a service that reads one app-config blob for each host.
#
# Runs Trusted Server under Viceroy against four stub origins, one behind the
# blob at the logical store ID and one behind each other blob, and asserts
# that every host is answered from the blob its selector names and from no
# other. The selector is the `TS__CONFIG__KEY` entry in the
# `trusted_server_config` store, and Viceroy is started afresh for each value
# it takes. Everything it needs is generated into a temp directory, so your
# `trusted-server.toml` is never read and your tracked `fastly.toml` is never
# modified.
#
# Usage:
#   ./scripts/config-by-host-local-test.sh          # a fresh sandbox for each request
#   ./scripts/config-by-host-local-test.sh reuse    # one sandbox serves both publishers
#
# `reuse` builds with the reusable sandbox and sets its four bounds.
#
# Environment:
#   WASM_BINARY_PATH   a built adapter, in place of building one
#   TS_BIN             a built `ts`, in place of building one
#   ORIGIN_PORT        the first of four consecutive stub-origin ports
#   TS_PORT            the port Viceroy listens on

set -euo pipefail

MODE="${1:-single}"
case "$MODE" in
  single | reuse) ;;
  *)
    echo "Unknown mode '$MODE'. Use one of: single, reuse." >&2
    exit 1
    ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
ORIGIN_PORT="${ORIGIN_PORT:-9199}"
TS_PORT="${TS_PORT:-7799}"
PORT_A="$ORIGIN_PORT"
PORT_B="$((ORIGIN_PORT + 1))"
PORT_SHARED="$((ORIGIN_PORT + 2))"
PORT_V2="$((ORIGIN_PORT + 3))"
HOST_A="a.publisher.example"
HOST_B="b.publisher.example"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
SELECTOR_ENTRY="TS__CONFIG__KEY"
# A selector in an `edgezero_runtime_env` store, scoped by the service id
# Viceroy reports. The adapter does not read that store.
OLD_SELECTOR_ENTRY="EDGEZERO__SERVICES__0000000000000000000000__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY"
REQUEST_TIMEOUT_SECONDS="${REQUEST_TIMEOUT_SECONDS:-30}"

PASS=0
FAIL=0
VICEROY_PID=""
ORIGIN_PID=""

info() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
ok() { printf '  \033[32mPASS\033[0m %s\n' "$*"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL + 1)); }

check() { # check <description> <actual> <expected>
  if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1: got '$2', want '$3'"; fi
}

stop_viceroy() {
  if [ -n "$VICEROY_PID" ]; then
    kill "$VICEROY_PID" 2>/dev/null || true
    wait "$VICEROY_PID" 2>/dev/null || true
    VICEROY_PID=""
  fi
}

cleanup() {
  local status=$?
  stop_viceroy
  if [ -n "$ORIGIN_PID" ]; then
    # macOS may launch the framework Python process as a child of the shim.
    pkill -TERM -P "$ORIGIN_PID" 2>/dev/null || true
    kill "$ORIGIN_PID" 2>/dev/null || true
    wait "$ORIGIN_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
  exit $status
}
trap cleanup EXIT INT TERM

for tool in viceroy curl; do
  command -v "$tool" >/dev/null || {
    echo "$tool not found. Viceroy installs with: cargo install viceroy --version 0.17.0 --locked" >&2
    exit 1
  }
done
PYTHON="$(command -v python3 || command -v python || true)"
[ -n "$PYTHON" ] || {
  echo "python3 not found. The harness runs its stub origins with it." >&2
  exit 1
}

# A port already in use means requests would go to something else entirely,
# most likely a leftover run whose config would read as a result.
for port in "$PORT_A" "$PORT_B" "$PORT_SHARED" "$PORT_V2" "$TS_PORT"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
    echo "Port $port is already in use. Stop the process, or set" >&2
    echo "ORIGIN_PORT / TS_PORT to something free." >&2
    exit 1
  fi
done

info "Building (debug wasm + ts CLI, mode: $MODE)"
WASM="${WASM_BINARY_PATH:-}"
if [ -z "$WASM" ]; then
  if [ "$MODE" = "reuse" ]; then
    cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1 \
      --features reusable-sandbox >/dev/null
  else
    cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1 >/dev/null
  fi
  # Copied aside, because a later build with other features writes the same path.
  cp "$REPO_ROOT/target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm" "$WORK/app.wasm"
  WASM="$WORK/app.wasm"
fi
TS="${TS_BIN:-}"
if [ -z "$TS" ]; then
  cargo build -p trusted-server-cli --target "$HOST_TRIPLE" >/dev/null
  TS="$REPO_ROOT/target/$HOST_TRIPLE/debug/ts"
fi

info "Starting stub origins on :$PORT_A, :$PORT_B, :$PORT_SHARED and :$PORT_V2"
cat > "$WORK/origins.py" <<'PYEOF'
"""Stub publisher origins, each answering with a marker of its own.

A page is private and not to be stored, so no cache stands between a request
and the blob that serves it, and a reused sandbox reports its counters on it.
"""
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def handler(name):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            body = (
                "<!doctype html>\n<html><head><title>%s</title></head>\n"
                "<body><p>ORIGIN_%s_SENTINEL</p></body></html>\n" % (name, name)
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "private, no-store")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, fmt, *args):
            print("origin %s: " % name + fmt % args, flush=True)

    return Handler


servers = [
    ThreadingHTTPServer(("127.0.0.1", int(port)), handler(name))
    for name, port in (pair.split("=") for pair in sys.argv[1:])
]
for server in servers[1:]:
    threading.Thread(target=server.serve_forever, daemon=True).start()
servers[0].serve_forever()
PYEOF
"$PYTHON" "$WORK/origins.py" "A=$PORT_A" "B=$PORT_B" "SHARED=$PORT_SHARED" "V2=$PORT_V2" \
  > "$WORK/origins.log" 2>&1 &
ORIGIN_PID=$!
for port in "$PORT_A" "$PORT_B" "$PORT_SHARED" "$PORT_V2"; do
  for _ in $(seq 1 40); do
    curl -s -o /dev/null "http://127.0.0.1:$port/" && break
    sleep 0.25
  done
done

info "Seeding an isolated config store (tracked fastly.toml remains untouched)"
# `config push --local` edits the adapter manifest, so it is given a complete
# temporary project. The crates link keeps manifest path validation pointed at
# this checkout without copying the workspace.
cp "$REPO_ROOT/edgezero.toml" "$WORK/edgezero.toml"
cp "$REPO_ROOT/fastly.toml" "$WORK/fastly.toml"
ln -s "$REPO_ROOT/crates" "$WORK/crates"
cat >> "$WORK/fastly.toml" <<'SECRETSEOF'

[[local_server.secret_stores.trusted_server_secrets]]
key = "publisher_proxy_secret"
data = "fictional-local-publisher-proxy-secret-value"

[[local_server.secret_stores.trusted_server_secrets]]
key = "ec_passphrase"
data = "fictional-local-ec-passphrase-secret-value"
SECRETSEOF

"$TS" config init --app-config "$WORK/app.toml" >/dev/null
# The example names no permission policy, so the blobs name the repository's
# sample, and every push supplies its file.
POLICY_FILE="$REPO_ROOT/config/permissions/sample.yaml"
sed -e 's/^# \[permissions\]$/[permissions]/' \
  -e 's/^# policy = "standard"$/policy = "sample"/' \
  "$WORK/app.toml" > "$WORK/app.policy.toml"
mv "$WORK/app.policy.toml" "$WORK/app.toml"
for line in '[permissions]' 'policy = "sample"'; do
  grep --fixed-strings --line-regexp --quiet -- "$line" "$WORK/app.toml" || {
    echo "could not name the sample policy in $WORK/app.toml" >&2
    exit 1
  }
done
# Every response then says which sandbox served it and how many applications
# that sandbox had built.
printf '\n[debug]\nsandbox_metrics_enabled = true\n' >> "$WORK/app.toml"

push_blob() { # push_blob <publisher domain> <origin port> [--key <key>]
  local domain="$1" port="$2"
  shift 2
  (
    cd "$WORK"
    TRUSTED_SERVER__PUBLISHER__DOMAIN="$domain" \
      TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=".$domain" \
      TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$port" \
      "$TS" config push --adapter fastly --local \
      --manifest "$WORK/edgezero.toml" --app-config "$WORK/app.toml" \
      --permissions "$POLICY_FILE" --no-diff --yes "$@" >/dev/null
  )
}
# One blob at the logical store ID, one under each publisher's host, and one
# under publisher A's host with a suffix. The blob at the logical store ID
# carries the policy file, and the keyed pushes keep it once, in its shared
# entry.
push_blob "shared.example" "$PORT_SHARED"
push_blob "$HOST_A" "$PORT_A" --key "$HOST_A"
push_blob "$HOST_B" "$PORT_B" --key "$HOST_B"
push_blob "$HOST_A" "$PORT_V2" --key "${HOST_A}_v2"
# Each case starts from the pushed blobs alone.
cp "$WORK/fastly.toml" "$WORK/fastly.base.toml"

add_entries() { # add_entries <config store> <KEY=value>...
  "$PYTHON" - "$WORK/fastly.toml" "$@" <<'PYEOF'
import sys

manifest, store, *entries = sys.argv[1:]
header = "[local_server.config_stores.%s.contents]" % store
lines = open(manifest).read().split("\n")
at = [index for index, line in enumerate(lines) if line.strip() == header]
if len(at) > 1:
    raise SystemExit("expected one %s table in the local manifest" % header)
if not at:
    lines += ["", "[local_server.config_stores.%s]" % store, 'format = "inline-toml"', header]
    at = [len(lines) - 1]
for entry in reversed(entries):
    key, value = entry.split("=", 1)
    lines.insert(at[0] + 1, '%s = "%s"' % (key, value))
open(manifest, "w").write("\n".join(lines))
PYEOF
}

use_selector() { # use_selector <selector, or "" for none> [<config store> <KEY=value>]
  cp "$WORK/fastly.base.toml" "$WORK/fastly.toml"
  if [ -n "$1" ]; then
    add_entries trusted_server_config "$SELECTOR_ENTRY=$1"
  fi
  # The sandbox bounds sit unscoped beside the blobs.
  if [ "$MODE" = "reuse" ]; then
    add_entries trusted_server_config \
      "TS__SANDBOX__MAX_REQUESTS=50" \
      "TS__SANDBOX__MAX_LIFETIME_MS=120000" \
      "TS__SANDBOX__TIMEOUT_MS=10000" \
      "TS__SANDBOX__MAX_MEMORY_MIB=512"
  fi
  if [ "$#" -eq 3 ]; then
    add_entries "$2" "$3"
  fi
}

start_viceroy() { # start_viceroy <log name>
  VICEROY_LOG="$WORK/$1.log"
  # Deliberately not wrapped in a subshell: `$!` would then be the subshell's
  # pid, so cleanup would kill the wrapper and orphan Viceroy.
  RUST_LOG=info viceroy serve -C "$WORK/fastly.toml" \
    --addr "127.0.0.1:$TS_PORT" "$WASM" > "$VICEROY_LOG" 2>&1 &
  VICEROY_PID=$!
  for _ in $(seq 1 40); do
    grep -q "Listening on" "$VICEROY_LOG" 2>/dev/null && break
    sleep 0.5
  done
  if ! grep -q "Listening on" "$VICEROY_LOG" 2>/dev/null; then
    echo "Trusted Server failed to start. Last log lines:" >&2
    tail -20 "$VICEROY_LOG" >&2
    exit 1
  fi
}

REQUESTS=0
fetch() { # fetch <host> [path], sets STATUS, BODY and HEADERS
  REQUESTS=$((REQUESTS + 1))
  BODY="$WORK/response-$REQUESTS.body"
  HEADERS="$WORK/response-$REQUESTS.headers"
  STATUS="$(curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" -D "$HEADERS" -o "$BODY" \
    -w '%{http_code}' -H "Host: $1" "http://127.0.0.1:$TS_PORT${2:-/}" || true)"
}

no_page() { # no_page <description>, about the last response
  if grep -qE "ORIGIN_(A|B|SHARED|V2)_SENTINEL" "$BODY"; then
    bad "$1: the answer carries a publisher's page"
  else
    ok "$1: no publisher's page is served"
  fi
}

served_by() { # served_by <description> <host> <origin name>
  local found=""
  fetch "$2"
  for name in A B SHARED V2; do
    if grep -qF "ORIGIN_${name}_SENTINEL" "$BODY"; then found="$found$name"; fi
  done
  check "$1" "$STATUS $found" "200 $3"
}

counter() { # counter <ordinal|builds>, from the last response
  tr -d '\r' < "$HEADERS" |
    awk -v name="x-ts-sandbox-$1:" 'tolower($1) == name { print $2 }'
}

refused() { # refused <description> <host>
  fetch "$2"
  check "$1" "$STATUS" "421"
  no_page "$1"
}

info "Without a selector every host reads the blob at the logical store ID"
use_selector ""
start_viceroy no-selector
served_by "a publisher's host reads the blob at the logical store ID" "$HOST_A" SHARED
served_by "any other host reads it too" "unknown.example" SHARED
stop_viceroy

info "With '{host}' as the selector each host reads its own blob"
use_selector "{host}"
start_viceroy by-host
served_by "publisher A's host reads publisher A's blob" "$HOST_A" A
served_by "publisher B's host reads publisher B's blob" "$HOST_B" B
served_by "publisher A again, after B" "$HOST_A" A
served_by "publisher B again, after A" "$HOST_B" B
# The fourth response says how it was served. A sandbox on its fourth request
# that has built exactly two applications shows the two publishers shared a
# sandbox and never an application. Viceroy reports one instance id for every
# sandbox, so the request's place in its sandbox is what tells them apart.
if [ "$MODE" = "reuse" ]; then
  check "one sandbox served all four requests and built an application for each publisher" \
    "$(counter ordinal) $(counter builds)" "4 2"
else
  check "a fresh sandbox served the request and built its one application" \
    "$(counter ordinal) $(counter builds)" "1 1"
fi
served_by "a host in capitals with a port reads the same blob" "A.Publisher.Example:8443" A
served_by "a host with a trailing dot reads the same blob" "$HOST_B." B

refused "a host with no blob is refused" "unknown.example"
refused "the logical store ID is not a host" "trusted_server_config"
refused "another publisher's key with a suffix is not that publisher" "$HOST_A.evil.example"
fetch "unknown.example" /health
check "the health probe answers for any host" "$STATUS" "200"
if grep -qF "no app-config blob for host" "$VICEROY_LOG"; then
  ok "a refusal is logged with the host it refused"
else
  bad "a refusal is not logged"
fi
stop_viceroy

info "With '{host}_v2' each host reads the blob under its name and the suffix"
use_selector "{host}_v2"
start_viceroy suffixed
served_by "publisher A's host reads the blob under its name and the suffix" "$HOST_A" V2
refused "a host with no suffixed blob is refused" "$HOST_B"
refused "no host is served from the blob at the logical store ID" "unknown.example"
stop_viceroy

info "A malformed selector serves no page"
use_selector "{Host}"
start_viceroy malformed
fetch "$HOST_A"
check "a page route is answered 500" "$STATUS" "500"
no_page "a page route under a malformed selector"
fetch "$HOST_A" /health
check "the health probe still answers" "$STATUS" "200"
stop_viceroy

info "A selector in an edgezero_runtime_env store is not read"
use_selector "" edgezero_runtime_env "$OLD_SELECTOR_ENTRY={host}"
start_viceroy old-selector
served_by "a publisher's host reads the blob at the logical store ID" "$HOST_A" SHARED
stop_viceroy

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  echo "Viceroy log of the last run:" >&2
  tail -40 "$VICEROY_LOG" >&2
  exit 1
fi
