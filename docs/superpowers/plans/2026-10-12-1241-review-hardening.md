# HTML finalization review hardening implementation plan

> **For agentic workers:** Use superpowers:executing-plans for central implementation and independent review. Checkboxes record observed completion.

**Goal:** Close the confirmed response-eligibility and maintenance gaps from the independent review of PR #1290.

**Architecture:** Preserve the explicit HTML terminal phase and conservative cache policy. Carry authoritative response metadata to terminal callbacks, bypass all body transformations for opted-in range-associated HTML, and preserve exactly-once auction abandonment. Use the existing platform-compatible clock for auction telemetry and exercise the real Axum harness in CI.

**Tech Stack:** Existing Rust/local futures, HTTP status/header types, `web_time`, Python standard library, and GitHub Actions.

## Reviewed design refinements

The user authorized fixes after the read-only review. These refinements supplement
the [existing design](../specs/2026-10-12-1241-async-html-finalization-design.md).

- Terminal callbacks receive the actual response status. Full 404/500 HTML remains
  eligible; a signing consumer may return `Unchanged` for unwanted statuses.
- When terminal integrations are enabled, HTML with status 206 or any
  `Content-Range` bypasses rewriting and terminal I/O. The latter includes
  range-associated error responses such as 416. Preserve encoded bytes,
  `Content-Range`, framing, and ordinary origin policy. Do not strip ranges from
  all GETs: non-document fetch/subresource semantics remain intact.
- Both response wrappers check actual metadata before removing framing or applying
  terminal privacy. Direct body helpers use private carried metadata and also
  preserve partial bytes. Dispatched auctions are abandoned once on bypass.
- Construct terminal input before moving auction ownership. This closes a latent
  error-path hazard without manufacturing an impossible body variant for a test.
- Reduce redundant replay flushes while keeping the pre-auction flush barrier.
- The live Workers failure names `AuctionObservationContext::from_parts` calling
  unsupported `std::time::Instant`. Change only that telemetry clock to
  `web_time::Instant`, already used by the orchestrator, and verify the actual
  Workers auction control. Broader clock work under #1240 remains separate.
- Keep cache bypass explicit. A user-independent flag alone does not define
  external freshness, signing-key identity, invalidation, failure caching, or
  callback behavior on hits. Document the product cost and future cache contract.
- Preserve the marker-placement integration contract. Neighbor checks do not
  establish parsing context. Document required real-consumer placement tests and
  avoid a second parser or misleading structural guarantees.
- Document the required registry field's source-compatibility migration. Keep
  registry metadata mandatory rather than restoring an unsafe optional fallback.

## Tasks and evidence

### 1. Response metadata and partial-response regressions

Files: `publisher.rs`, `publisher/html_finalization.rs`,
`integrations/html_finalization.rs`, and existing host parity fixtures.

- [x] Add tests for 206 and 200/416 with `Content-Range`, materialized/streamed
      bodies, lazy/buffered/direct helpers, compressed byte identity, preserved
      range/framing/validators, no callback/timer, and exactly-once abandonment.
- [x] Run the focused native tests before implementation and record the failure.
- [x] Add private status/range metadata and public borrowed status; wire every
      constructor and authoritative response wrapper. Bypass before mutation.
- [x] Verify callbacks observe 200/404/500; verify non-document range forwarding,
      unsolicited partial-response bypass, and feature-off behavior unchanged.
- [x] Run focused native and WASI tests and direct core lint.

### 2. Ownership and inexpensive cleanup

Files: `publisher/html_finalization.rs`, `publisher.rs`,
`integrations/html_finalization.rs`, `registry.rs`, `platform/mod.rs`,
`settings.rs`, and `scripts/fixtures/async-html-finalization-runtime.py`.

- [x] Initialize terminal input before moving auction ownership.
- [x] Simplify the unreachable writer branch while retaining poison behavior.
- [x] Put touched tests below imports and add a descriptive timeout validation
      message. Preserve existing output-limit and range tests.
- [x] Add a meaningful flush-count regression before reducing replay flushes;
      retain prefix delivery before auction collection, including missing body end.
- [x] Explain intentionally ignored cleanup exceptions and reap killed children.
      Verify cleanup against real short-lived/stubborn local process groups.

### 3. Actual-runtime evidence and CI

Files: `auction/telemetry.rs`, `.github/workflows/test.yml`, runtime harness.

- [x] Preserve the existing actual Workers panic as red evidence, change the
      telemetry clock only, and rerun the fresh release Workers auction fixture.
- [x] Add actual Axum disabled/success/hung/auction harness to existing native CI,
      Python 3.12, bounded timeout, and failure artifact retention.
- [x] Run Axum, Workers, Spin, and Fastly runtime controls after source freezes.
      Use isolated Viceroy 0.19 for blocking-host timeout proof; retain normal pins.

### 4. Documentation, verification, and draft PR

Files: canonical guides, existing spec/plan/review, this execution record.

- [x] Update status/partial contracts, marker obligations, cache costs, migration,
      and actual Workers evidence consistently across documents.
- [x] Run target-matched tests/lints, parity, build-digest checks, JS/docs gates,
      ordinary artifact fixture exclusion, and relevant integration/cache controls.
- [x] Obtain independent architecture and final review; resolve findings centrally.
- [ ] Commit and push the verified changes to the existing draft PR. Update its
      description with exact evidence and remaining maintainer decisions.

## Execution record

