#!/usr/bin/env bash
# Isolated provider preflight and actual Next.js publisher-registration gate.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${PUBLISHER_REGISTRATION_OUT:-${CARGO_TARGET_DIR:-$ROOT/target}/integration-test-artifacts/publisher-registration}"
MODE=framework
WASM=""
IMAGE=""
IMAGE_PROVIDED=0
DIST=""
BUNDLE_MODE=prebuilt
PREBID=""
ARTIFACTS=""
REUSE=""
usage() {
    echo "Usage: $0 [--transport-smoke|--prepare|--stamp-wasm] [--out DIR] [--wasm PATH] [--dist DIR] [--bundle-mode prebuilt|build-rs] [--image TAG] [--prebid DIR] [--artifacts DIR|--reuse-artifacts DIR]" >&2
}
while (($#)); do
    case "$1" in
        --transport-smoke) MODE=transport; shift ;;
        --prepare) MODE=prepare; shift ;;
        --stamp-wasm) MODE=stamp; shift ;;
        --out) OUT="$2"; shift 2 ;;
        --wasm) WASM="$2"; shift 2 ;;
        --dist) DIST="$2"; shift 2 ;;
        --bundle-mode) BUNDLE_MODE="$2"; shift 2 ;;
        --image) IMAGE="$2"; IMAGE_PROVIDED=1; shift 2 ;;
        --prebid) PREBID="$2"; shift 2 ;;
        --artifacts) ARTIFACTS="$2"; shift 2 ;;
        --reuse-artifacts) REUSE="$2"; shift 2 ;;
        *) usage; exit 2 ;;
    esac
done
# Feature checks require an explicitly selected current bundle set. Do not inherit
# a coordinator's frozen Task 0 baseline through TSJS_PREBUILT_DIR accidentally.
if [[ "$MODE" != transport && -z "$DIST" ]]; then
    echo '--dist is required for feature preparation, stamping and framework checks' >&2
    exit 2
fi
if [[ "$BUNDLE_MODE" != prebuilt && "$BUNDLE_MODE" != build-rs ]]; then usage; exit 2; fi
if [[ -n "$DIST" ]]; then
    DIST="$(cd "$DIST" && pwd)"
    export TSJS_PREBUILT_DIR="$DIST"
fi
if [[ "$BUNDLE_MODE" == build-rs ]]; then unset TSJS_PREBUILT_DIR; fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
exec > >(tee "$OUT/launcher.log") 2>&1
STARTED="$(date -u +%FT%TZ)"
FIXTURES="$ROOT/crates/trusted-server-integration-tests/fixtures"
APP="$FIXTURES/configs/trusted-server.publisher-registration.toml"
ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
ORIGIN="http://127.0.0.1:$ORIGIN_PORT"
HOST_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
VICEROY_PID=""
CONTAINER_ID=""
cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$VICEROY_PID" ]]; then
        kill "$VICEROY_PID" 2>/dev/null || true
        wait "$VICEROY_PID" 2>/dev/null || true
    fi
    if [[ -n "$CONTAINER_ID" ]]; then
        docker logs "$CONTAINER_ID" > "$OUT/nextjs.log" 2>&1 || true
        docker stop "$CONTAINER_ID" >/dev/null 2>&1 || true
    fi
    printf 'started=%s\nfinished=%s\nexit=%s\n' "$STARTED" "$(date -u +%FT%TZ)" "$status" > "$OUT/outcome.txt"
    echo "Publisher registration gate exit=$status; evidence: $OUT"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

