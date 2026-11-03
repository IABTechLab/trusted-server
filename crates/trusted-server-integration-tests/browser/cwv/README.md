# Parser, paint and slot-space lab checks

This runner uses a loopback HTTP server, parser-created production script tags,
real Rust HTML injection and the exact JS bytes returned by the production static
route. GPT transport and bids are fictional. No customer, GAM or bidder request
is allowed. These are lab results, not field CWV or production fill evidence.

The default, correctness-only and regressions-only modes retain the earlier
inline/deferred loading contract. They reject the current synchronous-unified
candidate until its architecture and outage boundary are accepted and the full
consumers are migrated. Use the explicit diagnostic scopes below for the limited
candidate experiments, not as a substitute for full acceptance.

## Export

From the repository root, with current production JS built by the coordinator:

```sh
HOST_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
TSJS_PREBUILT_DIR=/absolute/current-dist \
  cargo run -p trusted-server-integration-tests --example cwv_html \
  --target "$HOST_TARGET" -- /tmp/cwv-loading unreserved
TSJS_PREBUILT_DIR=/absolute/current-dist \
  cargo run -p trusted-server-integration-tests --example cwv_html \
  --target "$HOST_TARGET" -- /tmp/cwv-combined reserved
```

The exporter writes inline/shared documents, responsive and unknown-height
creatives, unmatched paths, absent wrappers and child-replacement controls. The
manifest contains source revision/status, source fingerprints, the exact exporter
source, selected configuration, toolchain and the production URL-to-asset map.
The runner snapshots and hashes every input before launching Chromium.

For an archived pre-change source tree, copy this example and the small
`test-utils` `publisher::fixture_ad_scripts` bridge, both of which call existing
production builders. Omit `.with_request_path(path)` and `.with_enforced_csp(name == "csp")`,
which did not exist in the archived source. Baseline output already uses the
original synchronous loading path. Use the archived source's frozen JS and `unreserved`.
Record these fixture-only source differences. Archive exports can supply
`CWV_SOURCE_REVISION` and `CWV_SOURCE_STATUS`; the manifest explicitly records
these environment overrides instead of presenting them as Git observations.

Do not substitute current generated head snippets for baseline snippets. Baseline
production code must generate baseline HTML and serve baseline JS.

## Run

Install the browser package's existing dependencies and its matching Playwright
Chromium first. The runner itself performs no installs or builds.

```sh
node --test crates/trusted-server-integration-tests/browser/cwv/*.test.cjs
node crates/trusted-server-integration-tests/browser/cwv/run.cjs \
  --baseline /tmp/cwv-baseline \
  --loading /tmp/cwv-loading \
  --combined /tmp/cwv-combined \
  --runs 5 --out /tmp/cwv-evidence
```

Four arms alternate forward/reverse order at 390x844 and 1280x1000. Each sample
starts in a fresh context, including warm samples before priming. The arms are:

- `baseline`: frozen pre-change production HTML and JS, without reservation.
- `loading`: changed production HTML and JS, without reservation.
- `reservation`: baseline HTML and JS plus the exact production reservation style
  from the changed reserved document. This CSS-only isolation derivative does not
  imply that old code understood the new config.
- `combined`: changed production HTML and JS with reservation.

Baseline and changed measurement HTML remain untouched. Every isolation derivative
and copied style has its own hash. All arms use the same selected conditions.

