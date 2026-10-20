#!/usr/bin/env bash
#
# Local harness for attestation.
#
# Builds the Fastly adapter with a key schedule made for this run, runs it
# under Viceroy against a stub origin, and checks the evidence the way a
# verifier would: with the public key alone, over the context, a line feed
# and the payload. Everything it needs is generated into a temp directory;
# your `trusted-server.toml` is never read and your tracked `fastly.toml` is
# never modified. The keys are made here, used here and deleted with the
# directory.
#
# The build is given `TRUSTED_SERVER_ATTESTATION_KEYS`, so core is rebuilt
# with the schedule in it, and rebuilt again by the next build that is not
# given one. Run this after the other harnesses.
#
# Usage:
#   ./scripts/attestation-local-test.sh
#
# Environment:
#   TS_BIN         a built `ts`, in place of building one
#   CARGO_TARGET_DIR  where cargo builds, when it is not `target`
#   ORIGIN_PORT    the stub origin's port
#   TS_PORT        the port Viceroy listens on

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
ORIGIN_PORT="${ORIGIN_PORT:-9299}"
TS_PORT="${TS_PORT:-7899}"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
PUBLISHER="publisher.example"
CONTEXT="harness-attestation:v1"
# 9 September 2001 at 01:46:40 UTC, so the build time the evidence reports
# is known before the build is made.
BUILD_EPOCH=1000000000
BUILD_TIME="2001-09-09T01:46:40Z"
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

cleanup() {
  local status=$?
  if [ -n "$VICEROY_PID" ]; then
    kill "$VICEROY_PID" 2>/dev/null || true
    wait "$VICEROY_PID" 2>/dev/null || true
  fi
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

for tool in viceroy curl openssl; do
  command -v "$tool" >/dev/null || {
    echo "$tool not found. Viceroy installs with: cargo install viceroy --version 0.17.0 --locked" >&2
    exit 1
  }
done
PYTHON="$(command -v python3 || command -v python || true)"
[ -n "$PYTHON" ] || {
  echo "python3 not found. The harness reads the evidence with it." >&2
  exit 1
}

for port in "$ORIGIN_PORT" "$TS_PORT"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
    echo "Port $port is already in use. Stop the process, or set" >&2
    echo "ORIGIN_PORT / TS_PORT to something free." >&2
    exit 1
  fi
done

info "Making two signing keys and their schedule"
# The first key is in force from the epoch. The second starts in 2100, so
# the first signs everything this run asks for.
for name in first next; do
  openssl ecparam -name prime256v1 -genkey -noout -outform DER -out "$WORK/$name.der" 2>/dev/null
  openssl ec -inform DER -in "$WORK/$name.der" -pubout -out "$WORK/$name.pem" 2>/dev/null
done
printf 'harness-first\t0\t%s\nharness-next\t4102444800\t%s\n' \
  "$(openssl base64 -A -in "$WORK/first.der")" \
  "$(openssl base64 -A -in "$WORK/next.der")" > "$WORK/schedule.tsv"

info "Building (debug wasm with the schedule compiled in, and the ts CLI)"
TRUSTED_SERVER_ATTESTATION_KEYS="$WORK/schedule.tsv" \
  TRUSTED_SERVER_COMMIT="harness-commit" \
  TRUSTED_SERVER_BUILD_RUN="harness-run" \
  TRUSTED_SERVER_BUILD_TAG="harness-tag" \
  SOURCE_DATE_EPOCH="$BUILD_EPOCH" \
  cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1 >/dev/null
# Copied aside, because the next build that is given no schedule writes the
# same path.
cp "$TARGET_DIR/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm" "$WORK/app.wasm"
WASM="$WORK/app.wasm"
TS="${TS_BIN:-}"
if [ -z "$TS" ]; then
  cargo build -p trusted-server-cli --target "$HOST_TRIPLE" >/dev/null
  TS="$TARGET_DIR/$HOST_TRIPLE/debug/ts"
fi

info "Starting a stub origin on :$ORIGIN_PORT"
mkdir -p "$WORK/origin"
printf '<!doctype html><html><body>ORIGIN_SENTINEL</body></html>\n' > "$WORK/origin/index.html"
"$PYTHON" -m http.server "$ORIGIN_PORT" --bind 127.0.0.1 --directory "$WORK/origin" \
  > "$WORK/origin.log" 2>&1 &
ORIGIN_PID=$!
for _ in $(seq 1 40); do
  curl -s -o /dev/null "http://127.0.0.1:$ORIGIN_PORT/" && break
  sleep 0.25
done

info "Seeding an isolated config store (tracked fastly.toml remains untouched)"
cp "$REPO_ROOT/edgezero.toml" "$WORK/edgezero.toml"
cp "$REPO_ROOT/fastly.toml" "$WORK/fastly.toml"
ln -s "$REPO_ROOT/crates" "$WORK/crates"
cat >> "$WORK/fastly.toml" <<'SECRETSEOF'

[[local_server.secret_stores.trusted_server_secrets]]
key = "publisher_proxy_secret"
data = "fictional-local-proxy-secret-for-this-harness"

[[local_server.secret_stores.trusted_server_secrets]]
key = "ec_passphrase"
data = "fictional-local-ec-passphrase-secret-value"
SECRETSEOF

"$TS" config init --app-config "$WORK/app.toml" >/dev/null
cat >> "$WORK/app.toml" <<TOMLEOF

[attestation]
operator = "Harness Operator"
context = "$CONTEXT"
verify_url = "https://verifier.example/verify?host=$PUBLISHER"
TOMLEOF
(
  cd "$WORK"
  TRUSTED_SERVER__PUBLISHER__DOMAIN="$PUBLISHER" \
    TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=".$PUBLISHER" \
    TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT" \
    "$TS" config push --adapter fastly --local \
    --manifest "$WORK/edgezero.toml" --app-config "$WORK/app.toml" \
    --no-diff --yes >/dev/null
)

info "Starting Trusted Server on :$TS_PORT"
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

REQUESTS=0
fetch() { # fetch <path and query> [extra curl args...], sets STATUS, BODY and HEADERS
  local target="$1"
  shift
  REQUESTS=$((REQUESTS + 1))
  BODY="$WORK/response-$REQUESTS.body"
  HEADERS="$WORK/response-$REQUESTS.headers"
  STATUS="$(curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" -D "$HEADERS" -o "$BODY" \
    -w '%{http_code}' -H "Host: Publisher.Example:8443" "$@" \
    "http://127.0.0.1:$TS_PORT$target" || true)"
}

