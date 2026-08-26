#!/usr/bin/env bash
#
# Local harness for the #1009 shared template cache.
#
# Runs Trusted Server under Viceroy against a stub origin and asserts the cache
# behaves. Everything it needs is generated into a temp directory; your
# `trusted-server.toml` is never read and your tracked `fastly.toml` is never
# modified, including while the harness is running.
#
# Usage:
#   ./scripts/template-cache-local-test.sh              # esi mode (shared template + edge assembly)
#   ./scripts/template-cache-local-test.sh inline       # today's shipped behaviour, as a control

set -euo pipefail

MODE="${1:-esi}"
case "$MODE" in
  inline | esi) ;;
  *)
    echo "Unknown mode '$MODE'. Use one of: inline, esi." >&2
    exit 1
    ;;
esac
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
ORIGIN_PORT="${ORIGIN_PORT:-9099}"
BID_PORT="${BID_PORT:-9100}"
TS_PORT="${TS_PORT:-7788}"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"

# The stub's bid endpoint sleeps this long. It exists to make the auction
# observable in the timings: with an instant auction, buffered and streaming
# assembly are indistinguishable.
BID_DELAY="${BID_DELAY:-1.5}"
REQUEST_TIMEOUT_SECONDS="${REQUEST_TIMEOUT_SECONDS:-30}"

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
  if [ -n "${VICEROY_PID:-}" ]; then
    kill "$VICEROY_PID" 2>/dev/null || true
    wait "$VICEROY_PID" 2>/dev/null || true
  fi
  if [ -n "${ORIGIN_PID:-}" ]; then
    # macOS may launch the framework Python process as a child of the shim.
    pkill -TERM -P "$ORIGIN_PID" 2>/dev/null || true
    kill "$ORIGIN_PID" 2>/dev/null || true
    wait "$ORIGIN_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
  exit $status
}
trap cleanup EXIT INT TERM

command -v viceroy >/dev/null || {
  echo "viceroy not found. Install: cargo install viceroy --version 0.17.0 --locked" >&2
  exit 1
}
command -v node >/dev/null || {
  echo "node not found. The harness executes the real GPT bundle to verify slot setup." >&2
  exit 1
}
command -v openssl >/dev/null || {
  echo "openssl not found. The harness needs it for the local HTTPS bid endpoint." >&2
  exit 1
}

# A port already in use means requests would go to something else entirely — most
# likely a leftover run, whose warm cache and stale config would read as a result.
for port in "$ORIGIN_PORT" "$BID_PORT" "$TS_PORT"; do
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "Port $port is already in use. Stop the process, or set" >&2
    echo "ORIGIN_PORT / BID_PORT / TS_PORT to something free." >&2
    lsof -nP -iTCP:"$port" -sTCP:LISTEN >&2
    exit 1
  fi
done

info "Building (debug wasm + ts CLI)"
cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1 >/dev/null
cargo build -p trusted-server-cli --target "$HOST_TRIPLE" >/dev/null
WASM="$REPO_ROOT/target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm"
TS="$REPO_ROOT/target/$HOST_TRIPLE/debug/ts"

info "Generating a local CA and HTTPS bid certificate"
openssl req -x509 -newkey rsa:2048 -sha256 -days 1 -nodes \
  -subj "/CN=Trusted Server local harness CA" \
  -keyout "$WORK/ca-key.pem" -out "$WORK/ca-cert.pem" >/dev/null 2>&1
openssl req -newkey rsa:2048 -sha256 -nodes -subj "/CN=localhost" \
  -keyout "$WORK/server-key.pem" -out "$WORK/server.csr" >/dev/null 2>&1
cat > "$WORK/server.ext" <<'EOF'
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1
EOF
openssl x509 -req -sha256 -days 1 -in "$WORK/server.csr" \
  -CA "$WORK/ca-cert.pem" -CAkey "$WORK/ca-key.pem" -CAcreateserial \
  -extfile "$WORK/server.ext" -out "$WORK/server-cert.pem" >/dev/null 2>&1

