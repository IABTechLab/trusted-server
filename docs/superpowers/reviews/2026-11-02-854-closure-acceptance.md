# Issue 854 closure acceptance

Status, 2026-11-03: The cacheable synchronous candidate improves the reproduced cold/warm lab timings. **Keep the PR draft. Do not merge or close the issue.** Acceptance of synchronous loading and the changed critical-script outage boundary remains pending, and real-GPT inventory is still missing. The November 2 contract and failed inline/deferred measurements below remain historical evidence.

Issue: https://github.com/IABTechLab/trusted-server/issues/854
PR: https://github.com/IABTechLab/trusted-server/pull/1460

## November 3 recovery candidate

The supported GPT profile now uses the existing versioned synchronous unified bundle, with configuration before execution and neither raw nor compiled early bootstrap embedded in HTML. Other browser-module profiles retain their provisional initializer unless actual enforced CSP requires omission. Diagnostics, trace, attribution/APS metadata, reservation and separately deferred integration order remain intact. No production flag, retry, codec or public registration-protocol change is introduced.

The change has a recorded red/green loader regression. Fresh checks pass 3,376 native core tests, 20 integrations, 30 doctests, 60 Axum tests, 28 parity tests and all 23 selected build/test/lint/runtime gates, including eight target-specific Clippy aliases, adapter test aliases, actual EOF/auction controls and a new stamped WASM. Existing ignored tests remain ignored. The compiled-IIFE startup suite passes 16 tests, including pre-core queue context/configuration and complete GPT API availability after the unified script. The selected Node suite passes 80 tests with zero skips. The previous broad JS result remains evidence for unchanged production JS inputs, not a newly executed full suite.

The identity-transport diagnostic screen executes 40 measurements per cold/warm condition, four alternating arms at two widths with five samples per group. Both successful-runtime cache and single-request/targeting/visible-creative checks pass. Baseline to combined medians are:

| Condition      | Viewport  | FCP/LCP, ms | First visible ad estimate, ms |
| -------------- | --------- | ----------- | ----------------------------- |
| Throttled cold | 390x844   | 948 to 824  | 1,345.7 to 1,231.9            |
| Throttled cold | 1280x1000 | 956 to 828  | 1,350.0 to 1,208.7            |
| Throttled warm | 390x844   | 348 to 168  | 564.8 to 395.1                |
| Throttled warm | 1280x1000 | 352 to 152  | 563.0 to 382.2                |

The combined document is 2,104 bytes instead of the previous candidate's 128,941 bytes. All 14 production JS modules remain byte-identical. The actual unified static asset is the same one already exported for the previous CSP path. Controlled reservation still changes mobile/desktop CLS from 0.207065/0.111133 to zero. These are fictional loopback observations with uncompressed HTTP, not field or compressed-delivery CWV.

A separately executed four-case diagnostic covers normal and old-hash/self CSP success plus critical-unified HTTP 404 under each policy. Success has working APIs, one queue callback, one targeted native request and a visible creative. Failure has no initialization/shim/renderer APIs, no queue drain, one 404 with no retry, zero native requests or delivery, completed parsing and only the declared core-dependent publisher error. CSP violations remain zero. This observes the changed outage boundary; it does not approve it.

Both diagnostic scopes explicitly report `fullAcceptancePassed=false`, and the outage scope also reports `changedOutageBehaviorApproved=false`. The unchanged full verifier rejects both scopes. Running the existing full CWV correctness scope on the new export exits 1 before any sample with `loading must exercise its intended production loading mode`. Its deferred-runtime contract has not been rewritten to make this candidate green. Full framework/live consumer migration remains deferred until the architecture and outage decision. Required CI coverage is not replaced by diagnostic passes.

The synchronous candidate does not satisfy outcome 1 of the earlier closure contract below. Issue #854 permits documenting blocking dependencies, but accepting that alternative and the loss of the ordinary-page inline fallback requires an explicit decision. Dedicated real-GPT inventory and genuine original-main reproduction are also still missing. No closing keyword, ready transition or review request is appropriate at this checkpoint.