fingerprint() {
    git ls-files --cached --others --exclude-standard -z -- "$@" ':!**/node_modules' ':!**/node_modules/**' |
        sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}'
}
RUNTIME_SOURCE="$(fingerprint Cargo.toml Cargo.lock rust-toolchain.toml .tool-versions .cargo crates/trusted-server-core crates/trusted-server-adapter-fastly crates/trusted-server-js crates/trusted-server-openrtb ':!crates/trusted-server-js/lib/test/**')"
BUNDLE_SOURCE=build-rs
BUNDLE_DIRECTORY="${DIST:-${TSJS_PREBUILT_DIR:-}}"
if [[ -n "$BUNDLE_DIRECTORY" ]]; then
    BUNDLE_SOURCE="$(cd "$BUNDLE_DIRECTORY" && find . -maxdepth 1 -name 'tsjs-*.js' -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}')"
    (cd "$BUNDLE_DIRECTORY" && find . -maxdepth 1 -name 'tsjs-*.js' -type f -print0 | sort -z | xargs -0 sha256sum) > "$OUT/bundles.sha256"
fi
PREBUILT_SOURCE="$BUNDLE_SOURCE"
if [[ "$BUNDLE_MODE" == build-rs ]]; then PREBUILT_SOURCE=build-rs; fi
RUSTC_VERSION="$(rustc -Vv)"
WASM_SOURCE="$(printf '%s\n%s\n%s\n' "$RUNTIME_SOURCE" "$RUSTC_VERSION" "$PREBUILT_SOURCE" | sha256sum | awk '{print $1}')"
printf '%s\n' "$RUSTC_VERSION" > "$OUT/toolchain.txt"
IMAGE_SOURCE="$(fingerprint .tool-versions crates/trusted-server-integration-tests/fixtures/frameworks/nextjs)"
CONFIG_SOURCE="$(fingerprint crates/trusted-server-integration-tests/fixtures/configs/trusted-server.publisher-registration.toml crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml crates/trusted-server-integration-tests/examples/publisher_registration_backends.rs scripts/test-publisher-registration.sh)"
printf '%s\n' "$WASM_SOURCE" > "$OUT/wasm-source.sha256"
printf '%s\n' "$RUNTIME_SOURCE" > "$OUT/runtime-source.sha256"
printf '%s\n' "$BUNDLE_SOURCE" > "$OUT/bundle-source.sha256"
printf '%s\n' "$BUNDLE_MODE" > "$OUT/bundle-build-mode.txt"
printf '%s\n' "$IMAGE_SOURCE" > "$OUT/image-source.sha256"
sha256sum "$APP" "$0" "$ROOT/crates/trusted-server-integration-tests/examples/publisher_registration_backends.rs" > "$OUT/inputs.sha256"

stamp_wasm() {
    # Explicitly invoked immediately after the coordinator/CI build, or after
    # this launcher itself builds. --wasm alone never silently trusts bytes.
    node - "$WASM" "$WASM_SOURCE" "$BUNDLE_SOURCE" "$RUSTC_VERSION" "$BUNDLE_MODE" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
const [wasm, sourceSha256, bundleSha256, rustc, bundleMode] = process.argv.slice(2);
const wasmSha256 = crypto.createHash('sha256').update(fs.readFileSync(wasm)).digest('hex');
fs.writeFileSync(`${wasm}.publisher-registration.json`, JSON.stringify({schemaVersion:1, sourceSha256, bundleSha256, wasmSha256, rustc, bundleMode, target:'wasm32-wasip1'}, null, 2));
NODE
}
verify_wasm() {
    node - "$WASM" "$WASM_SOURCE" "$BUNDLE_SOURCE" "$RUSTC_VERSION" "$BUNDLE_MODE" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const [wasm, source, bundles, rustc, bundleMode] = process.argv.slice(2);
const stamp = JSON.parse(fs.readFileSync(`${wasm}.publisher-registration.json`));
assert.equal(stamp.schemaVersion, 1);
assert.equal(stamp.rustc, rustc, 'WASM toolchain changed');
assert.equal(stamp.bundleMode, bundleMode, 'WASM JS build mode changed');
assert.equal(stamp.target, 'wasm32-wasip1');
assert.equal(stamp.sourceSha256, source, 'WASM source fingerprint mismatch; rebuild and explicitly stamp');
assert.equal(stamp.bundleSha256, bundles, 'WASM bundle fingerprint mismatch');
assert.equal(stamp.wasmSha256, crypto.createHash('sha256').update(fs.readFileSync(wasm)).digest('hex'), 'WASM bytes changed after stamp');
NODE
}
if [[ -n "$WASM" ]]; then WASM="$(realpath "$WASM")"; fi
if [[ "$MODE" == stamp ]]; then
    [[ -n "$WASM" ]] || { echo '--stamp-wasm requires --wasm from an immediately preceding build'; exit 2; }
    stamp_wasm
    echo "Stamped source and bytes: $WASM.publisher-registration.json"
    exit 0