| Option                        | Default | Meaning                                                                                                                                                        |
| ----------------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--runtime-delay-ms NUMBER`   | `750`   | Hold the first main-runtime HTTP response per navigation. `0` is valid. Cache hits do not contact the server and therefore have no hold. Retries are not held. |
| `--cpu-rate NUMBER`           | `1`     | Chromium CDP CPU slowdown rate, at least 1.                                                                                                                    |
| `--network-latency-ms NUMBER` | `0`     | CDP request latency in milliseconds.                                                                                                                           |
| `--download-kbps NUMBER`      | `0`     | CDP download rate in decimal kilobits/second. `0` means unlimited.                                                                                             |
| `--upload-kbps NUMBER`        | `0`     | CDP upload rate in decimal kilobits/second. `0` means unlimited.                                                                                               |
| `--cache cold\|warm`          | `cold`  | Cold disables HTTP cache and serves no-store responses. Warm primes then navigates again in the same page/context.                                             |

Numeric options must be finite and nonnegative except CPU rate, which must be at
least 1, and runs, which must be a positive safe integer. Runtime delay must also
fit Node's timer range. Invalid options fail before browser startup.

Warm responses cache successful exported JS, the hero image and the creative JS
for one hour. HTML, creative documents and failures remain no-store. Priming uses
the exact sample document, runtime-failure mode, viewport and selected conditions.
It waits for delivery when matched and retains its own observer/network evidence.
The measured navigation must show a native CDP HTTP-cache hit. A successful runtime
must itself hit cache. Service workers are blocked and do not count as HTTP cache.
No Playwright routes are installed because routing disables Chromium HTTP cache.
An HTTP proxy forwards only the exact publisher and creative fixture origins,
preserving response headers and browser HTTP caching. It rejects every other
origin and all CONNECT tunnels. It does not depend on browser loopback-bypass
settings. Unexpected URLs still fail assertions. The origin allowlist and proxy
requests are retained in `artifacts.json`.

For example, run the ordinary response with warm cache and explicit throttling:

```sh
node crates/trusted-server-integration-tests/browser/cwv/run.cjs \
  --baseline /tmp/cwv-baseline --loading /tmp/cwv-loading \
  --combined /tmp/cwv-combined --out /tmp/cwv-warm \
  --runtime-delay-ms 0 --cache warm --cpu-rate 4 \
  --network-latency-ms 150 --download-kbps 1600 --upload-kbps 750
```

Default cold runs retain the existing 750ms held-response FCP check. Native
captured script-load events also check whether the parser has reached the article:
blocking runtime must load before it, deferred runtime after it. This ordering
check does not depend on a slow response. A normal 0ms response or a warm cache hit
may legitimately finish before FCP. No timing-improvement threshold decides whether
measurements pass. CPU and network shaping apply to all fixture resources, not
just the runtime, and shared-host contention can still affect results.

`--correctness-only` omits the baseline comparison and repeated timing samples.
CI uses this mode against freshly exported changed artifacts, then uploads all
measurement evidence. Full before/after comparison is an explicit local command
because CI does not reconstruct historical production builds.

For the review regressions alone, add `--regressions-only` instead. This runs
four focused cases: true renderer absence, deferred-runtime failure with the
early renderer retained, successful enforced old-inline-hash/self CSP, and failure
of the synchronous runtime under that same CSP. Supply the frozen
`--baseline` export to derive hashes from genuinely pre-change inline script
text. Without it, CI explicitly records hashes from the current synchronous
legacy-inline output. Neither policy permits the new compiled early entry.
The exporter marks the CSP document with `with_enforced_csp(true)` and the runner
sends the matching enforced header. Fictional creative-inline hashes are also
included; the synchronous fallback must render without CSP violations. The normal
CSP case also checks every compiled core API, registration/scheduling APIs, shim
hook, and protocol/renderer versions with neither raw nor compiled early inline
bootstrap present. This is a reproducible check of the exact runtime bytes loaded
by the browser, not just a separately evaluated source module.

The failed synchronous-runtime case establishes a different boundary. It requires
core, shim and renderer APIs to remain unavailable, zero native requests and no TS
delivery, while still having no blocked inline scripts. The existing core-dependent
publisher probe must produce exactly its declared `tsjs.setConfig is not a function`
error; no other page/network errors are accepted. This case does not claim a
provisional fallback exists or that failed-runtime behavior matches normal CSP
delivery. It remains a separate sample and never relaxes the successful CSP gate.

Full mode has `8 * runs + 24` samples, correctness-only has 26, and regressions-only
has 4. The completion schema is unchanged.

## Diagnostic synchronous-unified screen

Use `--diagnostic-screen` only for the first identity-transport comparison of the
cacheable synchronous-unified candidate. It is not combined acceptance and must
not be passed to the full completion controller as a replacement for its required
scope. Existing full, correctness-only and regressions-only modes retain their
inline/deferred expectations for old artifacts. They are not migrated here.

```sh
for CACHE in cold warm; do
  node crates/trusted-server-integration-tests/browser/cwv/run.cjs \
    --diagnostic-screen --baseline /tmp/cwv-baseline \
    --loading /tmp/cwv-loading --combined /tmp/cwv-combined \
    --runs 5 --out "/tmp/cwv-diagnostic-$CACHE" \
    --runtime-delay-ms 0 --cache "$CACHE" --cpu-rate 4 \
    --network-latency-ms 100 --download-kbps 1600 --upload-kbps 750