header() { # header <name>, from the last response
  tr -d '\r' < "$HEADERS" | awk -v name="$1:" 'tolower($1) == name { print $2 }'
}

# Reads an envelope the way a verifier does. Writes the bytes the signature
# covers and the signature in the form openssl takes, and prints one field of
# the evidence, read from the payload and never from the readable copy.
cat > "$WORK/evidence.py" <<'PYEOF'
import base64
import json
import sys


def b64url(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def der_integer(raw):
    raw = raw.lstrip(b"\x00") or b"\x00"
    if raw[0] & 0x80:
        raw = b"\x00" + raw
    return b"\x02" + bytes([len(raw)]) + raw


envelope = json.load(open(sys.argv[1], encoding="utf-8"))
command = sys.argv[2]
payload = b64url(envelope["payload"])
if command == "field":
    value = json.loads(payload)
    for part in sys.argv[3].split("."):
        value = value.get(part, "") if isinstance(value, dict) else ""
    print(value)
elif command == "envelope":
    value = envelope
    for part in sys.argv[3].split("."):
        value = value.get(part, "") if isinstance(value, dict) else ""
    print(value)
elif command == "write":
    context, signed_path, signature_path = sys.argv[3:6]
    tamper = len(sys.argv) > 6 and sys.argv[6] == "tamper"
    if tamper:
        payload = payload.replace(b"publisher.example", b"bad-actor.example")
    open(signed_path, "wb").write(context.encode() + b"\n" + payload)
    raw = b64url(envelope["signature"]["value"])
    assert len(raw) == 64, "the signature is the raw r and s pair"
    body = der_integer(raw[:32]) + der_integer(raw[32:])
    open(signature_path, "wb").write(b"\x30" + bytes([len(body)]) + body)
PYEOF

field() { "$PYTHON" "$WORK/evidence.py" "$BODY" field "$1"; }
envelope_field() { "$PYTHON" "$WORK/evidence.py" "$BODY" envelope "$1"; }

verifies() { # verifies <public key> <context> [tamper], prints yes or no
  "$PYTHON" "$WORK/evidence.py" "$BODY" write "$2" "$WORK/signed.bin" "$WORK/signature.der" "${3:-}"
  if openssl dgst -sha256 -verify "$WORK/$1.pem" -signature "$WORK/signature.der" \
    "$WORK/signed.bin" >/dev/null 2>&1; then
    echo yes
  else
    echo no
  fi
}

info "The evidence, asked for as data with a nonce"
fetch "/_ts/attestation.json?nonce=Harness_1"
check "the endpoint answers" "$STATUS" "200"
check "no cache may store the answer" "$(header cache-control)" "no-store"
check "a page of any origin may read it" "$(header access-control-allow-origin)" "*"
check "the signature names its algorithm" "$(envelope_field signature.alg)" "ES256"
check "the key in force signed, and not the one that starts later" \
  "$(envelope_field signature.kid)" "harness-first"
check "the signature verifies over the context, a line feed and the payload" \
  "$(verifies first "$CONTEXT")" "yes"
check "it does not verify with the other key of the schedule" \
  "$(verifies next "$CONTEXT")" "no"
check "it does not verify under another context" \
  "$(verifies first "another-purpose:v1")" "no"
check "it does not verify once the host in the payload is changed" \
  "$(verifies first "$CONTEXT" tamper)" "no"
check "the host is the one the request named, lower case and without its port" \
  "$(field host)" "$PUBLISHER"
check "the publisher is the settings' own" "$(field publisher)" "$PUBLISHER"
check "the nonce is signed back as sent" "$(field nonce)" "Harness_1"
check "the operator is the settings' own" "$(field operator)" "Harness Operator"
check "the platform names itself" "$(field platform.name)" "fastly"
check "the build reports the commit it was given" "$(field build.commit)" "harness-commit"
check "the build reports the run it was given" "$(field build.run)" "harness-run"
check "the build reports the tag it was given" "$(field build.tag)" "harness-tag"
check "the build reports the time it was given" "$(field build.builtAt)" "$BUILD_TIME"
ISSUED="$(field issuedAt)"
AGE="$("$PYTHON" -c 'import datetime, sys; t = datetime.datetime.strptime(sys.argv[1], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc); print(int(abs((datetime.datetime.now(datetime.timezone.utc) - t).total_seconds()) < 300))' "$ISSUED" 2>/dev/null || echo 0)"
check "the evidence was signed within the last five minutes" "$AGE" "1"

info "What a relay or a careless caller cannot change"
fetch "/_ts/attestation.json?nonce=Harness_2" \
  -H "X-Forwarded-Host: bad-actor.example" -H "Forwarded: host=bad-actor.example"
check "a forwarded host header never becomes the signed host" "$(field host)" "$PUBLISHER"
check "and the evidence still verifies" "$(verifies first "$CONTEXT")" "yes"
fetch "/_ts/attestation.json"
if "$PYTHON" -c 'import base64, json, sys; e = json.load(open(sys.argv[1])); p = e["payload"]; sys.exit("nonce" in json.loads(base64.urlsafe_b64decode(p + "=" * (-len(p) % 4))))' "$BODY"; then
  ok "no nonce is signed unless one is asked for"
else
  bad "a nonce is signed although none was asked for"
fi
fetch "/_ts/attestation.json?nonce=not%20a%20nonce"
check "a malformed nonce is refused" "$STATUS" "400"
fetch "/_ts/attestation"
check "the page answers" "$STATUS" "200"
if grep -qF "Do not take this page" "$BODY"; then
  ok "the page claims nothing and sends the reader to a verifier"
else
  bad "the page does not tell the reader to check the claim"
fi
fetch "/_ts/attestation.json" -X POST
if [ "$STATUS" != "200" ] && ! grep -qF '"payload"' "$BODY"; then
  ok "another method is not answered with evidence ($STATUS)"
else
  bad "a POST was answered with evidence"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
  echo "Viceroy log:" >&2
  tail -40 "$WORK/viceroy.log" >&2
  exit 1
fi
