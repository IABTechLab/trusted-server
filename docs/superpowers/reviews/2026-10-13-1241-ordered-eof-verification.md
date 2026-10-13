# Ordered HTML EOF completion: implementation and verification

**Date:** 2026-10-13

**Branch:** `feat/1241-async-html-finalization`; draft PR #1290; issue #1241.

**Review baseline:** `3981d0222bf0767ba3c8de767d633018b0df4b58`.
The newer remote merge from `main` was preserved before final verification.

This report covers the revised [specification](../specs/2026-10-12-1241-async-html-finalization-design.md)
and [plan](../plans/2026-10-12-1241-async-html-finalization.md).
The October 12 reports describe the superseded buffered terminal API.

## Resulting design

Existing processors keep synchronous `process_chunk` calls. An optional borrowing
`async_finish` callback finishes a stage before downstream EOF. Accepted async output
passes through downstream ordinary processing. Factories declare capability and optional
output retention in the existing registration list; there is no separate production
finalizer lifecycle. `publisher.allow_async_html_finish` defaults to false and enabled
capability without permission is a configuration error.

Only declaring stages withhold output. Core owns the fallback and bounded candidate
outside each callback. Timeout, optional errors, poisoned candidates, late completion
and invalid assembly-marker counts retain the fallback allocation. One absolute budget
is shared; expiry skips later optional work while still finishing synchronous EOF.
Auction collection and encoding finish after the chain. Auction suffix storage is capped
before growth. Range/bodiless/unsupported response handling, actual status, template-cache
bypass, raw-origin readthrough and independent privacy policies remain covered.

The retention cap bounds withheld output and candidates. After overflow, the collector
becomes ordinary ready output; the existing `Vec<u8>` chunk interface can coalesce it with
the crossing chunk above that optional cap. Outer buffered-response limits still apply.

## Coverage and review

| Requirement                                               | Implementation and evidence                                                                                                                                                                                   |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Optional local future; unchanged synchronous entry points | `streaming_processor.rs`, `streaming_processor/finish.rs`: default absence, borrowing non-Send work and pre-effect synchronous rejection                                                                      |
| One ordered registration list and explicit permission     | `integrations/registry.rs`, `settings.rs`: default false, missing permission, permission alone and inconsistent retention metadata                                                                            |
| Stage-by-stage EOF and ready output                       | `html_processor/finish.rs`, publisher driver: downstream transforms async output before its own EOF; lazy materialized/streamed input releases prefix before pending work                                     |
| Cancellation and replacement acceptance                   | Leaf tests cover timeout borrowing/allocation identity, errors after partial writes, ignored poisoned writes, timer ties, late results, empty replacement and earlier accepted output surviving later failure |
| Retention and assembly boundaries                         | Exact-cap/crossing replay, creation/removal across streamed boundaries, retained markers, overflow bypass and accepted upstream replacement feeding downstream boundary validation                            |
| Auction, codec, cache and eligibility                     | Existing migrated publisher matrix, winning-bid/raw-origin controls, partial/status/flush regressions and fatal suffix overflow with exactly one abandonment summary                                          |
| Actual runtime behavior                                   | Fresh local Axum, Workers, Spin and Fastly disabled/success/hung/auction controls; required non-retaining Fastly early-byte control                                                                           |

Independent architecture and implementation reviews found and resolved constructor-time
deadline expiry, session validation before lazy reads, parser EOF failure classification,
remaining-budget timer initialization, timer-expiry propagation, reset forwarding and
missing acceptance tests. The final bidirectional document/source review approved the
expanded coverage. Review is separate from executed verification.

## Executed evidence

Observed red/green failures included missing callback/metadata/chain contracts, reset
state leakage, timer initialization and expiry propagation, constructor budget exhaustion,
and an undeclared async session consuming a body read before rejection. The final focused
`cargo test -p trusted-server-core --all-features async_eof` run passed 30 tests.

The newer main merge exposed a separate test-only compatibility failure: Cargo exports
`TRUSTED_SERVER__GIT_VERSION` to test processes, and the legacy configuration loader treated
it as application configuration. An explicit regression failed before filtering only that
metadata from the test-only environment source. Production configuration loading and
version-header behavior are unchanged. The final native all-feature run passed 2,986 unit
tests, 10 git-version filesystem tests, 10 template build-digest tests and 4 documentation
tests (5 existing ignored documentation examples).

