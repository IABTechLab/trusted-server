#!/usr/bin/env bash
# Run every registered Criterion bench in trusted-server-core, plus a
# Viceroy-based TTFB benchmark for EC-finalize, and save results.
#
# Usage: ./run-benches.sh <stage> [viceroy-request-count]
#   stage examples: before-fix, fix1, fix2, fix3
#   viceroy-request-count defaults to 50 requests per scenario
#
# Each Criterion run saves a baseline named after <stage>. Every stage other
# than "before-fix" also compares against the "before-fix" baseline, so the
# printed output (and the saved file) shows the % change since the original
# baseline. Bench targets are read from trusted-server-core/Cargo.toml, so a
# bench added later (e.g. registry_bundle_bench, added alongside Fix 2) is
# picked up automatically without editing this script.
#
# The Viceroy part covers what Criterion can't: fixes that live in
# trusted-server-adapter-fastly and depend on the Fastly KV store SDK (e.g.
# Fix 1, deferring the EC-finalize KV write past response send). It builds
# the release Fastly wasm binary, generates the integration-test Viceroy
# config, starts a minimal local origin plus Viceroy itself, and times two
# request scenarios against them:
#   - "withdrawal": ts-ec cookie for a pre-seeded identity + Sec-GPC: 1 —
#     exercises Fix 1's deferred tombstone write.
#   - "returning": same cookie, no GPC — consent-granted returning-visitor
#     control group, untouched by Fix 1, to sanity-check the comparison
#     isn't just machine noise.
# Only the withdrawal branch of EC-finalize deferral is reachable this way —
# the generated-EC branch needs a real JA4 TLS fingerprint to pass the
# browser bot gate, which Viceroy's plain-HTTP runtime never produces (see
# crates/trusted-server-integration-tests/tests/integration.rs,
# test_ec_lifecycle_fastly's doc comment). Also note Viceroy's local KV store
# is an in-memory HashMap with near-zero latency, so this won't show the real
# win of deferring a network round-trip — it's a correctness/regression
# check, not a production TTFB measurement. That needs a live Fastly service.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${REPO_ROOT}"

STAGE="${1:?Usage: $0 <stage> [viceroy-request-count]  (e.g. before-fix, fix1, fix2, fix3)}"
VICEROY_REQUEST_COUNT="${2:-50}"
BASELINE_STAGE="before-fix"
CARGO_TOML="crates/trusted-server-core/Cargo.toml"

# --- Criterion benches (trusted-server-core, pure functions) ---

mapfile -t BENCHES < <(
  awk '
    /^\[\[bench\]\]/ { in_bench = 1; next }
    in_bench && /^name[[:space:]]*=/ {
      gsub(/.*=[[:space:]]*"|"[[:space:]]*$/, "")
      print
      in_bench = 0
    }
  ' "${CARGO_TOML}"
)

