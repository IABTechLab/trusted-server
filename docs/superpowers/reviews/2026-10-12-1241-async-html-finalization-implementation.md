# Asynchronous HTML finalization implementation review

**Date:** 2026-10-12

**Branch:** `feat/1241-async-html-finalization`

**Planning commit:** `b750c879a`

**Status:** Implemented and reviewed. Repository regression gates pass. The actual
Workers enabled-auction control remains blocked by the existing clock panic in #1240.

**Design:** [Optional asynchronous HTML document finalization](../specs/2026-10-12-1241-async-html-finalization-design.md)

**Plan:** [Implementation and verification tasks](../plans/2026-10-12-1241-async-html-finalization.md)

## Result and scope

Trusted Server now offers an explicitly registered HTML terminal callback after
synchronous processing reaches EOF. It supports borrowing, non-`Send` futures,
ordered atomic replacements, bounded core-owned output, one chain deadline, and
unchanged-input fallback. The generic streaming traits remain unchanged.

The callback belongs in Trusted Server because its document state, auction assembly,
cache admission, privacy policy, and runtime service container belong here. No
EdgeZero revision, production platform manifest, JS behavior, KV implementation,
signing consumer, or existing clock behavior changes in this delivery.

Enabled finalizers deliberately retain the completed transformed document. They
bypass shared-template caching before lookup or reservation, and eligible HTML
responses use `private, no-store`. Disabled registrations keep the existing route.
Raw origin readthrough caching remains independent.

## Spec and plan coverage

The original granular checklists describe instructions, rather than claiming each
suggested intermediate commit exists. All thirteen tasks have implementation or
verification evidence below. Task 11's Workers auction control has an explicit
external blocker; passing native parity is not substituted for that live check.

| Plan tasks | Spec sections             | Implementation and observed evidence                                                                                                                                                                                                                                                                                                                                                                         |
| ---------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1          | 6.2, 8, 10                | `integrations/html_finalization.rs` defines the factory, session, borrowed input, outcomes, errors, deadline, and bounded replacement writer. Tests cover exact limits, poisoned candidates, empty replacement, unchanged output, and allocation bounds.                                                                                                                                                     |
| 2          | 9.1–9.3                   | Settings default to 5000 ms and validate 1–60000 inclusive. Runtime services default to no timer and preserve configured timers when cloned or replacing services. Explicit TOML omission and boundary tests pass.                                                                                                                                                                                           |
| 3          | 6.1, 7.1                  | Registry metadata preserves registration order and disabled eligibility. HTML parser, synchronous processors, and finalizer factories share one request-local document state. State-sharing and request-isolation tests pass.                                                                                                                                                                                |
| 4          | 9.2                       | Axum installs Tokio sleep, Workers installs `worker::Delay`, and Spin installs SDK sleep. Fastly has no cooperative timer. Target-matched native/WASM compilation, fixture-enabled WASM lint, and live runtime checks pass.                                                                                                                                                                                  |
| 5          | 7.2, 7.4, 9.3, 10, 13     | The private coordinator executes yielding callbacks in order. Tests verify earlier commits survive later failure, one timer covers the chain, expiry wins ties, late results are rejected, and cancellation drops active borrows before owned-byte fallback.                                                                                                                                                 |
| 6          | 8                         | Collection and candidate buffers use the configured body cap with checked length arithmetic. Overflow replays prefix, crossing chunk, and suffix once, without starting optional I/O. Exact byte-order and allocation-identity tests pass.                                                                                                                                                                   |
| 7          | 11                        | Registry metadata gates shared-template lookup before origin fetch. Tests verify warm/cold bypass, no reservation/store, preserved raw-origin readthrough intent, a working raw-byte cache double hit with per-request finalization, disabled eligibility, four conditional/range headers stripped on eligible GETs, preserved preconditions, and private headers without an auction.                        |
| 8          | 7.2–7.4, 12, 13           | One terminal driver covers materialized/streamed input and lazy/buffered output, with and without an auction. Decoder and synchronous EOF precede callbacks; seam handling and encoding follow them. Client cancellation verifies exactly one existing guard warning; fatal read/decode/write/flush errors verify exactly one abandonment telemetry summary.                                                 |
| 9          | 7.1–7.3, 10–12, 14.3–14.5 | The 128-case route matrix covers four document shapes, four codecs, auction presence, sink kind, and body kind. Corrupt/truncated codec tests prevent premature I/O. Tests cover unsupported/bodiless/non-HTML cases and eligible non-2xx HTML. A positive winning-bid test preserves the article, places one bid injection before the structural body end, and removes the seam. Native and WASI runs pass. |
| 10         | 6.1, 12, 14               | The fictional consumer is behind `test-utils`, contributes no JS, waits for completed HTML, and uses request HTTP services. Axum/Workers/Spin host parity passes. Fixture IDs are absent from fresh ordinary artifacts for all four adapters.                                                                                                                                                                |
| 11         | 9.2–9.3, 14.6             | The local harness builds fresh artifacts and verifies disabled, delayed-success, and hung-lookup controls on all four actual runtimes. Workers/Spin/Fastly release-profile runs pass. Actual auction fallback controls pass on Axum, Spin, and Fastly; Workers reproduces #1240.                                                                                                                             |
| 12         | 9–12, 15                  | Canonical configuration and integration guides document registration, timeout semantics, buffer limits, cache/privacy effects, synchronous helper limitations, required registry metadata, and local runtime commands.                                                                                                                                                                                       |
| 13         | 14–16                     | Full target-matched gates, actual-runtime controls, existing integration/browser checks, feature-off artifact inspection, and independent architecture/final reviews are recorded below.                                                                                                                                                                                                                     |