done
```

The baseline is mandatory and its manifest must identify pinned original main
`51730bdaf841e4d7bb96c2258e5e98169bdff9a7`, not the failing inline/deferred PR
snapshot. Keep genuine baseline production HTML and served bytes. Candidate
exports must come from the production builders, not rewritten head snippets.

The expected architecture is fixed independently of observed HTML. Every arm
must use one synchronous classic unified runtime. Candidate arms require a
versioned URL, GPT configuration before it, no `#trustedserver-bootstrap` and no
raw provisional GPT bootstrap. Native runtime load events must observe parser
blocking through `assertRuntimeOrdering`; the test does not treat that as safe
deferral. Warm samples must prove actual runtime HTTP-cache reuse through
`assertCacheUse`. Identity responses, fail-closed proxy, per-sample contexts,
priming, observers and raw timing/byte evidence are unchanged. No Playwright
routes or response codecs are added.

The screen runs exactly `8 * runs` measurement samples, 40 with this command.
It alternates baseline/loading/reservation/combined forward and reverse at
390x844 and 1280x1000. Each invocation selects one cache condition, so the pair
above records 80 samples. There are no skipped or optional cases within that
scope. The existing inline fixture checks direct early core calls, one initial
native request, candidate targeting and visible Server creative. The screen also
queues a test-only callback before production core executes. Evidence captures
core methods and a real `getConfig()` call when that queue drains, then captures
GPT registration, scheduling, shim and renderer APIs at the native runtime load.
The core callback runs before the GPT IIFE, so it does not require those later
APIs there. This init-script probe does not alter the exported HTML or runtime.

`acceptance.json.scope` is `diagnostic-screen`, with a declared expected sample
count and `diagnostic.fullAcceptancePassed=false` even if this limited scope
passes. Both existing negative-control entries are `not-requested` with explicit
reasons. Removing `defer` would be vacuous for this candidate; the reservation
negative control and broader correctness suites are outside the first screen.
The screen retains successful-arm reservation geometry and unreserved CLS gates.
A zero or incomplete case set fails. A failed rerun still invalidates the previous
outcome before dependencies or exports are read.

CSP and critical-runtime outages, publisher registration, framework lifecycle,
full negative controls, deferred-runtime correctness, production compressed
magnitude and real-GPT acceptance are not established by this screen. Missing
authorized Google inventory remains blocked outside this diagnostic scope.
Synchronous loading and loss of the inline critical-script fallback remain
architecture decisions, not contracts waived by a diagnostic pass. Inspect paint
and ad-startup regressions before broadening coverage. There is no newly invented
noise tolerance or timing-improvement threshold.

## Diagnostic critical-runtime outages

After the measurement screen, `--diagnostic-outages` observes four focused
availability cases at 390x844 using the reserved candidate export. It is mutually
exclusive with every other scope. It does not change the full-mode deferred-runtime
failure contract or approve loss of the inline fallback.