info "Starting stub origin on :$ORIGIN_PORT and HTTPS bidder on :$BID_PORT"
cat > "$WORK/origin.py" <<PYEOF
"""Stub publisher origin and deliberately slow HTTPS bid endpoint.

The page uses the real Fastly policy that exposed #1009's response-side bypass: no
Set-Cookie, a public Cache-Control, Surrogate-Control with stale windows, and a Vary the
cache key covers. A bypass therefore means a real bug rather than a fixture problem.

The bid endpoint returns a real winning bid. It used to return an empty seatbid, which made
every assertion below measure a page with no ads on it. That is how a seam that silently
discarded every non-empty bid map passed this harness for weeks.
"""
import gzip, json, ssl, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The impid must be the configured slot id: that is what the auction request sends and
# what the winning-bid map is keyed on. A mismatch yields no winner and no injected bid.
# The price is bucketed by the dense granularity to exactly "4.25", which is the value
# the assertions look for in the served page.
BID_RESPONSE = {
    "id": "stub",
    "seatbid": [
        {
            "seat": "mocktioneer",
            "bid": [
                {
                    "id": "stub-bid-1",
                    "impid": "ts-slot-header",
                    "adid": "stub-creative-1",
                    "price": 4.25,
                    "adm": "<div>stub creative</div>",
                    "w": 728,
                    "h": 90,
                }
            ],
        }
    ],
}

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
        print(f"origin: received GET {self.path}", flush=True)
        # Compresses when asked, because a real origin does and because a plaintext-only
        # stub hid a bug that broke the feature end to end: a gzip template has no
        # findable seam marker, and splicing plaintext bids into a gzip stream gives the
        # browser ERR_CONTENT_DECODING_FAILED.
        base = [
            ("Cache-Control", "public, max-age=60"),
            ("Surrogate-Control", "max-age=1200, stale-while-revalidate=21600, stale-if-error=604800"),
            ("Vary", "Accept-Encoding"),
        ]
        if "gzip" in (self.headers.get("Accept-Encoding") or ""):
            print("origin: served COMPRESSED", flush=True)
            self._send(gzip.compress(PAGE), "text/html; charset=utf-8",
                       base + [("Content-Encoding", "gzip")])
        else:
            print("origin: served PLAINTEXT", flush=True)
            self._send(PAGE, "text/html; charset=utf-8", base)

    def do_POST(self):
        print(f"origin: received POST {self.path}", flush=True)
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        time.sleep($BID_DELAY)
        self._send(json.dumps(BID_RESPONSE).encode(), "application/json")

    def log_message(self, fmt, *args):
        print("origin: " + fmt % args, flush=True)


class BidH(H):
    def do_GET(self):
        print(f"bidder: received health check {self.path}", flush=True)
        self._send(b"ok", "text/plain")


origin = ThreadingHTTPServer(("127.0.0.1", $ORIGIN_PORT), H)
threading.Thread(target=origin.serve_forever, daemon=True).start()

bidder = ThreadingHTTPServer(("127.0.0.1", $BID_PORT), BidH)
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain("$WORK/server-cert.pem", "$WORK/server-key.pem")
bidder.socket = tls.wrap_socket(bidder.socket, server_side=True)
bidder.serve_forever()
PYEOF
python3 "$WORK/origin.py" > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
sleep 1

info "Generating stub config (mode: $MODE)"
python3 - "$REPO_ROOT/trusted-server.example.toml" "$WORK/app.toml" "$MODE" \
  "$ORIGIN_PORT" "$BID_PORT" <<'PYEOF'
import sys

src, out, mode, origin_port, bid_port = sys.argv[1:6]
s = open(src).read()


def replace_once(content, old, new, description):
    if content.count(old) != 1:
        raise SystemExit(f"expected one {description} replacement target")
    return content.replace(old, new, 1)


