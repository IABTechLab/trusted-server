#!/usr/bin/env bash
#
# Local harness for the #1009 shared-template cache (C2).
#
# Runs Trusted Server under Viceroy against a stub origin and asserts the cache
# behaves. Everything it needs is generated into a temp directory; your
# `trusted-server.toml` is never read and your `fastly.toml` is restored on exit,
# including on failure or Ctrl-C.
#
# Usage:
#   ./scripts/c2-local-test.sh              # esi mode (shared template + edge assembly)
#   ./scripts/c2-local-test.sh inline       # today's shipped behaviour, as a control
#
# Spike-only. Remove with the spike.

set -euo pipefail

MODE="${1:-esi}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
ORIGIN_PORT="${ORIGIN_PORT:-9099}"
TS_PORT="${TS_PORT:-7788}"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"

# The stub's bid endpoint sleeps this long. It exists to make the auction
# observable in the timings: with an instant auction, buffered and streaming
# assembly are indistinguishable.
BID_DELAY="${BID_DELAY:-1.5}"

PASS=0
FAIL=0

info() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
ok() { printf '  \033[32mPASS\033[0m %s\n' "$*"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL + 1)); }

check() { # check <description> <actual> <expected>
  if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1 — got '$2', want '$3'"; fi
}

cleanup() {
  local status=$?
  [ -n "${VICEROY_PID:-}" ] && kill "$VICEROY_PID" 2>/dev/null || true
  [ -n "${ORIGIN_PID:-}" ] && kill "$ORIGIN_PID" 2>/dev/null || true
  # Restore fastly.toml unconditionally. `ts config push --local` edits it in
  # place, and it is a tracked file — leaving it modified would put a stub
  # config, and on a real deployment the operator's secrets, into `git status`.
  if [ -f "$WORK/fastly.toml.orig" ]; then
    cp "$WORK/fastly.toml.orig" "$REPO_ROOT/fastly.toml"
  fi
  rm -rf "$WORK"
  exit $status
}
trap cleanup EXIT INT TERM

command -v viceroy >/dev/null || {
  echo "viceroy not found. Install: cargo install viceroy --version 0.17.0 --locked" >&2
  exit 1
}

# A port already in use means requests would go to something else entirely — most
# likely a leftover run, whose warm cache and stale config would read as a result.
for port in "$ORIGIN_PORT" "$TS_PORT"; do
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "Port $port is already in use. Stop the process, or set" >&2
    echo "ORIGIN_PORT / TS_PORT to something free." >&2
    lsof -nP -iTCP:"$port" -sTCP:LISTEN >&2
    exit 1
  fi
done

info "Building (debug wasm + ts CLI)"
cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1 >/dev/null
cargo build -p trusted-server-cli --target "$HOST_TRIPLE" >/dev/null
WASM="$REPO_ROOT/target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm"
TS="$REPO_ROOT/target/$HOST_TRIPLE/debug/ts"

info "Starting stub origin on :$ORIGIN_PORT"
cat > "$WORK/origin.py" <<PYEOF
"""Stub publisher origin: one shareable page, plus a deliberately slow bid endpoint.

The page is as shareable as HTML gets — no Set-Cookie, a public Cache-Control, and a
Vary the cache key covers — so a bypass means a real bug rather than a fixture problem.
"""
import json, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGE = b"""<!doctype html>
<html><head><title>Stub article</title></head>
<body>
<h1>Stub article</h1>
<div id="ts-slot-header"></div>
<p>Body copy.</p>
</body></html>
"""

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, body, ctype, extra=()):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        for k, v in extra:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._send(PAGE, "text/html; charset=utf-8",
                   [("Cache-Control", "public, max-age=300"), ("Vary", "Accept-Encoding")])

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        time.sleep($BID_DELAY)
        self._send(json.dumps({"id": "stub", "seatbid": []}).encode(), "application/json")

    def log_message(self, fmt, *args):
        print("origin: " + fmt % args, flush=True)

ThreadingHTTPServer(("127.0.0.1", $ORIGIN_PORT), H).serve_forever()
PYEOF
python3 "$WORK/origin.py" > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
sleep 1

info "Generating stub config (mode: $MODE)"
python3 - "$REPO_ROOT/trusted-server.example.toml" "$WORK/app.toml" "$MODE" "$ORIGIN_PORT" <<'PYEOF'
import sys, re
src, out, mode, port = sys.argv[1:5]
s = open(src).read()

s = s.replace('origin_url = "https://origin.example.com"', f'origin_url = "http://127.0.0.1:{port}"', 1)
# The example config ships placeholders that validation rejects outright.
s = s.replace('password = "replace-with-admin-password-32-bytes"',
              'password = "local-harness-admin-password-not-a-real-one"', 1)
s = s.replace('proxy_secret = "change-me-proxy-secret"',
              'proxy_secret = "local-harness-proxy-secret-not-a-real-one"', 1)
s = re.sub(r'passphrase = "[^"]*"',
           'passphrase = "local-harness-ec-passphrase-not-a-real-one"', s, count=1)

# A real auction, pointed at the stub's slow endpoint, so the timings mean something.
s = s.replace('[integrations.prebid]\nenabled = false\nserver_url = "https://prebid.example.com/openrtb2/auction"',
              f'[integrations.prebid]\nenabled = true\nserver_url = "http://127.0.0.1:{port}/bid"\n'
              'external_bundle_url = "https://assets.example.com/prebid/trusted-prebid-stub.js"', 1)