Fresh JS build, formatting and type checks passed; Vitest passed 1,211 tests in 50 files.
Docs lint, formatting and build passed. Python fixture syntax, Bash syntax and all three
real-process cleanup tests passed.

### Runtime observations

All runs build fresh fixture artifacts, create isolated configuration, use loopback
services and reap their child processes. Disabled controls make no lookup; success runs
check exact output/privacy; hung runs retain exact fallback bytes; auction runs preserve
assembly. The fixture budget is 750 ms, distinct from the production default of 5,000 ms.

| Runtime | Success request | Hung request | Observed timeout | Auction request |
| ------- | --------------- | ------------ | ---------------- | --------------- |
| Axum    | 0.260 s         | 0.819 s      | 754 ms           | 0.232 s         |
| Workers | 0.230 s         | 0.822 s      | 750 ms           | 0.251 s         |
| Spin    | 0.224 s         | 0.805 s      | 751 ms           | 0.216 s         |
| Fastly  | 0.224 s         | 0.815 s      | 751 ms           | 0.213 s         |

Fastly's additional non-retaining control received its first body byte at **0.0053 s**,
before the 150 ms lookup completed; total request time was 0.217 s. The harness asserts
actual body-byte timing against the lookup-completion event, not response headers.
This proves the supported Fastly streaming path can release ready bytes before EOF I/O.
It does not promise arbitrary processors or buffered adapters have zero TTFB cost.

Commands: `scripts/async-html-finalization-local-test.sh axum --auction`, and the
`cloudflare`, `spin`, `fastly` variants with `--release --auction`.
Workers uses isolated `worker-build` 0.8.5 and `wasm-bindgen` 0.2.126 matching the lockfile.
Fastly timeout/streaming controls use isolated Viceroy 0.19 for backend timeout simulation;
ordinary WASI tests and template controls use the repository-pinned Viceroy 0.17.
Spin 4.0.0 passed the actual local controls; EdgeZero's CLI still warns that its verified
SQLite writer range is Spin 2–3. Repository runtime/tool pins were not changed.

## Evidence boundary

The final revised source snapshot (changed Rust/fixture/example files, sorted paths and
contents separated by NUL bytes) has SHA-256
`766c30f633421b259436a5d220ac8e52bf73308cedc81462cd4f8ece9269cdf8`.
Earlier pre-merge runs are not substituted for final evidence.

| Final local check                          | Result                                                                                                                           |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| All eight target-matched Clippy aliases    | Passed; Fastly repeated after the test-only metadata fix                                                                         |
| Core all-target/all-feature Clippy         | Passed                                                                                                                           |
| `cargo test-fastly` on pinned Viceroy 0.17 | Fastly 222, core 2978 (6 existing ignored), JS 10, git-version 10, OpenRTB 21, docs 4 (5 existing ignored) passed                |
| `cargo test-fastly-reuse`                  | 222 passed                                                                                                                       |
| Native adapter test aliases                | Axum 49, Workers 54, Spin 88 passed                                                                                              |
| Native core/digest gates                   | 2986 core, 10 git-version, 10 digest, 4 docs passed; digest lint passed                                                          |
| CLI helper and codegen                     | 892 executed CLI tests including selected ignored browser controls; codegen 7 passed                                             |
| Integration parity and lint                | 18 parity tests and all-target integration lint passed                                                                           |
| Exact CI integration selection             | Generator 8, docs fixture 1, integration 36, parity 18 passed; separate focused Fastly EC also passed                            |
| Existing template-cache controls           | ESI 22, inline 9 and purge 34 passed                                                                                             |
| JS, docs and formatting                    | Fresh build, 1211 tests/type checks, JS format, docs lint/build/format, outside-doc Markdown, Rust format and diff checks passed |
| Runtime/cleanup                            | All four actual runtimes plus Fastly early-byte proof passed; three cleanup tests and syntax checks passed                       |

Native checks used fresh JS bundles through the supported `TSJS_PREBUILT_DIR` setting.
The merged main change intentionally rejects the old `TSJS_SKIP_BUILD` setting. Real
runtime harnesses built their own private bundles. Template controls ran an external
copy of the unchanged harness with only worktree/shared-target artifact paths adjusted;
the first ESI attempt failed to locate its GPT artifact, and the final attempt corrected
that harness environment path. No assertions were relaxed or source tooling changed.
Hosted CI is separate from local verification. Maintainers still need to accept the API,
conservative processed-template cache policy and consumer-owned marker placement before
the PR leaves draft. No background signing service, external queue or publisher workflow
is required or introduced.