s = replace_once(
    s,
    'origin_url = "https://origin.example.com"',
    f'origin_url = "http://127.0.0.1:{origin_port}"',
    "publisher origin",
)
# A real auction points at the slow HTTPS stub so the timings mean something.
s = replace_once(
    s,
    '[integrations.prebid]\nenabled = false',
    '[integrations.prebid]\nenabled = true\n'
    'external_bundle_url = "https://assets.example.com/prebid/trusted-prebid-stub.js"',
    "Prebid integration",
)
s = replace_once(
    s,
    'endpoint = "https://prebid.example.com/openrtb2/auction"',
    f'endpoint = "https://localhost:{bid_port}/bid"\ntimeout_ms = 5000',
    "Prebid provider endpoint",
)
s = replace_once(
    s,
    '\n[proxy]\n',
    '\n[proxy]\nallowed_domains = ["assets.example.com", "127.0.0.1"]\n',
    "proxy table",
)
s = replace_once(
    s,
    '[auction]\n# Keep disabled until provider endpoints, routes, and profile values below are\n'
    '# replaced with deployment-specific settings.\nenabled = false',
    '[auction]\n# Keep disabled until provider endpoints, routes, and profile values below are\n'
    '# replaced with deployment-specific settings.\nenabled = true',
    "auction enablement",
)
s = replace_once(
    s,
    'sanitize_creatives = false\ntimeout_ms = 2000',
    'sanitize_creatives = false\ntimeout_ms = 10000',
    "auction timeout",
)
s = replace_once(
    s,
    'auction_timeout_ms = 500  # override via TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__AUCTION_TIMEOUT_MS',
    'auction_timeout_ms = 10000  # override via TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__AUCTION_TIMEOUT_MS',
    "creative opportunity auction timeout",
)

# The template-cache keys go directly under the table header. The slot is a table of its own
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

info "Seeding an isolated config store (tracked fastly.toml remains untouched)"
# `config push --local` edits the adapter manifest. Give it a complete temporary
# project instead of editing the tracked manifest and trying to restore it afterward:
# a SIGKILL, machine crash, or failed restore could otherwise leave real serialized
# credentials in a tracked file. The crates symlink keeps manifest path validation
# pointed at this checkout without copying the workspace.
cp "$REPO_ROOT/edgezero.toml" "$WORK/edgezero.toml"
cp "$REPO_ROOT/fastly.toml" "$WORK/fastly.toml"

# The application registers provider backends dynamically. Pre-register the exact
# deterministic name so Viceroy reuses a local backend that trusts the temporary CA.
python3 - "$WORK/fastly.toml" "$WORK/ca-cert.pem" "$BID_PORT" <<'PYEOF'
import hashlib
import json
import sys

manifest, ca_certificate, port = sys.argv[1:4]
provider_id = "pbs-main"
timeout_ms = "5000"


def field(value):
    return f"{len(value)}:{value}"


canonical = "".join([
    field("https"),
    field("localhost"),
    field(port),
    field("1"),
    "n",
    "s",
    field(provider_id),
    field(timeout_ms),
    field(timeout_ms),
])
digest = hashlib.sha256(canonical.encode()).hexdigest()[:32]
readable = f"https_localhost_{port}_p_{provider_id}_fb{timeout_ms}_bb{timeout_ms}"
backend_name = f"backend_{readable}_{digest}"
backend = f'''[local_server.backends.{backend_name}]
url = "https://localhost:{port}"
cert_host = "localhost"
ca_certificate.file = {json.dumps(ca_certificate)}
'''

content = open(manifest).read()
marker = "[local_server.backends]\n\n"
if content.count(marker) != 1:
    raise SystemExit("expected one local backend insertion target")
content = content.replace(marker, f"{marker}{backend}\n", 1)
open(manifest, "w").write(content)
PYEOF

python3 - "$WORK/fastly.toml" <<'PYEOF'
import sys

with open(sys.argv[1], "a") as manifest:
    manifest.write('''
[[local_server.secret_stores.ts_secrets]]
key = "publisher_proxy_secret"
data = "fictional-local-publisher-proxy-secret-value"

[[local_server.secret_stores.ts_secrets]]
key = "ec_passphrase"
data = "fictional-local-ec-passphrase-secret-value"

[[local_server.secret_stores.ts_secrets]]
key = "handler_password"
data = "fictional-local-handler-password-secret-value"
''')
PYEOF