The independent architecture review approved the addendum. Staff review found a
remaining handler-ordering gap: active/inactive ads and tag suppression could
change partial-response cache policy before the routing bypass. A new nine-case
handler regression reproduced it, and the range decision now precedes those
body-dependent policy branches. Re-review found no remaining blocking findings.
Independent cookie and diagnostics privacy remains intact.

### Red/green evidence

- Partial-response tests failed on the original implementation: a ranged fetch
  invoked a finalizer and the wrappers removed framing/validators and changed
  cache policy. The new 64-case sink matrix and eight handler cases pass.
- The status contract test initially failed to compile because the input had no
  status. Both sinks now expose 200/404/500 accurately without excluding full
  error pages.
- A direct 206 helper test with only status marked partial caught decode/re-encode
  of the original gzip bytes. The guard now derives partial eligibility from
  status as well as carried range metadata.
- Large-document replay initially flushed nine times. It now flushes twice on
  the no-auction fixture. A controlled pending-auction test confirms every ready
  prefix byte is flushed before collection with and without a structural body end.
- The cleanup test against the exact committed pre-fix harness failed because a
  forcibly killed child was not reaped. All three real-process cleanup cases pass
  after the bounded final wait. Tests do not replace process handling with mocks.
- The captured Workers stack named the unsupported standard clock in auction
  telemetry. A fresh release Workers fixture now passes the combined route after
  changing only that clock to `web_time::Instant`.

The response regressions verify no callback/outside work; independent path review
confirms bypass precedes creation of the terminal coordinator and its timer. No
impossible body variant was introduced to test the latent ownership reorder.

### Final verification

| Check                                  | Observed result                                                                                                                                                               |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Focused native finalization tests      | 36 pass, including the 64-case sink matrix, eight ranged-origin cases, nine live/inactive/suppressed policy cases, actual status, direct helpers, and both flush regressions. |
| Pinned Fastly/WASI gate                | 219 adapter, 2932 core, 3 JS crate, 21 OpenRTB, and 4 doc tests pass; existing ignored tests remain ignored.                                                                  |
| Fastly reusable sandbox                | 219 adapter tests pass.                                                                                                                                                       |
| Target-matched Clippy                  | All eight aliases pass on final source. Primary core all-target/all-feature and digest lint pass.                                                                             |
| Native adapter/core tests              | Axum 44, Workers 53, Spin 87; core 2939 unit, 10 digest, 4 doc tests pass. Six core doc tests remain ignored.                                                                 |
| CLI/codegen/parity                     | 875 CLI tests pass with 18 existing ignored; codegen 7 and parity 18 pass. CLI tests preceded the final range-policy adjustment; CLI source did not change.                   |
| Exact CI runtime integration selection | 63 pass, plus the separate Fastly EC lifecycle control. Ordinary artifacts were rebuilt and isolated first.                                                                   |
| Existing template-cache controls       | ESI 22, inline 9, purge 34 checks pass on pinned Viceroy 0.17, including real winning bids and cookie variants.                                                               |
| JS                                     | Build, 50 Vitest files / 1208 tests, type checks, and formatting pass.                                                                                                        |
| Docs/format/syntax                     | Docs lint/build/format, repository Markdown, Rust formatting, diff whitespace, Bash and Python syntax checks pass.                                                            |
| Cleanup                                | Three real-process tests pass: already exited, graceful TERM, and TERM ignored then KILL/reap.                                                                                |
| Ordinary artifacts                     | All four adapters build; the fictional fixture registration ID is absent from every ordinary artifact.                                                                        |

### Actual runtimes

All four actual runtime controls pass: disabled baseline, delayed success, hung
lookup with exact fallback bytes, and enabled-auction fallback assembly. Workers,
Spin, and Fastly use release-profile fixture artifacts; Axum runs the same command
now used by CI. The fixture budget is 750 ms, and timer adapters retain a 30000 ms
transport bound to distinguish timer release from an early HTTP timeout.

| Adapter | Success request | Hung request | Terminal evidence                | Auction control |
| ------- | --------------- | ------------ | -------------------------------- | --------------- |
| Axum    | 0.215 s         | 0.811 s      | 750 ms terminal time             | Pass, 0.214 s.  |
| Workers | 0.242 s         | 0.828 s      | 753 ms terminal time             | Pass, 0.292 s.  |
| Spin    | 0.213 s         | 0.809 s      | 751 ms terminal time             | Pass, 0.211 s.  |
| Fastly  | 0.209 s         | 0.806 s      | 750 ms bounded host-call release | Pass, 0.210 s.  |

Workers and Axum were rerun after the final range-policy correction. Spin/Fastly
actual fixture runs preceded that last correction, which only changes opted-in
range-associated response handling; final native/WASI tests and fresh ordinary
builds verify the correction. Existing browser controls were not repeated because
no JS behavior changes in this hardening.

Fastly transport-bound proof uses an isolated Viceroy 0.19; normal tool pins and
CI controls retain 0.17. Passing Workers auction telemetry does not declare all
clock paths under #1240 fixed. Cacheability and API-shape approval remain
maintainer decisions, explicitly documented in the draft PR. Marker cardinality
still does not establish HTML parsing context; real integrations must preserve
structure and test winning-bid placement. The required registry field remains a
source migration for Git/path consumers.

Verification logs, generated manifests, body/header/timing diagnostics, and
artifact hashes are retained outside version control. No operator configuration,
credentials, or external lookup services were used.
