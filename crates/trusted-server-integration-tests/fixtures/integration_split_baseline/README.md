# Integrations extraction baseline

These are observed schema-one outputs from corrected stack tip
`ca95a2dacb7fac1a47d7504ec0cc8d0dad8539bf`, before production extraction edits.
`capture-provenance.json` records the source main commit, separate Cookie,
mediator and URL-ownership fixes, toolchain, EdgeZero tag/revision, fixture hashes
and every output hash. The source config and credentials are fictional.

No expected runtime output was hand-authored. Two independent captures produced
identical bytes for all eleven observations and the separate artifact inventory.
The harness also mutates each observation and executes the same comparison to
prove that its golden detects a changed output.

## Checks

Run the behavior checks with a freshly built candidate CLI:

```sh
scripts/check-integration-split-baseline.sh
```

This runs the runtime observations, all golden mutation checks, 32 fresh source
parses for envelope identity, and eight CLI read-model cases. The CLI test is
ignored in an ordinary package test because it requires that fresh executable.
The runner selects it explicitly. It never rewrites goldens.

Check public exports separately:

```sh
node --test scripts/integration-api/*.test.mjs
scripts/check-integration-api-delta.sh --all
```

The export check covers default and `test-utils` builds for native Linux,
WASI and Worker targets. `api/provenance.json` pins the historical compiler
inputs and six snapshots. The empty initial delta ledger permits no new,
removed or changed exports. Future changes need exact reviewed entries in
`scripts/integration-api/delta-manifest.json`; the three classes and tool limits
are documented beside that checker. Normalizer schema 2 reproduces the same
historical raw captures while excluding compiler `CfgTrace` source locations;
it does not rebaseline candidate exports.

To capture outputs for investigation, use a scratch directory and a freshly
built CLI. Review the diff before copying any bytes into this directory:

```sh
INTEGRATION_SPLIT_BASELINE_TS=/absolute/path/to/candidate/ts \
INTEGRATION_SPLIT_CAPTURE_DIR=/tmp/integration-split-capture \
cargo test -p trusted-server-integration-tests --test integration_split_baseline \
  capture_corrected_baseline -- --ignored --exact
```

Capture is excluded from CI's `--include-ignored` selection. CI must not bless
its own candidate output. The dedicated runner also excludes capture.

## Observations

| File                       | Observed behavior                                                                                                                                                                                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `registry.json`            | All 14 registered capability owners, mediator selection, six globally interleaved APS/PBS/standard providers, route tables, module order, deferred Prebid, trusted tag attributes and envelope data SHA                              |
| `settings.json`            | Complete serialized schema-one configuration                                                                                                                                                                                         |
| `attributes.json`          | Actual registry chaining over 95 shared URL inputs for script and link attributes                                                                                                                                                    |
| `request-decisions.json`   | Enabled/disabled GPT diagnostics, activation/deactivation, duplicate directives/cookies, malformed-byte neighbors, sanitized requests, bootstraps and response cache privacy                                                         |
| `trusted-attributes.json`  | Positive GAM attribution, APS publisher-native mode and their combined trusted publisher tag                                                                                                                                         |
| `document.html`            | Combined integration head insertion, managed/blocked assets, source rewriting and Next.js data                                                                                                                                       |
| `diagnostics.html`         | Conditional standalone diagnostics delivery                                                                                                                                                                                          |
| `suppressed-datadome.html` | Request-only client-tag suppression                                                                                                                                                                                                  |
| `adapter-runtime.json`     | Twelve actual Axum/Cloudflare/Spin responses covering buffered/chunked Next.js/GTM composition and the split-RSC plus auction path from #1135                                                                                        |
| `auction-runtime.json`     | Twenty-four production auction executions, all six provider wire requests, reverse network completion, plan/response/mediator-input order, four mediator HTTP failures, successful no-bid/winner and floors through live/split paths |
| `cli-views.json`           | Actual ad-template lint, match, explain and successful/failing check output                                                                                                                                                          |

`artifacts.json` records the hashes and sizes of all 13 embedded production
IIFEs separately. Artifact hash changes are not silently treated as behavior
changes or used to replace executable behavior checks.

## Narrow normalization

- Generated browser `?v=` hashes become named markers. The actual URL path,
  query key, placement and attributes remain in the HTML observation.
- Provider root `tmax` stays an integer within its configured ceiling, then
  takes that ceiling in the observation. The raw wire string changes only that
  one numeric property, preserving other bytes, field order and escaping.
- Provider/mediator elapsed milliseconds and total auction milliseconds must be
  unsigned and within budget, then become zero. Other metadata is retained.
- CLI output replaces only the known absolute fixture source path. Color and
  application environment overlays are disabled explicitly.
- JSON observation objects are sorted for reproducibility. Arrays retain their
  order. The separate raw wire strings preserve serialization order.

The auction fixture uses real Axum backend naming and deterministic HTTP
responses, with no network/store access. The existing Next.js service fixture
has one fixed backend name, so it is reused unchanged for routed observations
but cannot represent six concurrent providers. The local auction fixture
supplies distinct names instead of changing code under the frozen baseline.

## Recorded proof and limits

At this checkpoint, the behavior runner passes four tests, the API script suite
passes 27 tests, and all six fresh compiler views match their snapshots. Actual
core source canaries for an added export and a removed export both fail the
checker. Restoring the source passes again. Eleven output goldens reject deliberate
mutations. The config fixture has two context allowlist values; existing core
32-parse tests additionally compare template fingerprints and collection-rich
envelope identities.

The corrected prerequisite stack also passed 3,355 native core tests with two
existing ignored tests, 2,125 browser tests with one existing skip, all 13 IIFE
builds, 29 host parity tests, ten build-digest tests, core clippy and focused
Viceroy ownership/Lockr tests. Those results are reused while their production
inputs remain unchanged.

This harness uses native adapters with deterministic upstream services. It does
not prove real-network deadlines, gzip parity, real-browser behavior, or a full
Fastly/all-adapter gate sweep. Those checks remain separate. No operator-TOML
compatibility path or second integration inventory is introduced here.