ln -s "$REPO_ROOT/crates" "$WORK/crates"
(cd "$WORK" && "$TS" config push --adapter fastly --local \
  --manifest "$WORK/edgezero.toml" --app-config "$WORK/app.toml" \
  --no-diff --yes >/dev/null)

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
  curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" -D "$out.headers" -o "$out" \
    -w '%{time_starttransfer} %{time_total} %{http_code}' \
    -H "Host: ts.example.com" \
    -H "Accept-Encoding: gzip" \
    -H "sec-fetch-dest: document" -H "sec-fetch-mode: navigate" \
    "$@" "http://127.0.0.1:$TS_PORT/article"
}

origin_gets() { grep -cF "origin: received GET /article" "$WORK/origin.log" || true; }

info "Running assertions (mode: $MODE)"
BEFORE=$(origin_gets)
R1=$(req "$WORK/r1.html")
R2=$(req "$WORK/r2.html")

if [ -s "$WORK/r1.html" ]; then
  ok "first response has a body"
else
  bad "first response body is empty"
fi
if [ -s "$WORK/r2.html" ]; then
  ok "second response has a body"
else
  bad "second response body is empty"
fi

# Content assertions must never run against compressed bytes. `inline` responses stay
# gzipped end to end — only the shared path decodes, because its seam split is textual —
# and `grep` over a gzip stream matches nothing, which reads as a pass for every
# "must not contain" check and as a silent failure for every "must contain" one.
# Decode a copy and assert against that, in both modes.
SERVED="$WORK/r2.served.html"
if ! gzip -dc "$WORK/r2.html" > "$SERVED" 2>/dev/null; then
  cp "$WORK/r2.html" "$SERVED"
fi
AFTER=$(origin_gets)
FETCHES=$((AFTER - BEFORE))

read -r TTFB1 TOTAL1 CODE1 <<< "$R1"
read -r TTFB2 TOTAL2 CODE2 <<< "$R2"