s = s.replace('providers = []', 'providers = ["prebid"]', 1)
s = s.replace('\n[proxy]\n', '\n[proxy]\nallowed_domains = ["assets.example.com", "127.0.0.1"]\n', 1)
s = s.replace('[auction]\nenabled = false', '[auction]\nenabled = true', 1)
s = s.replace('auction_timeout_ms = 500', 'auction_timeout_ms = 3000', 1)
s = s.replace('timeout_ms = 2000', 'timeout_ms = 3000', 1)

# The spike keys go directly under the table header. The slot is a table of its own
# and must go at the end: inserted here it would swallow every scalar key that
# follows into `[[creative_opportunities.slot]]`.
scalars = f'''assembly_mode = "{mode}"
template_cache_vary = []
origin_is_cookie_independent = true'''
lines = s.split("\n")
lines.insert(lines.index("[creative_opportunities]") + 1, scalars)
lines.append('''
[[creative_opportunities.slot]]
id = "ts-slot-header"
div_id = "ts-slot-header"
page_patterns = ["/article"]
formats = [{ width = 728, height = 90 }]
''')
open(out, "w").write("\n".join(lines))
PYEOF

"$TS" config validate --app-config "$WORK/app.toml" >/dev/null

info "Seeding the config store (your fastly.toml is restored on exit)"
cp "$REPO_ROOT/fastly.toml" "$WORK/fastly.toml.orig"
(cd "$REPO_ROOT" && "$TS" config push --adapter fastly --local \
  --app-config "$WORK/app.toml" --no-diff --yes >/dev/null)
cp "$REPO_ROOT/fastly.toml" "$WORK/fastly.toml"
cp "$WORK/fastly.toml.orig" "$REPO_ROOT/fastly.toml"

info "Starting Trusted Server on :$TS_PORT"
# Deliberately not wrapped in a subshell: `$!` would then be the subshell's pid, so
# cleanup would kill the wrapper and orphan viceroy. The next run would fail to bind
# and answer from the stale server instead — with the previous mode's config and a warm
# cache, which looks like a test result rather than a mistake.
RUST_LOG=info viceroy serve -C "$WORK/fastly.toml" \
  --addr "127.0.0.1:$TS_PORT" "$WASM" > "$WORK/viceroy.log" 2>&1 &
VICEROY_PID=$!

for _ in $(seq 1 40); do
  grep -q "Listening on" "$WORK/viceroy.log" 2>/dev/null && break
  sleep 0.5
done
if ! grep -q "Listening on" "$WORK/viceroy.log" 2>/dev/null; then
  echo "Trusted Server failed to start. Last log lines:" >&2
  tail -20 "$WORK/viceroy.log" >&2
  exit 1
fi

req() { # req <output-file> [extra curl args...]
  local out="$1"; shift
  curl -s -o "$out" -w '%{time_starttransfer} %{time_total} %{http_code}' \
    -H "Host: ts.example.com" \
    -H "sec-fetch-dest: document" -H "sec-fetch-mode: navigate" \
    "$@" "http://127.0.0.1:$TS_PORT/article"
}

origin_gets() { grep -c "GET /article" "$WORK/origin.log" || true; }

info "Running assertions (mode: $MODE)"
BEFORE=$(origin_gets)
R1=$(req "$WORK/r1.html")
R2=$(req "$WORK/r2.html")
AFTER=$(origin_gets)
FETCHES=$((AFTER - BEFORE))

read -r TTFB1 TOTAL1 CODE1 <<< "$R1"
read -r TTFB2 TOTAL2 CODE2 <<< "$R2"

check "first request returns 200" "$CODE1" "200"
check "second request returns 200" "$CODE2" "200"

if [ "$MODE" = "inline" ]; then
  check "inline fetches the origin every time" "$FETCHES" "2"
  check "inline writes no shared template" \
    "$(grep -c 'c2_template_cache stored' "$WORK/viceroy.log" || true)" "0"
else
  check "second request is served from cache" "$FETCHES" "1"
  check "no unresolved esi:include reaches the browser" \
    "$(grep -c 'esi:include' "$WORK/r2.html" || true)" "0"
  check "a bids script is present" \
    "$(grep -c 'window.tsjs' "$WORK/r2.html" || true)" "1"

  HDRS=$(curl -s -D- -o /dev/null -H "Host: ts.example.com" \
    -H "sec-fetch-dest: document" -H "sec-fetch-mode: navigate" \
    "http://127.0.0.1:$TS_PORT/article")
  check "cache hit is not shared-cacheable" \
    "$(echo "$HDRS" | grep -ci 'cache-control: private, no-store' || true)" "1"

  POSTS_BEFORE=$(grep -c "POST /article" "$WORK/origin.log" || true)
  curl -s -o /dev/null -X POST -d 'x=1' -H "Host: ts.example.com" \
    "http://127.0.0.1:$TS_PORT/article"
  check "a POST still reaches the origin" \
    "$(( $(grep -c "POST /article" "$WORK/origin.log" || true) - POSTS_BEFORE ))" "1"
fi

info "Timing (bid endpoint delays $BID_DELAY s)"
printf '  request 1  ttfb=%ss  total=%ss\n' "$TTFB1" "$TOTAL1"
printf '  request 2  ttfb=%ss  total=%ss\n' "$TTFB2" "$TOTAL2"
cat <<EOF

  Read it like this: TTFB ~= total means the response is buffered — the reader
  waits for the auction before receiving any byte. Streaming assembly is the
  change that should pull TTFB away from total by roughly the bid delay. That
  gap is the pass/fail for that work; today there is none.
EOF

info "Result"
printf '  %d passed, %d failed\n\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