fi
if [[ "$MODE" != prepare ]]; then
    PINNED_VICEROY="$(awk '$1 == "viceroy" { print $2 }' "$ROOT/.tool-versions")"
    ACTUAL_VICEROY="$(viceroy --version)"
    [[ "$ACTUAL_VICEROY" == "viceroy $PINNED_VICEROY" ]] || {
        echo "Pinned Viceroy $PINNED_VICEROY required; found $ACTUAL_VICEROY" >&2
        exit 1
    }
fi
{ git rev-parse HEAD; git status --short; rustc -Vv; cargo --version; if command -v viceroy >/dev/null; then viceroy --version; else echo 'Viceroy not installed in artifact-preparation job'; fi; node --version; docker version --format '{{.Server.Version}}'; df -h / /tmp; } > "$OUT/environment.txt"
if [[ -n "$WASM" ]]; then
    verify_wasm
else
    WASM="${CARGO_TARGET_DIR:-$ROOT/target}/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"
    # Cargo owns incremental freshness when the launcher is the build owner.
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
    [[ "$RUNTIME_SOURCE" == "$(fingerprint Cargo.toml Cargo.lock rust-toolchain.toml .tool-versions .cargo crates/trusted-server-core crates/trusted-server-adapter-fastly crates/trusted-server-js crates/trusted-server-openrtb ':!crates/trusted-server-js/lib/test/**')" ]] || { echo 'Runtime source changed during build'; exit 1; }
    stamp_wasm
fi
sha256sum "$WASM" > "$OUT/wasm.sha256"
cp "$WASM.publisher-registration.json" "$OUT/wasm-build.json"

if [[ -z "$IMAGE" ]]; then IMAGE="test-nextjs:publisher-registration-${IMAGE_SOURCE:0:16}"; fi
IMAGE_LABEL="$(docker image inspect --format '{{index .Config.Labels "org.trusted-server.fixture-source"}}' "$IMAGE" 2>/dev/null || true)"
if [[ "$IMAGE_LABEL" != "$IMAGE_SOURCE" ]]; then
    if [[ -n "$REUSE" || "$IMAGE_PROVIDED" == 1 ]]; then
        echo "Prepared image missing or source-label mismatch: $IMAGE"; exit 1
    fi
    NODE_VERSION="$(awk '$1 == "nodejs" {print $2}' .tool-versions)"
    docker build --build-arg "NODE_VERSION=$NODE_VERSION" --label "org.trusted-server.fixture-source=$IMAGE_SOURCE" -t "$IMAGE" "$FIXTURES/frameworks/nextjs"
fi
docker image inspect "$IMAGE" > "$OUT/image.json"

# The configured external artifact is real Prebid, never an inert network stub.
# Transport-only callers can omit it because no browser executes that config.
PREBID_SHA=none
if [[ -n "$PREBID" ]]; then
    PREBID="$(cd "$PREBID" && pwd)"
    PREBID_SHA="$(node - "$PREBID" <<'NODE'
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const dir = process.argv[2];
const m = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json')));
assert.equal(m.schemaVersion, 1);
assert.equal(path.basename(m.filename), m.filename);
const bytes = fs.readFileSync(path.join(dir, m.filename));
assert.equal(crypto.createHash('sha256').update(bytes).digest('hex'), m.sha256);
assert.equal(`sha384-${crypto.createHash('sha384').update(bytes).digest('base64')}`, m.sri);
process.stdout.write(m.sha256);
NODE
)"
    cp "$PREBID/manifest.json" "$OUT/prebid-manifest.json"