for value in "$TTFB1" "$TOTAL1" "$CODE1" "$TTFB2" "$TOTAL2" "$CODE2"; do
  if ! [[ "$value" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    bad "curl returned a non-numeric timing/status field: '$value'"
  fi
done

check "first request returns 200" "$CODE1" "200"
check "second request returns 200" "$CODE2" "200"

# The bid the stub origin returns, bucketed and then escaped the way the seam escapes
# it. Asserted in both modes: a shared-mode failure that inline shares would otherwise
# read as "the fixture never bids" rather than "the seam drops bids".
WINNING_BID='\"hb_pb\":\"4.25\"'
# Must stay in step with `AD_ASSEMBLY_SEAM` in publisher.rs. The template cache stores
# this inert comment. The cold response turns it into a synthetic ESI include only in a private
# working copy; the warm response splits these bytes directly.
SEAM_MARKER='<!--ts-ad-seam-->'

template_cache_state() {
  awk 'tolower($1) == "x-ts-template-cache:" { gsub(/\r/, "", $2); print $2 }' "$1" | tail -1
}

assembly_state() {
  awk 'tolower($1) == "x-ts-assembly:" { gsub(/\r/, "", $2); print $2 }' "$1" | tail -1
}

# Shared by the ESI assertions below.
check_hit_is_private() {
  local hdrs
  hdrs=$(curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" -D- -o /dev/null \
    -H "Host: ts.example.com" \
    -H "Accept-Encoding: gzip" \
    -H "sec-fetch-dest: document" -H "sec-fetch-mode: navigate" \
    "http://127.0.0.1:$TS_PORT/article")
  check "cache hit is not shared-cacheable" \
    "$(echo "$hdrs" | grep -ci 'cache-control: no-store, private' || true)" "1"
}

check_post_reaches_origin() {
  local before
  before=$(grep -cF "origin: received POST /article" "$WORK/origin.log" || true)
  curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" -o /dev/null \
    -X POST -d 'x=1' -H "Host: ts.example.com" \
    -H "Accept-Encoding: gzip" \
    "http://127.0.0.1:$TS_PORT/article"
  check "a POST still reaches the origin" \
    "$(( $(grep -cF "origin: received POST /article" "$WORK/origin.log" || true) - before ))" "1"
}

if [ "$MODE" = "inline" ]; then
  check "inline fetches the origin every time" "$FETCHES" "2"
  check "inline writes no shared template" \
    "$(grep -c 'template_cache stored' "$WORK/viceroy.log" || true)" "0"
  check "inline delivers the winning bid" \
    "$(grep -cF "$WINNING_BID" "$SERVED" || true)" "1"
else
  check "second request is served from cache" "$FETCHES" "1"
  check "cold request reports a stored miss" "$(template_cache_state "$WORK/r1.html.headers")" "miss-stored"
  check "warm request reports a cache hit" "$(template_cache_state "$WORK/r2.html.headers")" "hit"
  check "cold response uses the repaired ESI parser" \
    "$(assembly_state "$WORK/r1.html.headers")" "esi-parser"
  check "warm response keeps the streaming byte seam" \
    "$(assembly_state "$WORK/r2.html.headers")" "byte-seam"
  check "no unresolved seam marker reaches the browser" \
    "$(grep -cF "$SEAM_MARKER" "$SERVED" || true)" "0"
  check "a bids script is present" \
    "$(grep -c 'window.tsjs' "$SERVED" || true)" "1"
  # `window.tsjs` alone passes while initial ads are dead: shared modes suppress the head
  # slot script, so if the seam does not carry slots, `adSlots` stays `[]` and `adInit`
  # defines nothing. This harness passed green through exactly that bug.
  # The slots ride the scheduler call (`s(b,a)`) rather than a bare assignment, so the
  # navigation-generation guard covers them; `var a=JSON.parse(...)` is where they land.
  check "the seam carries slot definitions, not just bids" \
    "$(grep -c 'var a=JSON.parse' "$SERVED" || true)" "1"
  check "the slot definitions reach the guarded scheduler" \
    "$(grep -cF 's(b,a)' "$SERVED" || true)" "1"
  check "the slot definitions are populated, not an empty array" \
    "$(grep -c 'var a=JSON.parse("\[\]")' "$SERVED" || true)" "0"
  # The assertion the harness was missing entirely. `window.tsjs` and populated slots
  # both pass on a page whose bids are `{}` — which is what shared modes served, on
  # every request, for as long as this file has existed.
  check "the seam carries a real bid, not an empty map" \
    "$(grep -cF 'var b=JSON.parse("{}")' "$SERVED" || true)" "0"
  check "the winning bid's bucketed price reaches the reader" \
    "$(grep -cF "$WINNING_BID" "$SERVED" || true)" "1"

  GPT_BUNDLE=""
  while IFS= read -r -d '' candidate; do
    if [ -z "$GPT_BUNDLE" ] || [ "$candidate" -nt "$GPT_BUNDLE" ]; then
      GPT_BUNDLE="$candidate"
    fi
  done < <(find "$REPO_ROOT/target/wasm32-wasip1/debug/build" \
    -path '*/out/tsjs-gpt.js' -type f -print0)
  if [ -z "$GPT_BUNDLE" ] || [ ! -s "$GPT_BUNDLE" ]; then
    bad "the generated GPT module cannot be found"
  else
    cat > "$WORK/verify-seam.mjs" <<'NODEEOF'
import fs from "node:fs";
import vm from "node:vm";

const [documentPath, gptPath] = process.argv.slice(2);
const html = fs.readFileSync(documentPath, "utf8");
const gpt = fs.readFileSync(gptPath, "utf8");
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/gi)].map((match) => match[1]);
const seam = scripts.find((script) => script.includes('var b=JSON.parse("'));
if (!seam) throw new Error("served document has no executable seam payload");

const element = {
  id: "ts-slot-header",
  parentElement: null,
  checkVisibility: () => true,
  getBoundingClientRect: () => ({ width: 728, height: 90 }),
  querySelectorAll: () => [],
};
const defined = [];
const gptSlots = [];
const pubads = {
  addEventListener() {},
  disableInitialLoad() {},
  enableSingleRequest() {},
  getSlots: () => gptSlots,
  refresh() {},
};
const googletag = {
  cmd: { push(...callbacks) { callbacks.forEach((callback) => callback()); return callbacks.length; } },
  defineSlot(unit, formats, divId) {
    defined.push({ unit, formats, divId });
    const slot = {
      addService() { return slot; },
      clearTargeting() {},
      getSlotElementId: () => divId,
      setTargeting() { return slot; },
    };
    gptSlots.push(slot);
    return slot;
  },
  destroySlots: () => true,
  display() {},
  enableServices() {},
  pubads: () => pubads,
};
const listeners = new Map();
const windowObject = {
  addEventListener(type, callback) { listeners.set(type, callback); },
  getComputedStyle: () => ({ display: "block", visibility: "visible" }),
  googletag,
  history: { pushState() {}, replaceState() {} },
  location: {
    host: "ts.example.com",
    href: "https://ts.example.com/article",
    origin: "https://ts.example.com",
    pathname: "/article",
    protocol: "https:",
  },
  requestAnimationFrame(callback) { callback(); return 1; },
  tsjs: { navGeneration: 0 },
};
const documentObject = {
  documentElement: {},
  getElementById: (id) => id === element.id ? element : null,
  querySelectorAll: () => [],
  readyState: "complete",
  visibilityState: "visible",
};
globalThis.window = windowObject;
globalThis.document = documentObject;
globalThis.history = windowObject.history;
globalThis.location = windowObject.location;
globalThis.requestAnimationFrame = windowObject.requestAnimationFrame;

vm.runInThisContext(gpt, { filename: gptPath });
vm.runInThisContext(seam, { filename: documentPath });

const slots = windowObject.tsjs.adSlots ?? [];
const bids = windowObject.tsjs.bids ?? {};
if (slots.length !== 1 || slots[0].id !== "ts-slot-header") {
  throw new Error(`scheduler received invalid slots: ${JSON.stringify(slots)}`);
}
if (!bids["ts-slot-header"] || bids["ts-slot-header"].hb_pb !== "4.25") {
  throw new Error(`scheduler received no winning bid: ${JSON.stringify(bids)}`);
}
if (defined.length !== 1 || defined[0].divId !== "ts-slot-header") {
  throw new Error(`GPT defineSlot contract failed: ${JSON.stringify(defined)}`);
}
console.log(`slots=${slots.length} bids=${Object.keys(bids).length} defined=${defined.length}`);
NODEEOF
    GPT_RESULT=$(node "$WORK/verify-seam.mjs" "$SERVED" "$GPT_BUNDLE" 2>&1) || {
      bad "the served seam failed the real GPT module contract: $GPT_RESULT"
      GPT_RESULT=""
    }
    [ -z "$GPT_RESULT" ] || check \
      "the real scheduler defines the populated GPT slot" "$GPT_RESULT" \
      "slots=1 bids=1 defined=1"
  fi

  check_hit_is_private
  check_post_reaches_origin
fi

if [ "$MODE" = "esi" ]; then
  info "Where the marker actually lives"
  echo "  The cached template (the shared copy — has a hole where bids go):"
  grep -oE "template_cache stored [0-9]+ bytes \(seam marker present: [a-z]+\)" \
    "$WORK/viceroy.log" | sort -u | sed 's/^/    /'
  echo
  echo "  What the reader receives (hole filled, no marker):"
  printf '    %d seam marker(s), %d window.tsjs\n' \
    "$(grep -cF "$SEAM_MARKER" "$SERVED" || true)" \
    "$(grep -c 'window\.tsjs' "$SERVED" || true)"
  cat <<'EOF'

  The marker is never visible in page source. It remains inert in the template cache. A cold miss
  converts it to one synthetic ESI include in a private working copy; a warm hit
  byte-splits it directly. Both replace it before sending the response.
  `seam marker present: true` above proves the stored copy is reader-agnostic.
EOF
fi

cat > "$WORK/probe.py" <<'PROBEEOF'
"""Measures time to first *body* byte, which curl's time_starttransfer does not.

For a streaming response, headers commit long before any body byte, so
time_starttransfer reports header-commit time and a stream that stalls before its
first chunk looks identical to one that does not.
"""
import socket, sys, time

host, port, path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
timeout = float(sys.argv[4])

req = (
    f"GET {path} HTTP/1.1\r\nHost: ts.example.com\r\n"
    "sec-fetch-dest: document\r\nsec-fetch-mode: navigate\r\n"
    "accept-encoding: gzip\r\n"
    "Connection: close\r\n\r\n"
).encode()

s = socket.create_connection((host, port), timeout=timeout)
s.settimeout(timeout)
t0 = time.time()
s.sendall(req)

buf = b""
t_headers = t_body = None
total = 0
while True:
    chunk = s.recv(65536)
    if not chunk:
        break
    if t_headers is None:
        t_headers = time.time()
    buf += chunk
    total += len(chunk)
    # First byte past the header terminator is the first body byte.
    if t_body is None and b"\r\n\r\n" in buf:
        head_end = buf.index(b"\r\n\r\n") + 4
        if len(buf) > head_end:
            t_body = time.time()
t_end = time.time()
s.close()

def ms(t):
    return "n/a" if t is None else f"{(t - t0) * 1000:.0f}ms"

print(f"headers={ms(t_headers)}  first_body_byte={ms(t_body)}  complete={ms(t_end)}  bytes={total}")
PROBEEOF

info "Delivery timing (bid endpoint delays $BID_DELAY s)"
cat <<'EOF'
  Measured with a socket probe, not curl. `time_starttransfer` reports the first byte
  of the *response*, which for a streaming response is the headers — committed long
  before any body byte. A stream that stalls before its first chunk looks identical to
  one that does not.
EOF
echo
probe_body_ms() {
  python3 "$WORK/probe.py" 127.0.0.1 "$TS_PORT" /article "$REQUEST_TIMEOUT_SECONDS"
}
echo "  request A: $(probe_body_ms)"
B_LINE="$(probe_body_ms)"
echo "  request B: $B_LINE"
echo

FIRST_BODY=$(echo "$B_LINE" | sed -n 's/.*first_body_byte=\([0-9]*\)ms.*/\1/p')
COMPLETE=$(echo "$B_LINE" | sed -n 's/.*complete=\([0-9]*\)ms.*/\1/p')

if ! [[ "$FIRST_BODY" =~ ^[0-9]+$ && "$COMPLETE" =~ ^[0-9]+$ ]]; then
  bad "socket probe did not return numeric body timings: '$B_LINE'"
else
  if [ "$MODE" = "inline" ]; then
    check "inline delivers the article before the auction resolves" \
      "$(awk -v f="$FIRST_BODY" -v c="$COMPLETE" 'BEGIN { print (f < c / 3) ? "yes" : "no" }')" \
      "yes"
  else
    # The property the unit tests cannot reach: in-process there is no bid provider, so
    # there is no auction to wait on and reordering the stream is unobservable. Here the
    # bid endpoint really sleeps, so the first body byte either beats it or does not.
    check "cache hit streams: the article is delivered before the auction resolves" \
      "$(awk -v f="$FIRST_BODY" -v c="$COMPLETE" 'BEGIN { print (f < c / 3) ? "yes" : "no" }')" \
      "yes"
  fi
  printf '    first body byte %sms, complete %sms\n\n' "$FIRST_BODY" "$COMPLETE"
fi

if [ "$MODE" != "inline" ]; then
  # Guards a regression where assembly rewrote a reader's accepted gzip origin request
  # to identity, making the origin send ~674KB where it would have sent ~100KB. The
  # cache still stores identity; that does not require changing what this reader accepts.
  check "the origin fetch stays compressed" \
    "$(grep -c 'served PLAINTEXT' "$WORK/origin.log" || true)" "0"
fi

info "Result"
printf '  %d passed, %d failed\n\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
