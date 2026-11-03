# Real GPT local acceptance

This gate is separate from the deterministic GPT fixture tests. It starts the actual Rust WASM in Viceroy, the Next.js fixture container, real GPT through the production first-party proxy, and the existing local OpenRTB provider fixture. It never installs `gpt-fixture.js` or synthesizes a GPT response.

No live acceptance or original-main reproduction has been executed without operator-supplied dedicated inventory. Missing inventory or original-main artifacts produce a nonzero `blocked` outcome, not a skip. A local pass does not validate a deployed service.

## Required inventory

Supply an external JSON file. Do not commit inventory, creative IDs, customer data, or credentials. There are no inventory defaults. The following describes the fields, but is deliberately invalid until an inventory owner supplies values and authorizes traffic.

```json
{
  "schemaVersion": 1,
  "dedicatedTestInventory": true,
  "authorizeGoogleTraffic": false,
  "registered": {
    "path": "<dedicated GAM registered ad unit path>",
    "tsCreativeId": "<positive integer GAM TS creative ID>",
    "ordinaryCreativeId": "<positive integer GAM fallback creative ID>",
    "tsMarker": "Fixture publisher registration winner",
    "ordinaryMarker": "<unique visible ordinary creative text>"
  },
  "unrelated": {
    "path": "<different dedicated GAM ad unit path>"
  }
}
```

The two paths must differ. This fixture supports a numeric GAM network segment and ordinary slash-separated unit names. Configure dedicated 300x250 inventory as follows:

- The registered ad unit must return the known TS test creative when the first request carries `hb_bidder=fixture-bidder`, `hb_adid=fixture-win-fixture-header`, and its price targeting. That GAM creative must use the real TS-compatible PUC/creative bridge. The local provider returns visible ADM text `Fixture publisher registration winner`, not a debug creative.
- Without candidate targeting, the registered ad unit must return the supplied ordinary creative ID and visible marker.
- The unrelated ad unit must return that same ordinary creative ID and marker when explicitly refreshed. It must not request anything before that explicit refresh.
- Do not enable inventory-side automatic refresh. No GAM credentials are needed by the launcher. An inventory owner must configure the creative/line-item selection before authorizing Google traffic.

The launcher uses the real default GPT bootstrap URL. Google is never mapped to the local provider. Only the fictional auction provider's production-predicted backend names map to loopback.

## Artifacts and preparation

Use the repository-pinned Node, Rust, Viceroy, and Docker. Install browser dependencies and Chromium through the coordinator's normal setup. `--prebid DIR` takes the existing verified external Prebid artifact directory containing `manifest.json`, its real bundle, SHA-256, and SHA-384 SRI. `--dist DIR` must be the matched current production TSJS bundle directory.

`--wasm PATH` accepts an already built current WASM with its existing `.publisher-registration.json` source/byte stamp. The launcher verifies it against the current source and supplied dist. Omit `--wasm` to let this launcher build and stamp the current WASM. It does not rebuild TSJS dist implicitly.

The original-main baseline is mandatory for full live acceptance. Supply `--baseline-wasm PATH --baseline-dist DIR`, built from original main `51730bdaf`, not a current build using old options. The baseline source archive can contain the separately recorded fixture-only build adaptations needed to export artifacts. Its sibling `.publisher-registration-baseline.json` stamp records the original full revision, immutable WASM hash, matched baseline dist hash, and optionally the baseline source-manifest hash.

Immediately after the coordinator's original-main build, stamp it from the repository root:

```bash
LC_ALL=C \
BASELINE_WASM=/absolute/original-main.wasm \
BASELINE_DIST=/absolute/original-main-dist \
BASELINE_SOURCE_MANIFEST=/absolute/baseline-source.json \
BASELINE_REVISION="$(git rev-parse 51730bdaf)" \
node <<'NODE'
const fs = require('node:fs');
const h = require('./crates/trusted-server-integration-tests/browser/publisher-registration/real-gpt.cjs');
const e = process.env;
fs.writeFileSync(`${e.BASELINE_WASM}.publisher-registration-baseline.json`, JSON.stringify({
  schemaVersion: 1,
  gitRevision: e.BASELINE_REVISION,
  wasmSha256: h.sha256(fs.readFileSync(e.BASELINE_WASM)),
  bundleSha256: h.distFingerprint(e.BASELINE_DIST),
  sourceManifestSha256: h.sha256(fs.readFileSync(e.BASELINE_SOURCE_MANIFEST))
}, null, 2));
NODE
```

`distFingerprint` matches `cd DIST; find . -maxdepth 1 -name 'tsjs-*.js' -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum`. The explicit `LC_ALL=C` makes this independent of the invoking shell's locale. It hashes the concatenated `<sha256>  ./<filename>\n` lines, not concatenated bundle bytes.

Prepare without starting a runtime or contacting Google:

```bash
node scripts/test-publisher-registration-real-gpt.mjs \
  --inventory /absolute/dedicated-inventory.json \
  --dist /absolute/current-dist --wasm /absolute/current.wasm \
  --prebid /absolute/verified-prebid \
  --baseline-wasm /absolute/original-main.wasm \
  --baseline-dist /absolute/original-main-dist \
  --out /absolute/evidence --prepare
```

Preparation generates readable candidate and legacy `app.toml`, invokes the existing `generate-viceroy-config` and `publisher_registration_backends` tools, and builds/reuses the source-fingerprinted Next.js image. The legacy app omits `initialization` entirely. Both app configs set the exact operator-supplied `gam_unit_path`. `--image TAG` is optional and must have a matching `org.trusted-server.fixture-source` label. Real inventory enters only through `docker run` server environment, never the image build.