```sh
node crates/trusted-server-integration-tests/browser/cwv/run.cjs \
  --diagnostic-outages --baseline /tmp/cwv-baseline \
  --loading /tmp/cwv-loading --combined /tmp/cwv-combined \
  --out /tmp/cwv-diagnostic-outages --runs 1 \
  --runtime-delay-ms 0 --cache cold --cpu-rate 1 \
  --network-latency-ms 0 --download-kbps 0 --upload-kbps 0
```

The exact cases are normal success, normal critical-unified HTTP 404, enforced
old-inline-hash/self CSP success, and the same CSP with critical-unified HTTP 404.
There are always four cases, independent of `--runs`, as in regressions-only mode.
Baseline remains pinned and supplies the genuine old inline hashes. Production
HTML and runtime bytes are unchanged. Native observers, cache configuration,
fail-closed proxy and failure artifacts use the existing sample machinery.

Successful cases require synchronous native runtime ordering, the pre-core queue,
full runtime APIs, one targeted initial request and visible Server creative.
Failed cases separately record normal and enforced-CSP behavior. They require
all core/GPT/shim/renderer APIs unavailable, an undrained pre-core queue, no
successful runtime load, exactly one runtime 404 without retry, zero native
requests/delivery/visible ad, and only the existing declared
`tsjs.setConfig is not a function` publisher error. DOMContentLoaded and article
geometry must establish that parsing continued after failure. Enforced-CSP cases
also require zero CSP violations. No late recovery or retry is claimed.

Completion uses `scope=diagnostic-outages`, an exact four-case completeness gate,
`diagnostic.fullAcceptancePassed=false` and
`diagnostic.changedOutageBehaviorApproved=false`. Each sample records its policy,
expected availability and the explicit changed-outage-not-approval interpretation.
Both existing negative controls are `not-requested` with a reason. A successful
diagnostic means these observations matched the declared failure boundary, not
that the changed outage boundary is acceptable for adoption or issue closure.
The public full-acceptance verifier rejects both diagnostic scopes.

Keep a separate output directory. Prior screen measurements remain evidence for
their frozen inputs and recorded tooling hashes; this diagnostic does not rerun,
replace or relabel those 80 samples. Full correctness, registration/framework,
controller and live gates remain outside this scope. Missing authorized Google
inventory stays blocked. Start with zero delay; a separately recorded held-response
run can be requested later if needed.

## Full-mode observations and functional gates

The following describes the retained full modes. Diagnostic scopes run only the
cases and observations declared in their sections above.

- Pre-navigation buffered `PerformanceObserver` entries capture FCP, every LCP
  candidate, resources and raw layout shifts with source geometry.
- CLS is the largest session-window sum, excluding recent input, with one-second
  gaps and a five-second maximum window. Unit tests cover these boundaries.
- Navigation timing retains DOM interactive and DOMContentLoaded. Native script
  load events retain parser/article state. Missing required observers, paint
  metrics, runtime load evidence or declared fixture cases fail the run.
- Except in `--regressions-only`, two negative controls derive from the reserved
  changed document. Removing only the runtime's `defer` attribute must reproduce
  parser blocking through native load/parser observations and FCP/resource timing.
  Removing only the production reservation styles must produce article movement
  and positive observer-reported CLS with a source inside the article. These are
  separate correctness samples, never mixed into the four measurement arms. Their
  exact HTML and hashes are retained. These controls execute only when the browser
  runner runs; the focused Node tests alone do not establish browser behavior.
- The transport fixture records exact native requests and request-time targeting.
  It installs before production bootstrap and does not deduplicate TS requests.
- Real Universal Creative renders the server bid in an iframe. Assertions inspect
  actual nested creative DOM, not just a render event. Original-main baseline and
  CSS-only reservation arms predate the new renderer protocol marker; they must
  still pass actual Server creative, candidate-targeting and one-request checks.
  Candidate arms additionally require renderer protocol version 1. First-visible time is
  named `firstVisibleEstimateMs`: it combines creative-ready, visible frame
  bounds and an animation-frame barrier, not hardware paint or a GAM impression.