Every production addition maps to an existing spec contract. The implementation
does not add an interleaved asynchronous streaming API or cacheable finalizer mode.
The timeout starts after synchronous EOF, and optional failure preserves bytes
entering the failing stage. Existing transport, parser, codec, and sink failures
retain fatal handling. Marker-count checks protect opaque seams; they do not claim
to validate arbitrary HTML structure or integration-specific semantics.

## Necessary implementation refinements

- `AuctionDispatch` requires the request integration registry. Cache admission
  needs finalizer metadata before lookup, and an optional registry could incorrectly
  admit a cached template. Callers must pass the same registry to dispatch and
  finalization; all production adapters do so.
- Materialized deflate input uses a validated zlib reader. The prior convenience
  decoder could accept a truncated trailer as EOF; the opted-in route must verify
  completion before starting external I/O. Tests reject truncated trailers while
  preserving the existing materialized-body limit semantics.
- The opt-in driver and terminal coordinator futures are boxed. The existing
  authorized cold-template buffered fallback is also boxed to limit nested WASI
  future stack size. An existing winning-bid cold-miss test trapped on the new
  branch, passed on the clean planning baseline, and passed after this narrow fix.
  It costs one allocation on already-buffered authorized cache misses; inline
  delivery and warm-template delivery do not take that fallback.
- The runtime harness uses a Python process-group driver with generated temporary
  manifests and a restricted environment. It handles chunked Spin HTTP requests,
  validates transport bounds, and retains body/header/event/timing diagnostics.
  Fixture-only Workers/Spin loggers provide deadline diagnostics without altering
  ordinary production logging. The Workers fixture also installs a diagnostic
  panic hook under the same test feature.
- Verification used one reviewed implementation change instead of manufacturing
  the plan's suggested per-task commits. No implementation API was published
  between contract creation and complete routing.

## Repository verification

All commands ran against the implementation worktree, with Rust 1.95.0 and Node
24.12.0. Target-matched checks used the workspace aliases; shared Cargo cache use
does not substitute an old artifact because fresh builds and artifact hashes were
recorded. The normal Fastly gate used the pinned Viceroy 0.17.

| Check                           | Observed result                                                                                                                                           |
| ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Eight Clippy aliases            | Pass: Fastly, Axum, Workers native/WASM, Spin native/WASM, CLI, and codegen.                                                                              |
| Core primary-package Clippy     | All targets/all features and the native build-digest lint pass.                                                                                           |
| Fastly tests                    | 219 adapter tests, 2924 core tests, 3 JS crate tests, 21 OpenRTB tests, and 4 core doc tests pass; existing ignored tests remain ignored.                 |
| Fastly reusable sandbox         | 219 adapter tests pass.                                                                                                                                   |
| Native adapter tests            | Axum 44, Workers 53, Spin 87 pass.                                                                                                                        |
| Core all-feature tests          | 2931 unit tests, 10 digest tests, and 4 doc tests pass; 6 doc tests remain ignored.                                                                       |
| Host parity                     | 18 tests pass.                                                                                                                                            |
| Exact CI Rust runtime selection | 63 tests pass across configuration generation, documentation fixture, integration tests, and parity.                                                      |
| CLI and codegen                 | 875 CLI tests and 7 codegen tests pass; existing ignored CLI tests remain ignored.                                                                        |
| JS build, Vitest, type checks   | 50 test files / 1208 tests pass.                                                                                                                          |
| Browser controls                | Next.js: 21 pass / 1 expected skip. WordPress: 11 pass / 11 expected skips. Initial-render ownership: 10 scenarios pass. Escaping fixtures: 2 tests pass. |
| Documentation                   | Lint, build, formatting, and Markdown checks pass. Core rustdoc builds with existing warnings, including private intra-doc links.                         |
| Ordinary artifacts              | Fresh Fastly, Workers, and Spin release builds and an Axum build pass; none contains the fictional finalizer registration ID.                             |