if [[ ${#BENCHES[@]} -eq 0 ]]; then
  echo "No [[bench]] entries found in ${CARGO_TOML}" >&2
  exit 1
fi

echo "Stage: ${STAGE}"
echo "Criterion benches to run: ${BENCHES[*]}"
echo

for bench in "${BENCHES[@]}"; do
  out="bench-result-${STAGE}-${bench}.txt"
  echo "==> ${bench} (stage: ${STAGE}) -> ${out}"

  if [[ "${STAGE}" == "${BASELINE_STAGE}" ]]; then
    cargo bench -p trusted-server-core --bench "${bench}" -- \
      --save-baseline "${STAGE}" | tee "${out}"
  else
    # This criterion version rejects combining --save-baseline with
    # --baseline. --baseline alone compares against the saved baseline
    # without overwriting it (the fresh run lands in criterion's internal
    # "new"/"change" scratch dirs), which is what we want: before-fix stays
    # intact for every later stage to compare against.
    set +e
    cargo bench -p trusted-server-core --bench "${bench}" -- \
      --baseline "${BASELINE_STAGE}" 2>&1 | tee "${out}"
    status="${PIPESTATUS[0]}"
    set -e
    if [[ "${status}" -ne 0 ]]; then
      # A bench added after the before-fix stage last ran has no saved
      # baseline yet to compare against — establish one now from the
      # current state instead of failing the whole run.
      {
        echo
        echo "No '${BASELINE_STAGE}' baseline yet for ${bench} (bench added after that stage ran) — establishing it from the current state."
      } | tee -a "${out}"
      cargo bench -p trusted-server-core --bench "${bench}" -- \
        --save-baseline "${BASELINE_STAGE}" | tee -a "${out}"
    fi
  fi
  echo
done

# --- Viceroy TTFB bench (trusted-server-adapter-fastly, EC-finalize) ---

ORIGIN_PORT=8888
VICEROY_PORT=7878
BASE_URL="http://127.0.0.1:${VICEROY_PORT}"
VICEROY_CONFIG="target/integration-test-artifacts/configs/viceroy.toml"
WASM_PATH="target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"
VICEROY_OUT="bench-result-${STAGE}-viceroy.txt"

UA="Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
# Pre-seeded by crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml
WITHDRAWAL_EC_ID="$(printf 'a%.0s' $(seq 1 64)).test01"
RETURNING_EC_ID="$(printf 'b%.0s' $(seq 1 64)).test02"

ORIGIN_PID=""
VICEROY_PID=""

cleanup_viceroy() {
    [ -n "${VICEROY_PID}" ] && kill "${VICEROY_PID}" 2>/dev/null || true
    [ -n "${ORIGIN_PID}" ] && kill "${ORIGIN_PID}" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup_viceroy EXIT

echo "==> Building release Fastly wasm binary (stage: ${STAGE}) -> ${VICEROY_OUT}"
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1

echo "==> Generating Viceroy config"
./scripts/generate-integration-viceroy-configs.sh

echo "==> Starting minimal origin on 127.0.0.1:${ORIGIN_PORT}"
python3 -c "
import http.server, socketserver
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'<html><body><h1>Test Origin</h1></body></html>'
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass
socketserver.ThreadingTCPServer.allow_reuse_address = True
with socketserver.ThreadingTCPServer(('127.0.0.1', ${ORIGIN_PORT}), H) as httpd:
    httpd.serve_forever()
" &
ORIGIN_PID=$!

echo "==> Starting Viceroy on ${BASE_URL}"
viceroy serve "${WASM_PATH}" -C "${VICEROY_CONFIG}" --addr "127.0.0.1:${VICEROY_PORT}" \
    > "/tmp/viceroy-bench-${STAGE}.log" 2>&1 &
VICEROY_PID=$!

echo "==> Waiting for readiness"
for _ in $(seq 1 30); do
    if curl -s -o /dev/null --max-time 2 "${BASE_URL}/health"; then
        break
    fi
    sleep 1
done
if ! curl -s -o /dev/null --max-time 2 "${BASE_URL}/health"; then
    echo "Viceroy did not become ready; see /tmp/viceroy-bench-${STAGE}.log" >&2
    exit 1
fi

# Warm up (JIT/first-instance effects, discarded)
for _ in $(seq 1 5); do
    curl -s -o /dev/null "${BASE_URL}/" \
        -H "Accept: text/html" -H "Sec-Fetch-Dest: document" -H "Sec-Fetch-Mode: navigate" \
        -H "User-Agent: ${UA}" -H "Cookie: ts-ec=${RETURNING_EC_ID}; us_privacy=1YNN"
done

run_viceroy_scenario() {
    local label="$1"
    shift
    local extra_headers=("$@")
    local ttfbs=()

    for _ in $(seq 1 "${VICEROY_REQUEST_COUNT}"); do
        local ttfb
        ttfb=$(curl -s -o /dev/null -w "%{time_starttransfer}" --max-time 10 \
            -H "Accept: text/html" -H "Sec-Fetch-Dest: document" -H "Sec-Fetch-Mode: navigate" \
            -H "User-Agent: ${UA}" "${extra_headers[@]}" "${BASE_URL}/")
        ttfbs+=("${ttfb}")
    done

    printf '%s\n' "${ttfbs[@]}" | awk -v label="${label}" '
        { sum += $1; vals[NR] = $1; if ($1 > max) max = $1; if (min == "" || $1 < min) min = $1 }
        END {
            n = NR
            mean = sum / n
            asort(vals)
            p50 = vals[int(n * 0.50) + 1]
            p95 = vals[int(n * 0.95) < n ? int(n * 0.95) + 1 : n]
            printf "%-12s n=%-4d mean=%.2fms p50=%.2fms p95=%.2fms min=%.2fms max=%.2fms\n", \
                label, n, mean * 1000, p50 * 1000, p95 * 1000, min * 1000, max * 1000
        }
    '
}

{
    echo "# Viceroy EC-finalize TTFB benchmark: ${STAGE}"
    echo "# Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
    echo "# Git: $(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
    echo "# Requests per scenario: ${VICEROY_REQUEST_COUNT}"
    echo ""
    echo "==> withdrawal (ts-ec cookie + Sec-GPC: 1 — exercises Fix 1's deferred tombstone write)"
    run_viceroy_scenario "withdrawal" -H "Cookie: ts-ec=${WITHDRAWAL_EC_ID}" -H "Sec-GPC: 1"
    echo ""
    echo "==> returning (ts-ec cookie, consent granted — control group, untouched by Fix 1)"
    run_viceroy_scenario "returning" -H "Cookie: ts-ec=${RETURNING_EC_ID}; us_privacy=1YNN"
} | tee "${VICEROY_OUT}"

cleanup_viceroy
trap - EXIT

echo
echo "Done. Results saved as bench-result-${STAGE}-*.txt in ${REPO_ROOT}"