Evidence is under `/home/pav/.cache/ts-854-acceptance/recovery-2026-11-03/`: `screen-cold/`, `screen-warm/`, `outages/`, `full-contract-rejection/`, `gates/`, `unified-iife.log` and `all-node.log`. The measurement JSON adds `recovery20261103` with all 80 new metric rows, four outage observations, source/report/tooling hashes and the new WASM hash. Its original 200 measurements and historical blocked status are unchanged. The screen used its recorded initial tooling; the later opt-in outage extension is separately tested against the same production bytes, not relabeled as a timing rerun. Both independent read-only cross-reviews found no additional correctness findings in the loader or diagnostic changes.

## Closure contract

The PR must demonstrate these outcomes through the production HTML transformation and compiled JavaScript:

1. Supported integration profiles can continue parsing content without waiting for the external GPT runtime. Early APIs and interception remain functional. Other profiles retain tested synchronous delivery.
2. Explicit responsive reservation prevents the reproduced ad-induced layout shift. Unconfigured and unknown-height placements do not acquire an invented height policy.
3. A publisher-registered placement requests its final live native slot once initially, with eligible server targeting. An unrelated slot remains untouched, the creative is actually visible, and a legitimate later refresh still works.

Registration requires a publisher adapter and initial loading disabled before services are enabled. Reservation requires an explicit configuration and stable wrapper. This is not transparent duplicate prevention for unchanged publisher pages.

A successful offline run cannot stand in for real GPT. Full closure additionally requires the observed original-main failure and successful candidate delivery against dedicated real GPT/GAM test inventory. Missing prerequisites are blocked, not skipped or passing.

## Matched baseline

The baseline is original main `51730bdaf841e4d7bb96c2258e5e98169bdff9a7`. The candidate merges that revision in `2168a75fc` before adding the acceptance tests and their fixes. Cargo lockfile, JS lockfile, Rust pin and tool-version pins match.

Prepare the original source without backporting the fix:

```bash
node scripts/prepare-client-cwv-baseline.mjs \
  --revision 51730bdaf841e4d7bb96c2258e5e98169bdff9a7 \
  --out /absolute/new-baseline-directory
```

The helper archives the exact revision, adds only a `test-utils` publisher bridge and a compatible copy of the HTML exporter, and records the original archive hash, exact fixture adaptations and dependency fingerprints in `baseline-source.json`. It does not change baseline production initialization, script loading or reservation behavior.

Build TSJS in the archived tree with its own lockfile:

```bash
cd /absolute/new-baseline-directory/source/crates/trusted-server-js/lib
npm ci
npm run build
```

From the archived source root, export baseline HTML and build the original WASM:

```bash
export TSJS_PREBUILT_DIR="$PWD/crates/trusted-server-js/dist"
HOST_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
CWV_SOURCE_REVISION=51730bdaf841e4d7bb96c2258e5e98169bdff9a7 \
CWV_SOURCE_STATUS='Archive plus recorded fixture-only adaptations' \
  cargo run --locked -p trusted-server-integration-tests \
  --example cwv_html --target "$HOST_TARGET" -- /absolute/baseline-export unreserved
cargo build --locked -p trusted-server-adapter-fastly --release --target wasm32-wasip1
```