- Compatible reservation must preserve article geometry and zero controlled
  ad-induced CLS. No-fill retains space. Boundary widths 1023/1024 exercise the
  mobile-first rule. Oversize and unknown-height creatives do not get a promise
  of zero shift.
- Slow, failed, absent and late-retried runtime cases cover both automatic and
  publisher-registered slots. Registration starts with explicitly disabled GPT
  initial loading and checks the publisher's exact native object and targeting.
  The early fallback and takeover must keep one initial request. The absent-script case is another explicit
  hashed document intervention. Late retry uses a normal DOM script element
  after the parser-created runtime tag has failed and the fallback has rendered.
- True renderer absence removes the compiled early tag, blocks standalone GPT
  and the original unified route, and retains the raw protocol bootstrap.
  Its fixture-only direct-core probe is removed because core is intentionally
  unavailable. Registered delivery must use ordinary publisher fill without
  candidate targeting, while retaining the admitted candidate data. This is
  separate from a failed deferred runtime, which must keep the real early bridge
  and candidate delivery.
- Inline/shared payloads, direct early core APIs, publisher queues, missing
  wrappers, nonmatching paths and replacement of the inner ad element are hard
  functional checks. The child-replacement fixture is not a React hydration
  proof; the existing real Next.js publisher-registration gate covers that.

## Evidence

Export input filenames remain `manifest.json`, the declared HTML/assets and
`app.toml`. No new export format is required.

`acceptance.json` is the machine-readable completion record, also mirrored as
`artifacts.json.outcome`. Its `schemaVersion` is 1. `status` is `running`, `passed`,
`failed` for an assertion/runtime failure, or `blocked` for a missing export,
dependency or Chromium executable. `passed` means the selected scope completed,
including report writes and browser/server cleanup. The scope is
`measurement-and-correctness`, `correctness`, `regressions`, or the limited
`diagnostic-screen` and `diagnostic-outages` scopes described above. It does not certify
any live publisher or real-GPT behavior. `conditions`, `samples`, `profileSamples`,
timestamps and an error when unsuccessful describe the run.

`negativeControls` has `parser-blocking` and `reservation-removed` entries. Each
entry has a `status` of `pending`, `running`, `passed`, `failed`, `blocked` or
`not-requested`; completed controls include `sampleIndex` and `htmlFile`. A
regressions-only run records `not-requested` and the reason explicitly. Validate
both the overall status and the scope/control statuses when consuming evidence.
The runner initializes these outcomes before reading exports, so a failed rerun
cannot retain an earlier passed outcome. Malformed CLI arguments fail before
an output directory is established and do not produce a completion record.

`artifacts.json` retains exact inputs, provenance, browser/Node versions, runner
and helper hashes, normalized conditions, cache semantics and interventions.
`samples.json` retains all raw observations and per-sample geometry, native events,
CDP requests/cache-hit flags, server responses with applied delay, network calls
and separate warmup evidence. Each sample records the selected conditions and any
negative control. `summary.json` reports
sample count, median, minimum and maximum for each metric, with repeated
measurement samples kept separate from functional cases. `comparisons.json`
reports signed median deltas and percentages where the baseline is nonzero.
Raw/gzip/Brotli sizes cover HTML, individual inline tags, all inline JS and the
remaining fetched runtime. Screenshots and traces include every sample; failures
also retain observer/native state, server responses and available cache evidence.
Use a different `--out` directory for each condition set. The runner records one
selected condition set per invocation, not an implicit device/network matrix.

A fast first paint can coexist with later ad startup because the retained
bootstrap scheduler waits for page load. Report both, including regressions.
The earlier full-mode exports retain the original inline GPT bootstrap. The
synchronous candidate omits it and has the separately documented critical-script
outage boundary. Neither diagnostic scope approves that change or proves full
acceptance. Real-GPT staging and live publisher activation require separate approval.