elif [[ "$MODE" != transport ]]; then
    echo '--prebid DIR with verified manifest is required'; exit 2
fi

if [[ -n "$REUSE" ]]; then
    ARTIFACTS="$(cd "$REUSE" && pwd)"
    node - "$ARTIFACTS" "$CONFIG_SOURCE" "$PREBID_SHA" "$ORIGIN" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const [dir, source, prebid, origin] = process.argv.slice(2);
const m = JSON.parse(fs.readFileSync(`${dir}/artifacts.json`));
assert.equal(m.schemaVersion, 1);
assert.equal(m.configSource, source, 'prepared config source changed');
assert.equal(m.prebidSha256, prebid, 'prepared Prebid artifact changed');
assert.equal(m.origin, origin, 'prepared origin/port changed');
for (const [name, hash] of Object.entries(m.files)) assert.equal(crypto.createHash('sha256').update(fs.readFileSync(`${dir}/${name}`)).digest('hex'), hash, `prepared artifact changed: ${name}`);
NODE
else
    ARTIFACTS="${ARTIFACTS:-$OUT/artifacts}"
    mkdir -p "$ARTIFACTS"
    ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"
    for variant in full reduced inline shared; do
        CASE_OUT="$ARTIFACTS/$variant"
        mkdir -p "$CASE_OUT"
        node - "$APP" "$CASE_OUT/app.toml" "$variant" "$PREBID" <<'NODE'
const fs = require('node:fs');
const [input, output, variant, prebid] = process.argv.slice(2);
let app = fs.readFileSync(input, 'utf8');
if (variant === 'reduced') app = app.replace('auction_timeout_ms = 1500', 'auction_timeout_ms = 375');
if (variant === 'shared') app = app.replace('assembly_mode = "inline"', 'assembly_mode = "esi"\norigin_is_cookie_independent = true\ntemplate_cache_vary = ["rsc", "next-router-state-tree", "next-router-prefetch", "next-url"]');
if (prebid) {
    const m = JSON.parse(fs.readFileSync(`${prebid}/manifest.json`));
    app = app.replace('external_bundle_url = "https://assets.example.com/fixture-prebid.js"', `external_bundle_url = "https://assets.example.com/fixture-prebid.js"\nexternal_bundle_sha256 = "${m.sha256}"\nexternal_bundle_sri = "${m.sri}"`);
}
fs.writeFileSync(output, app);
NODE
        cargo run -p trusted-server-integration-tests --target "$HOST_TARGET" --bin generate-viceroy-config -- \
            --app-config "$CASE_OUT/app.toml" --template "$FIXTURES/configs/viceroy-template.toml" --origin-url "$ORIGIN" --output "$CASE_OUT/viceroy.toml"
        cargo run -p trusted-server-integration-tests --target "$HOST_TARGET" --example publisher_registration_backends -- \
            "$CASE_OUT/app.toml" "$ORIGIN" "$CASE_OUT/backends.toml" "$CASE_OUT/predicted.json"
        tee -a "$CASE_OUT/viceroy.toml" < "$CASE_OUT/backends.toml" >/dev/null
    done
    node - "$ARTIFACTS" "$CONFIG_SOURCE" "$PREBID_SHA" "$ORIGIN" <<'NODE'
const fs = require('node:fs');
const crypto = require('node:crypto');
const [dir, configSource, prebidSha256, origin] = process.argv.slice(2);
const files = {};
for (const variant of ['full','reduced','inline','shared']) for (const file of ['app.toml','viceroy.toml','backends.toml','predicted.json']) {
    const name = `${variant}/${file}`;
    files[name] = crypto.createHash('sha256').update(fs.readFileSync(`${dir}/${name}`)).digest('hex');
}
fs.writeFileSync(`${dir}/artifacts.json`, JSON.stringify({schemaVersion:1,configSource,prebidSha256,origin,files}, null, 2));
NODE
fi
cp "$ARTIFACTS/artifacts.json" "$OUT/prepared-artifacts.json"
if [[ "$MODE" == prepare ]]; then
    echo "Prepared verified artifacts: $ARTIFACTS"
    exit 0