Copy the completed baseline WASM to a separate immutable path before building candidate artifacts in a shared Cargo target directory. Immediately create its baseline stamp using the command in the [real-GPT guide](../../../crates/trusted-server-integration-tests/browser/publisher-registration/real-gpt.README.md#artifacts-and-preparation).

Build/export the candidate using the [CWV runner instructions](../../../crates/trusted-server-integration-tests/browser/cwv/README.md). Use a current stamped WASM, matching TSJS dist, and the existing external Prebid artifact with Rubicon and SharedId. Do not reuse a stale Next.js image after changing the fixture adapter.

## Acceptance entry point

After preparing the artifacts:

```bash
node scripts/test-client-cwv-acceptance.mjs \
  --baseline /absolute/baseline-export \
  --loading /absolute/candidate-unreserved-export \
  --combined /absolute/candidate-reserved-export \
  --dist /absolute/candidate-dist \
  --wasm /absolute/candidate.wasm \
  --prebid /absolute/verified-prebid \
  --baseline-dist /absolute/original-main-dist \
  --baseline-wasm /absolute/original-main.wasm \
  --inventory /absolute/dedicated-inventory.json \
  --runs 5 --out /absolute/new-acceptance-evidence
```

Keep evidence outside Git. The wrapper snapshots all supplied exports, bundles, WASM/stamps and inventory before execution, passes the selected dist to every consumer, verifies candidate source fingerprints and matching baseline toolchain/dependency pins, and checks that source and snapshotted inputs remain unchanged through completion.

It runs:

- Fixture, observer, option, launcher and acceptance unit tests.
- Offline registration contracts and explicit legacy automatic-mode diagnostic controls. The latter are not mislabeled as original-main real-GPT evidence.
- Actual Rust/Next.js inline and shared-template contracts.
- Four comparison arms in five declared conditions: normal cold/warm, slower mobile cold/warm, and a controlled 750 ms runtime hold. Every arm runs at mobile and desktop widths with alternating repeated samples.
- Parser-blocking and reservation-removal negative controls. Warm runs must observe genuine HTTP cache reuse, including the successful runtime, without Playwright routing disabling that cache.
- Original-main and candidate real-GPT acceptance with actual provider admission, per-slot network membership, public lifecycle correlation and visible creative checks.

The slower mobile condition is 4x CPU, 100 ms network latency, 1,600 kbit/s download and 750 kbit/s upload. It is a declared lab condition, not a claim about all mobile visitors. Normal conditions have no artificial runtime hold or throttling. Timing distributions and transfer costs are reported without selecting a favorable sample or introducing an improvement threshold after measurement.

Omitting `--inventory` still runs the offline work but finishes nonzero with a blocked real-GPT row. The final controller reports `functionalGatesPassed` and `requiresPerformanceReview`; passing functional assertions is not an automatic issue-closing decision. Successful preparation, listing or structural preflight cannot set `liveAcceptancePassed=true`.

## Failures exposed by the stronger tests

The actual pre-PR baseline exposed an inline-bootstrap CSP hash incompatibility that the earlier registration-era baseline did not. Enforced-response-CSP pages now omit the changed provisional raw bootstrap and use the synchronous unified runtime before publisher scripts. The unchanged activation marker remains. No policy is weakened and no new inline permission is required.

This deliberately has no provisional fallback if that synchronous runtime fails to load. The acceptance suite must verify both successful old-hash/self delivery and this unavailable-runtime boundary. Non-CSP deferred-runtime failure retains the early renderer and remains a separate contract.

The merge also exposed two Rust test initializers missing the request-path/CSP or trace fields. These are corrected. The existing Prebid shim budget is adjusted only by its measured 135-character increase relative to matching main, preserving the existing 38-character headroom and the checks against embedding Prebid itself.

## Evidence and disposition

Local evidence root: `/home/pav/.cache/ts-854-acceptance/`. Initial failed attempts are not successful runs.

| Check                                                                         | Result                                                                                                                                    | Evidence                                       |
| ----------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Full JS                                                                       | 2,126 tests across 77 files passed; lint and format passed                                                                                | `full-js-final.log`, `js-{lint,format}.log`    |
| Native core                                                                   | 3,373 tests and 20 integrations passed; 30 doctests passed                                                                                | `core-native.log`                              |
| Axum                                                                          | 60 tests passed                                                                                                                           | `axum-native.log`                              |
| Cross-adapter parity                                                          | 28 tests passed                                                                                                                           | `parity.log`                                   |
| Adapter/CLI/codegen Clippy                                                    | All eight target-matched aliases passed                                                                                                   | `gates/clippy-*.log`                           |
| Fastly, reuse, Cloudflare and Spin test aliases                               | Passed                                                                                                                                    | `gates/test-*.log`                             |
| Native digest lint, clock, Node pins, URL domains and documentation proposals | Passed                                                                                                                                    | `gates/`                                       |
| EOF                                                                           | Native test/lint and actual Axum/Fastly auction controls passed                                                                           | `gates/eof-*.log`                              |
| ShellCheck and changed-workflow actionlint                                    | Passed using explicit installed binaries after shim-only failures                                                                         | `gates/*-retry.log`                            |
| Docs                                                                          | Format and build passed                                                                                                                   | `docs-{format,build}.log`                      |
| Next.js image                                                                 | Actual production build/typecheck passed                                                                                                  | `image-build.log`, `image-build-canonical.log` |
| Real-GPT preparation                                                          | Final async candidate and original-main config generation passed using fictional preparation-only inventory; no runtime or Google traffic | `real-gpt-preparation-final/`                  |

Existing ignored Rust tests and doctests remain ignored; these counts do not claim their execution. Fastly absolute-deadline and Worker deadline-runtime fixture commands were not independently rerun in this acceptance session.

The first Next.js build exposed unsupported direct Set iteration under the fixture's generated ES5 target. The adapter now iterates `Array.from(set)` without relaxing compiler settings. The first real-GPT preparation exposed locale-dependent shell sorting versus JS sorting of bundle names. Both launchers now use canonical `LC_ALL=C` sorting, with a regression covering `gpt.js` and `gpt_diagnostics.js`. Rebuilding and restamping preserved the exact production WASM bytes, SHA-256 `560ff7193bb00871ef97e607e046ae6aeb1a9672c02c488a202a76234b22309c`.

The first combined run passed 77 units, 109 offline registration cases, 18 legacy controls and 40 actual framework cases. Its measurement phase correctly failed because it expected the PR's new renderer-version marker on the original-main baseline and CSS-only reservation arm. Those arms must instead prove the original renderer through unchanged targeting, request count and actual nested creative visibility. Candidate arms retain the new version assertion. The corrected frozen run at `final-acceptance-corrected/` passed 78 units, the same 109 offline/18 legacy/40 framework cases, and all five 64-sample profiles. Those profiles contain 200 repeated measurement samples and 120 functional/control samples. The overall command exited 1 with a blocked real-GPT row, not success.

## Matched measurements and the unresolved regression

[Committed evidence](2026-11-02-854-cwv-measurements.json) retains all 200 individual measurement rows, exact conditions, input/report hashes, bytes and post-measurement byte-equivalence proof. Full observations, screenshots and traces remain in the local evidence directory. Each cell below is baseline to combined median; FCP and LCP were equal in this fixed article fixture.

| Condition              | Viewport  | FCP/LCP, ms | First visible ad estimate, ms |
| ---------------------- | --------- | ----------- | ----------------------------- |
| Normal cold            | 390x844   | 44 to 32    | 249.7 to 238.5                |
| Normal cold            | 1280x1000 | 36 to 36    | 243.8 to 237.6                |
| Normal warm            | 390x844   | 28 to 28    | 222.1 to 221.7                |
| Normal warm            | 1280x1000 | 32 to 32    | 222.9 to 222.4                |
| Throttled cold         | 390x844   | 936 to 792  | 1,333.5 to 1,565.5            |
| Throttled cold         | 1280x1000 | 944 to 812  | 1,305.4 to 1,571.0            |
| Throttled warm         | 390x844   | 348 to 756  | 550.0 to 1,008.3              |
| Throttled warm         | 1280x1000 | 356 to 764  | 563.7 to 1,018.1              |
| Controlled 750 ms hold | 390x844   | 780 to 32   | 995.8 to 974.9                |
| Controlled 750 ms hold | 1280x1000 | 784 to 36   | 990.6 to 966.0                |

Matching reservation changes controlled CLS from 0.207065 to zero at mobile width and 0.111133 to zero at desktop width. This does not erase the warm-cache regression. At throttled mobile width, all baseline paint samples were 312 to 352 ms and all combined samples were 752 to 764 ms. The loading-only arm reproduces the slowdown; reservation-only retains baseline-like timing.

Both baseline and candidate runtime requests were genuine HTTP cache hits with zero transferred runtime bytes in warm measurements. The document still grows from 38,728 to 128,941 raw bytes. The new compiled inline entry adds 69,512 bytes, and the provisional bootstrap grows from 36,756 to 57,260 bytes. That extra uncached document transfer explains the warm-cache result. This needs a loading/cacheability design change, not a weaker test or a favorable timing threshold.

The loopback server serves uncompressed HTTP. Its exact latency delta is not a prediction of production gzip/Brotli delivery. Estimated HTML gzip still grows from 10,018 to 36,499 bytes; estimated cold HTML-plus-runtime gzip grows from 40,336 to 54,938 bytes. The regression's exact production magnitude remains unverified, but its extra-inline-transfer mechanism is directly observed. Slower cold delivery also delays the first visible ad despite improving page paint.

## Final-state proof and retained failures

After the frozen run, the controller gained exact original-main revision/blob validation and stopped conflating functional success with issue closure. The live launcher gained running-outcome invalidation and interruption cleanup. These are separately checked orchestration/provenance changes, not new measurements.

A bounded TypeScript cleanup removes the PR's two source errors and new test diagnostics without relaxing compiler settings. Standalone `tsc --noEmit` still exits 2 with 315 existing diagnostics, compared with 321 on original main and 323 before cleanup. Exact file/code/message signature comparison finds no new or increased signatures. The focused affected Vitest run passes 289 tests, with 309 intentionally unselected; the earlier full JS result is reused for unchanged inputs.

Fresh production compilation proves all 14 JS modules and both trace assets byte-identical to the measured distribution. Fresh Rust exports prove every servable file byte-identical for both loading and combined outputs, 12 files each; only manifest metadata changes. Rebuilding production WASM preserves SHA-256 `560ff7193bb00871ef97e607e046ae6aeb1a9672c02c488a202a76234b22309c`. The native digest suite passes all 10 tests. These proofs justify reusing the existing browser observations rather than relabeling them as new runs.

The actual isolated Next.js provider HTTP interface also passes default win, forced no-bid transition, path-driven no-bid, reset, invalid PUT rejection and restored win checks, without opening a GPT page or contacting Google. Evidence: `provider-control-http.json` and its retained executable fixture command.

Final focused Node validation passes all 82 tests with zero skips, including stale-success invalidation, actual CLI SIGINT/SIGTERM/SIGKILL handling, ordinary discovery exclusion, canonical locale fingerprints and a post-spawn journal-write failure. The live launcher atomically marks a new invocation running before mutations, owns only its child process groups and uniquely named container, and unwinds cleanup before publishing failure. Review caught and corrected the journal-write error path; no exception is ignored to make the gate pass.

The final async launcher passes actual no-Google `--prepare` against the stamped original-main and candidate artifacts. Separate operational fault checks deliberately make the browser executable unavailable, so no page or Google request can start. A normal expected launch failure proves actual provider preflight, Viceroy exit and Docker removal. SIGINT and SIGTERM at the owned-runtime milestone prove actual Viceroy process exit, owned Docker removal and failed outcomes with exits 130 and 143. These are operational passes, not real-GPT delivery or original-main failure reproduction. SIGKILL remains uncatchable and can require manual resource cleanup; its persisted state cannot retain an old live success.

Evidence: `final-all-node.log`, `acceptance-boundary-final.log`, `type-only-final-eslint.log`, `final-servable-byte-proof.json`, `real-gpt-preparation-final/`, `real-gpt-no-browser-cleanup-proof.json`, `runtime-signal-proof.json` and their executable scratch commands. Independent read-only correctness reviews checked the real-GPT observations, provenance controller, type-only boundary and live-runner cleanup changes, with no remaining findings after corrections. Actual Google/GAM delivery remains unverified.

## Hosted fixture findings and focused follow-up

The first hosted JavaScript scan flagged four new fixture operations. The proxy now constructs connection hostname/port only from the server-created exact-origin map, never the incoming URL. Its upstream path remains request-derived and its response/cache semantics remain unchanged. Tests verify exact request method/path/host/body at the upstream separately from a fixed plain-text response, rather than reflecting request data. Real-GPT configuration assertions now parse the generated scalar/host-list values and compare exact endpoints instead of substring/hostname-regex checks. No suppressions or global allowlist changes are added.

All 82 Node checks pass after these changes. Four cold browser regressions and a 32-case warm run pass, including genuine cache reuse and the original-main comparison. Candidate and original-main emitted app configs are byte-identical before/after the assertion changes; production JS, HTML and WASM remain unchanged. Evidence: `codeql-fixture-units.log`, `codeql-cold-regressions/`, `codeql-warm-regressions/` and `codeql-fixture-proof.json`. The original five-condition timings are retained as historical executions, not relabeled as this follow-up. Hosted analysis is separate and must be checked on the pushed head.

Dedicated inventory must provide two distinct test ad-unit paths, known TS and ordinary creative IDs, and their visible markers. No customer inventory or guessed public sample placement substitutes for this setup. See the [real-GPT guide](../../../crates/trusted-server-integration-tests/browser/publisher-registration/real-gpt.README.md).

Do not add an automatic issue-closing keyword, mark the PR ready, or claim closure while a required functional gate is blocked/failed or the measured warm-cache regression remains unresolved. No deployment or publisher activation is part of this local test run.
