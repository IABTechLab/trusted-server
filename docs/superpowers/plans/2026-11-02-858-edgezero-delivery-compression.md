# Consume EdgeZero delivery compression

## Goal

Keep Trusted Server's SSAT output-only compression behavior while replacing its
local platform mechanism with [EdgeZero PR #452](https://github.com/IABTechLab/edgezero/pull/452).
Both PRs remain draft for joint review. The
[SSAT design](../specs/2026-10-29-858-ssat-delivery-compression-offload-design.md)
owns application policy; EdgeZero owns capability, checked identity intent, and
Fastly's final delivery headers.

## Preparation

- [x] Publish EdgeZero's reviewed spec, implementation plan, implementation, and
      passing native/Viceroy/workspace checks in draft PR #452 before consumer edits.
- [x] Preserve old branch as `backup/issue-858-before-edgezero-20261102`.
- [x] Rebase one feature-only tree delta onto main
      `51730bdaf841e4d7bb96c2258e5e98169bdff9a7`. Do not replay inherited parser-stack
      or merge commits. The clean rebase preserves current trace, ingress, and
      absolute-deadline code.

## Implementation

1. Pin all six root workspace EdgeZero dependencies to
   `1c0f475097a813a3a9cbb7a4a121ca0523d5ed64`. Regenerate only the necessary lock
   entries and verify all eight EdgeZero packages have one exact git source.
   Do not update Fastly SDK pins or unrelated registry versions.
2. Read support from the incoming EdgeZero request before encoder selection.
   Remove `DeliveryCompression`, `PublisherAdapterOptions`, their per-adapter
   literals, and the local generic response marker/header helper. Restore the
   existing cache-header argument where it is the only remaining publisher option.
3. Carry one explicit local pending output plan through every finalizer. It must
   distinguish preserved input coding, explicit guest output, and adapter identity
   output without a second synchronized offload boolean. Keep origin decoding and
   template-store identity transforms separate from final delivery encoding.
4. Make buffered, lazy, template-miss/hit/fallback, and ordered-EOF outputs use that
   plan. Install identity bytes or the final identity-producing stream first,
   normalize its representation metadata, then call EdgeZero's checked marking
   method. Never put the final intent on raw origin or pending intermediate output.
5. In the real custom Fastly sender, call the shared EdgeZero finalizer after late
   header effects and existing privacy guards, before response parts are consumed.
   Preserve its streaming pump, post-send work, error attribution, and all current
   trace/deadline safeguards. Do not switch to the collecting standard converter.
6. Adapt regressions to inject typed request support and assert no premature
   response intent. Core tests assert identity output/intent; Fastly terminal tests
   assert the hint and framing after the framework finalizer. Retain unavailable,
   deflate/failed-reader, non-SSAT/non-HTML, unsupported-origin, bodyless/range/error,
   privacy, prefix-before-auction, and codec-equivalence coverage.

## Verification and publication

- Run the focused delivery, publisher, ordered-EOF and Fastly terminal tests through
  the pinned dependency, then neighboring current-main trace/deadline/ingress tests.
- Run current repository CI gates, including adapter tests/lints, parity, digest,
  clock, formatting, JS, docs, script/pin/host checks and the required local EOF and
  deadline runtime fixtures. Record unavailable tooling or baseline failures rather
  than claiming those checks pass.
- Inspect the final diff and dependency graph independently. No application-local
  Fastly delivery helper or branch capability/options wrapper should remain.
  Pre-existing static-asset hint behavior is outside this change.
- Commit the stable consumer state, update the original branch with an explicit
  lease for its verified old remote SHA, and update draft PR #884. Retain the backup.
- Cross-link both draft PRs and both issues, record the exact framework dependency,
  and keep staging negotiation/performance criteria open.

Replace the temporary framework revision pin with a merged EdgeZero release before
marking the consumer merge-ready. No production deployment or performance claim is
part of this plan.

## Implementation and verification record

The consumer now uses the six exact dependency pins and eight matching lockfile
sources. The lockfile changes no registry or SDK versions. The pending output plan
replaces the local mechanism, and all four adapter `app.rs` files match the main
baseline. Independent source review found no blockers and verified all ten runtime
input hashes.

The following records the 2026-11-02 integration checks for consumer `db27e738`
and framework `63c7b63`, before the test-only review follow-up below. These checks
passed with Rust 1.98.1, Node 24.20.0 and Viceroy 0.21.0:

- All 32 Rust, adapter, lint, parity, digest, clock, host and documentation gates.
  Native core passed 3,345 tests with two ignored. The normal Fastly alias passed
  244 adapter tests and 3,338 core tests, plus the remaining shared-crate and
  documentation tests. The reusable-sandbox alias also passed.
- Actual Fastly deadline cases, Cloudflare and Spin deadline runtime fixtures,
  and actual Axum plus release-mode Fastly EOF controls, including auction and
  prefix-before-completion behavior.
- JavaScript tests passed 1,838 tests with one skipped; JS formatting/build and
  repository script and Node-pin checks passed.
- Documentation formatting and build passed. The final review-log update receives
  a separate documentation check before publication.

An isolated single-package Fastly test build failed to link
`fastly_http_req::pending_req_header_insert` under Viceroy. Its cause remains
unresolved; unchanged-main, dependency-pin-only and shared-helper-only probes did
not reproduce it. The required `cargo test-fastly` and `cargo test-fastly-reuse`
configurations, actual deadline fixture and release EOF controls all passed with
the default runner. Final verification uses no trap override, SDK stub, pin change
or runtime-source workaround. Earlier trap-mode diagnostics are not final proof.

## Review follow-up, 2026-11-03

Review finding R1 replaces the framework Vary test's copied production predicate
with literal expected header fields. Framework commit `1c0f4750` changes
only that test; this consumer refreshes all six declarations and eight lock
sources without changing registry or SDK versions. The original review and
integration evidence above remains associated with its original revisions.

During the 2026-11-03 review, local SDK contract execution and both consumer
Fastly alias configurations failed before tests on
`fastly_http_req::pending_req_header_insert`. Same-tree hosted CI logs verified
successful SDK, alias and release EOF execution in that hosted environment.
The current local discrepancy remains unresolved. This follow-up includes no
runtime workaround and makes no new local WASM-pass claim.

No deployment ran. Real Fastly reader negotiation, progressive CDN delivery,
service-chain behavior and vCPU/transfer measurements remain staging work.