Run the same command without `--prepare` for full live acceptance. `--origin-port PORT` changes the default loopback origin port 8888. Preparation can invoke Cargo and Docker builds; coordinate ownership before running it.

`--preflight` validates inventory and original-main artifact provenance only. `--list` lists the candidate and baseline tests without starting a runtime. Both require `--inventory`, and report blocked if baseline artifacts are absent. Neither is evidence that a browser test passed.

## Cases and assertions

The original-main case holds actual Next.js hydration scripts until the original automatic SSR placement renders the known TS creative. It then releases hydration and mounts a changed final element ID with a narrower native size list and enabled initial loading. The observer requires two real request memberships, public lifecycle correlation, and an explicitly completed final render. It reports original failure only when the old disconnected native slot and final slot remain live, or the final request renders ordinary/no-fill instead of the initial TS creative. Unknown creatives and observation timeouts do not count as reproduction. Expected React hydration errors are retained in baseline evidence.

The current opt-in cases cover:

- eligible first-request TS targeting and known creative, with unrelated native inventory untouched;
- no-bid and incompatible eligibility ordinary fallback;
- failed and late deferred runtime with the actual early renderer retained, eligible TS delivery before takeover, and no repeated request after takeover;
- eligible TS-first delivery followed by a controlled actual-provider no-bid transition, fresh real Prebid/provider exchanges for both registered and unrelated ad units, candidate-targeting retirement, and an actual two-member SRA refresh;
- React SSR-placeholder replacement before final registration;
- cancellation before scheduled dispatch, then successful native destruction while the element is connected, with actual `getSlots()` absence before DOM removal;
- Next.js navigation, teardown, and ordinary no-bid replacement.

The two deferred-runtime fault cases leave the actual early compiled entry, renderer, and script guard intact. They fail/hold only the deferred GPT runtime asset. The known TS creative and eligible targeting must render before a late runtime is released. The late case then waits for actual real-Prebid/refresh-wrapper initialization and rejects another initial request. All cases require first-party bootstrap and `pubads_impl` responses with `X-GPT-Proxy: true`, and reject direct GPT-script bypass.

The observer decodes GET/URL-encoded POST and SRA inventory/targeting. It accounts for every per-slot network membership using distinct configured paths and public `slotRequested` events, then checks creative ID and a visible marker inside the corresponding real GPT frame. The marker and every containing iframe must have positive viewport-intersecting geometry and visible, nontransparent, noncollapsed DOM ancestors. Hidden frame text alone is not evidence. Unknown encodings, repeated paths in one SRA request, undecodable membership, extra refreshes, wrong mounts, unknown/wrong creatives, missing markers, and no-fill fail. There are no timing-based success thresholds. Polling timeouts bound observation; they do not prove the absence of an event indefinitely.

The publisher authors real `pbjs.addAdUnits` entries with fixture-bidder parameters before refresh. `PUT /api/fixture-auction` accepts only `{ "noBid": true|false }` and changes actual local provider behavior without navigating or resetting the consumed native slot. Every provider exchange records its effective mode. `DELETE` resets both the ledger and forced mode; the existing `/no-bid` path continues to work without a control setting.

Pure observer tests include extra-refresh and wrong-creative rejection, malformed/ambiguous SRA, original-main false-reproduction rejection, strict options, config generation, dist fingerprint compatibility, and default/path/forced/reset provider controls:

```bash
node --test crates/trusted-server-integration-tests/browser/publisher-registration/real-gpt.test.cjs \
  scripts/test-publisher-registration-real-gpt.test.mjs
```

## Evidence and remaining coverage

`outcome.json` records `status`, `mode`, `liveAcceptancePassed`, `originalFailureObserved`, separate candidate/baseline outcomes, and explicit total/passed/failed/skipped counts. Before reading preparation inputs or owning resources, the launcher atomically replaces any previous root outcome with `status=running` and both proof flags false. Full acceptance requires a terminal passing outcome for both runs, positive test counts, zero skips, a proven original failure, and all candidate assertions. Preparation, listing, or preflight always leaves `liveAcceptancePassed=false`.

SIGINT and SIGTERM abort outstanding commands/readiness requests and unwind the owned runtime once, then write a failed outcome with exit 130 or 143 and the received signal. Subprocesses run asynchronously in process groups created by this launcher; it never signals unrelated groups. An in-flight Docker start acknowledgement finishes before cleanup uses its reserved unique name, avoiding a race with daemon-side container creation. `commands.json` records owned command PIDs; each runtime's `runtime.json` records its container name and Viceroy PID/group. Cleanup retains Docker logs and its stop result.

SIGKILL, host shutdown, or a second external force-kill cannot run JavaScript cleanup. An invocation killed after startup invalidation remains `running` with `liveAcceptancePassed=false`, never an old success. Its resources may still need manual cleanup. Verify the recorded container/process identities before removing them. The focused SIGKILL regression cleans its own local discovery subprocess; it does not claim automatic resource cleanup after SIGKILL.

The evidence directory retains source/artifact hashes, toolchain, command outcomes, provider preflight, generated configs/mappings, Docker/Viceroy logs, raw browser request URLs and bodies, raw GPT/ad/document/script response bytes with hashes, public lifecycle snapshots, raw provider ledger, original-main reproduction details, screenshots, and Playwright traces. Failures retain the same observations and `failure.txt`. Treat these artifacts as sensitive because live test inventory appears in them.

Live execution remains blocked until dedicated inventory is provided. This suite does not cover arbitrary consent matrices, viewport breakpoint races, repeated same-path inventory in one SRA batch, every navigation/cancellation interleaving, or field CWV. The baseline narrows sizes; it does not independently cover every reordered-size variant. These gaps must not be reported as passing evidence.