The browser controls ran before the final narrow WASI future-layout correction;
that correction changed no JS behavior. Full Rust gates and the exact CI runtime
selection were rerun after the correction. A subsequent test-only raw-origin cache
hit regression passed natively and on WASI with primary-package Clippy; it changes
no production behavior or artifact. No source edits were made by reviewers
or the build validator.

The existing template-cache controls pass on pinned Viceroy 0.17: ESI has 22
top-level assertions plus 17 cookie/session cases; inline has 9 top-level
assertions plus 17 cookie/session cases. Both exercise real winning bids; ESI
also verifies the GPT scheduler contract. The ESI warm-hit socket probe delivers its first
body bytes in 16 ms while the delayed auction completes near 3020 ms. These
controls verify existing behavior with the feature disabled.

The enabled-finalizer raw-origin cache-hit regression uses a working core test
double, rather than claiming a live platform cache hit. Local verification logs
and generated artifacts remain outside version control; they contain exact
command output and runtime diagnostics.

## Actual-runtime verification and limits

The fixture uses a 750 ms chain budget, delays document EOF by 50 ms, and delays a
successful lookup by 150 ms. On timer adapters the hung transport timeout is
30000 ms, so returning near 750 ms proves the terminal timer released the page
rather than relying on the transport timeout. Each hung case returns the exact
249-byte disabled-control body. Successful cases return the exact expected
replacement, with one completed-document lookup.

| Adapter | Profile/runtime                      | Success request | Hung request | Terminal evidence                                                |
| ------- | ------------------------------------ | --------------- | ------------ | ---------------------------------------------------------------- |
| Axum    | Native dev binary                    | 0.225 s         | 0.825 s      | 753 ms terminal time; timer present.                             |
| Workers | Release WASM, `wrangler dev --local` | 0.244 s         | 0.826 s      | 752 ms terminal time; timer present.                             |
| Spin    | Release WASM, `spin up`              | 0.226 s         | 0.819 s      | 753 ms terminal time; timer present.                             |
| Fastly  | Release WASM, isolated Viceroy 0.19  | 0.213 s         | 0.804 s      | 749 ms from lookup to response; timer absent, bounded host call. |

Development-profile baseline/success/hung runs also passed on all four runtimes.
The optional actual-auction fallback control passed on Axum, Spin, and Fastly.
Positive winning-bid assembly is additionally covered by the native/WASI core
regression and the existing template-cache controls.

Two runtime qualifications remain explicit:

1. Pinned Viceroy 0.17 does not simulate backend timeouts, so it cannot prove a
   Fastly hung-call bound. An isolated official 0.19 executable was used for that
   live check; repository pins remain unchanged. Backend timeout support is
   documented in the [official 0.19 release](https://github.com/fastly/Viceroy/releases/tag/v0.19.0).
2. The Workers actual-auction control returns HTTP 500 with
   `time not implemented on this platform` from `std::time` and then a WASM
   trap. This reproduces the separate #1240 clock problem. Ordinary Workers
   finalization controls pass. Native parity and mocked auction tests do not
   establish that the combined actual-Workers auction route works. No clock
   workaround is bundled, and that live combination remains unverified until
   #1240 is resolved.

## Independent review

Architecture and staff-level final reviews accepted the implementation with no
remaining blocking findings. Review requested a real lazy-body client-cancellation
test and clearer documentation of the synchronous helper's terminal-phase limits;
both were added and re-reviewed. Architecture review requested a positive winning
bid with an enabled finalizer; that regression now passes natively and on WASI.

The spec, plan, code, and tests agree on ownership, ordering, deadline scope,
cache/privacy policy, optional fallback, and excluded response types. The remaining
Workers runtime limitation is documented rather than counted as a passing test.
The implementation branch and isolated worktree are preserved for maintainer
review; no merge, push, deployment, or public issue comment is part of this work.
