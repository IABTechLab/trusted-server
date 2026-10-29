# Parser, paint and slot-space lab checks

This runner uses a loopback HTTP server, parser-created production script tags,
real Rust HTML injection and the exact JS bytes returned by the production static
route. GPT transport and bids are fictional. No customer, GAM or bidder request
is allowed. These are lab results, not field CWV or production fill evidence.

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

Four comparisons alternate forward/reverse order in fresh contexts at 390x844
and 1280x1000. Baseline and changed HTML remain untouched. The reservation-only
comparison is an explicitly recorded CSS-only isolation derivative: baseline
HTML and JS plus the exact production reservation style emitted in the changed
reserved document. It does not imply that old code understood the new config.
Every derived document and copied style has its own hash.

The server holds each first main-runtime response for 750ms. This makes parser
blocking observable without appending CPU loops to production JS. No CPU
throttling or external network shaping is applied. Script resource timing and
FCP must demonstrate the intended blocking/deferred ordering, but no arbitrary
millisecond improvement threshold decides whether measurements pass.

`--correctness-only` omits the baseline comparison and repeated timing samples.
CI uses this mode against freshly exported changed artifacts, then uploads all
measurement evidence. Full before/after comparison is an explicit local command
because CI does not reconstruct historical production builds.

For the review regressions alone, add `--regressions-only` instead. This runs
three focused cases: true renderer absence, deferred-runtime failure with the
early renderer retained, and enforced old-inline-hash/self CSP. Supply the frozen
`--baseline` export to derive hashes from genuinely pre-change inline script
text. Without it, CI explicitly records hashes from the current synchronous
legacy-inline output. Neither policy permits the new compiled early entry.
The exporter marks the CSP document with `with_enforced_csp(true)` and the runner
sends the matching enforced header. Fictional creative-inline hashes are also
included; the synchronous fallback must render without CSP violations.

## Observations and functional gates

- Pre-navigation buffered `PerformanceObserver` entries capture FCP, every LCP
  candidate, resources and raw layout shifts with source geometry.
- CLS is the largest session-window sum, excluding recent input, with one-second
  gaps and a five-second maximum window. Unit tests cover these boundaries.
- Navigation timing retains DOM interactive and DOMContentLoaded. Missing required
  observers, paint metrics or declared fixture cases fail the run.
- The transport fixture records exact native requests and request-time targeting.
  It installs before production bootstrap and does not deduplicate TS requests.
- Real Universal Creative renders the server bid in an iframe. Assertions inspect
  actual nested creative DOM, not just a render event. First-visible time is
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

`artifacts.json` retains exact inputs, provenance, browser/Node versions, runner
hashes, settings and interventions. `samples.json` retains all raw observations
and per-sample geometry, native events and network calls. `summary.json` reports
sample count, median, minimum and maximum for each metric, with repeated
measurement samples kept separate from functional cases. `comparisons.json`
reports signed median deltas and percentages where the baseline is nonzero.
Raw/gzip/Brotli sizes cover HTML, individual inline tags, all inline JS and the
remaining fetched runtime. Screenshots and traces include every sample; failures
also retain observer/native state when available.

A fast first paint can coexist with later ad startup because the retained
bootstrap scheduler waits for page load. Report both, including regressions.
The original GPT bootstrap remains inline; this work does not claim to remove
its bytes or synchronous execution. Real-GPT staging and live publisher
activation require separate approval.