fi
wait_ready() {
    local url="$1"
    for ((i=0; i<120; i++)); do
        if curl -fsS --max-time 2 "$url" >/dev/null 2>&1; then return 0; fi
        sleep 0.25
    done
    echo "Readiness failed: $url" >&2
    return 1
}
# Run the mandatory actual provider preflight before the framework suite.
CONTAINER_ID="$(docker run -d --rm -p "127.0.0.1:$ORIGIN_PORT:3000" -e "ORIGIN_HOST=127.0.0.1:$ORIGIN_PORT" "$IMAGE")"
printf '%s\n' "$CONTAINER_ID" > "$OUT/container.id"
wait_ready "$ORIGIN/api/fixture-auction"
CONSENT="$(node <<'NODE'
const bytes = Buffer.alloc(32);
const setBits = (offset, width, value) => {
    const bits = value.toString(2).padStart(width, '0');
    for (let index = 0; index < width; index++) if (bits[index] === '1') bytes[(offset + index) >> 3] |= 1 << (7 - ((offset + index) % 8));
};
const now = Math.floor(Date.now() / 100);
setBits(0, 6, 2); setBits(6, 36, now); setBits(42, 36, now); setBits(152, 24, 1 << 23);
process.stdout.write(bytes.toString('base64url'));
NODE
)"
for budget_case in full reduced; do
    CASE_OUT="$OUT/$budget_case"
    mkdir -p "$CASE_OUT"
    cp "$ARTIFACTS/$budget_case/"{app.toml,viceroy.toml,backends.toml,predicted.json} "$CASE_OUT/"
    curl -fsS -X DELETE "$ORIGIN/api/fixture-auction" > "$CASE_OUT/route-reset.json"
    PORT="$(node -e 'const s=require("node:net").createServer();s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')"
    RUST_LOG=info viceroy "$WASM" -C "$CASE_OUT/viceroy.toml" --addr "127.0.0.1:$PORT" --log-stdout --log-stderr > "$CASE_OUT/viceroy.log" 2>&1 &
    VICEROY_PID=$!
    printf '%s\n' "$VICEROY_PID" > "$CASE_OUT/viceroy.pid"
    wait_ready "http://127.0.0.1:$PORT/health"
    for response_case in win no-bid; do
        PATH_SUFFIX=""
        if [[ "$response_case" == no-bid ]]; then PATH_SUFFIX=/no-bid; fi
        curl -fsS --max-time 30 -H 'X-TSJS-Page-Bids: 1' -H 'Sec-Fetch-Site: same-origin' -H 'User-Agent: Mozilla/5.0 FixturePublisherRegistration' -H "Cookie: euconsent-v2=$CONSENT" \
            "http://127.0.0.1:$PORT/_ts/page-bids?path=/publisher-registration$PATH_SUFFIX" > "$CASE_OUT/payload-$response_case.json"
    done
    curl -fsS "$ORIGIN/api/fixture-auction" > "$CASE_OUT/exchanges.json"
    kill "$VICEROY_PID"
    wait "$VICEROY_PID" || true
    VICEROY_PID=""
    node - "$CASE_OUT" "$budget_case" <<'NODE'
const fs = require('node:fs');
const assert = require('node:assert/strict');
const [dir, budgetCase] = process.argv.slice(2);
const read = name => JSON.parse(fs.readFileSync(`${dir}/${name}.json`, 'utf8'));
const predicted = read('predicted');
const {exchanges} = read('exchanges');
const win = read('payload-win');
const noBid = read('payload-no-bid');
assert.equal(predicted.endpoint, 'https://auction.example.com/api/fixture-auction');
assert.equal(predicted.certificate_check, true);
assert.equal(exchanges.length, 2, 'both actual Rust requests must reach the route');
for (const exchange of exchanges) {
    assert.equal(exchange.path, '/api/fixture-auction');
    assert.equal(exchange.request.imp[0].id, 'fixture-header');
    assert.ok(exchange.request.imp[0].ext.prebid.bidder['fixture-bidder']);
    assert.ok(exchange.request.tmax > 0);
}
assert.ok(win.slots.some(slot => slot.id === 'fixture-header'));
const bid = win.bids['fixture-header'];
assert.equal(bid.hb_bidder, 'fixture-bidder');
assert.equal(bid.hb_adid, 'fixture-win-fixture-header');
assert.equal(bid.w, 300); assert.equal(bid.h, 250);
assert.match(bid.adm, /Fixture publisher registration winner/);
assert.equal(bid.debug_bid, undefined, 'fixture must exercise real rendering, not debug ADM replacement');
assert.equal(Object.keys(noBid.bids).length, 0);
assert.ok(noBid.slots.some(slot => slot.id === 'fixture-header'));
const log = fs.readFileSync(`${dir}/viceroy.log`, 'utf8');
const observed = [...new Set([...log.matchAll(/reusing existing dynamic backend: (backend_https_auction_example_com_443_p_[^\s]+)/g)].map(match => match[1]))];
assert.ok(observed.length, 'must observe static-name collision reuse');
const mappings = new Map(predicted.backends.map(backend => [backend.name, backend]));
for (const name of [...log.matchAll(/backend_https_auction_example_com_443[^\s"'<>),]*/g)].map(match => match[0])) assert.ok(mappings.has(name), `unmapped/external provider attempt ${name}`);
assert.doesNotMatch(log, /created dynamic backend: backend_https_auction_example_com/);
const timers = observed.map(name => mappings.get(name).transport_timeout_ms);
if (budgetCase === 'full') assert.ok(timers.includes(predicted.configured_timeout_ms));
else assert.ok(timers.every(timer => timer < predicted.configured_timeout_ms));
fs.writeFileSync(`${dir}/admission.json`, JSON.stringify({passed:true,budgetCase,observedBackends:observed,observedTimers:timers,providerRequests:exchanges.length,admittedBid:bid,noBidAdmitted:true}, null, 2));
console.log(`PASS ${budgetCase}: provider requests=${exchanges.length}, timers=${timers.join(',')}, winner=${bid.hb_adid}`);
NODE
done
docker logs "$CONTAINER_ID" > "$OUT/nextjs.log" 2>&1
docker stop "$CONTAINER_ID" >/dev/null
CONTAINER_ID=""
if [[ "$MODE" == transport ]]; then
    echo 'Transport preflight passed. No browser or production validation is implied.'
    exit 0
fi
for assembly in inline shared; do
    RUN_OUT="$OUT/framework-$assembly"
    mkdir -p "$RUN_OUT"
    (
        cd "$ROOT/crates/trusted-server-integration-tests/browser"
        TEST_FRAMEWORK=nextjs PUBLISHER_REGISTRATION_RUN=1 PUBLISHER_REGISTRATION_ASSEMBLY="$assembly" \
        WASM_BINARY_PATH="$WASM" VICEROY_CONFIG_PATH="$ARTIFACTS/$assembly/viceroy.toml" \
        INTEGRATION_FRAMEWORK_IMAGE="$IMAGE" PUBLISHER_REGISTRATION_LOG_DIR="$RUN_OUT" \
        PUBLISHER_REGISTRATION_PREBID_DIR="$PREBID" PUBLISHER_REGISTRATION_DIST="$DIST" \
        PUBLISHER_REGISTRATION_RESULTS="$RUN_OUT" \
        npx --no-install playwright test --config playwright.publisher-registration.config.ts
    )
done
echo 'Framework registration gate passed on actual Rust HTML and React lifecycle. Real GPT staging remains separate.'
