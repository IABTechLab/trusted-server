# Issue 854 measurement, implementation and validation plan

Status: Implementation and merged-main local verification complete. Code commit `7b83a8ce87e773e2234a6dde7380a123f36e7d1f`. Hosted CI remains separate. Authorized 2026-10-29. Baseline `c36c82df43c2f62a59444290f863f736b579ed33`.
Spec: [Client CWV completion](../specs/2026-10-29-854-client-cwv-completion-design.md).

## Ownership and evidence

One coordinator owns the combined result, Cargo commands and integration builds. Loading and reservation workers have disjoint source ownership; measurement owns its new fixture/exporter/runner. Avoid global formatting until writers finish. Read and verify returned changes directly.

Local evidence directory: `/tmp/ts-854-cwv/evidence`. This location is temporary; summarize commands, exact hashes and results in the committed validation record. Never commit private browser captures or customer information. Fixtures use fictional example data.

## 0. Baseline before behavior changes

- [x] Record clean branch, PR checks, toolchain and dependency state.
- [x] Install pinned npm dependencies in JS/browser/docs roots.
- [x] Freeze exact baseline bundles and raw GPT bootstrap outside generated output.
- [x] Run baseline JS suite: 1,490 tests across 51 files, no type errors.
- [x] Inspect the existing failing publisher-registration GitHub job.
- [x] Archive baseline tracked sources.
- [x] Generate production HTML with the same fixture exporter and fixture-only access to existing private builders; archived baseline omits the new reservation-path setter.
- [x] Run baseline parser/paint/layout/request measurements before interpreting changed measurements.

Original CI failure is a launcher literal requiring Viceroy 0.17.0 while the PR merge checkout's authoritative tool pin is 0.21.0. Local branch pin and local binary are still 0.17.0. Fix the launcher to read its checkout's pin and test matching/mismatching version behavior. Do not downgrade CI or remove pin validation.

## 1. Implement and prove explicit reservation

- Add optional typed config and validation, omitted-field compatibility and all necessary struct-literal migrations.
- Render escaped exact-ID min-height CSS with ordered breakpoint rules.
- Pass path separately through HTML processing on ordinary, streaming, buffered and shared-template paths.
- Tests: absent config, matching/nonmatching path, absent element, widths around boundaries, zero/unknown height, unsafe IDs, deterministic first-matching same-wrapper policy, invalid ordering, no-fill and shared-template visitor neutrality.
- Run focused native Rust tests plus template/stream neighbors. Run real browser geometry and CLS checks after the exporter is ready.
- Document publisher wrapper ownership and whitespace tradeoffs.

## 2. Implement and prove dependency-safe deferral

- Extract shared GPT shim/render bridge without changing semantics; preserve exports used by existing tests.
- Add compiled early entry and default supported-profile selection with explicit synchronous fallback.
- Keep original raw bootstrap, unified route/hash and unsupported profiles unchanged.
- Enable standalone GPT route only for enabled GPT; version and defer its tag.
- Preserve GAM/APS executing-script attributes and script-text safety.
- Tests: core direct APIs/queues before runtime, early dynamic script rewrite, early candidate rendering, delayed/failed runtime, takeover exactly once, pre-runtime navigation, stale payload exclusion, active diagnostics and blocked integration fallback.
- Preserve old working response-CSP policies with conservative synchronous fallback for any enforced origin or configured response header. Report-Only alone does not force fallback. Prove actual origin propagation and shared cold/warm behavior, not merely the tag builder.
- Run affected GPT/core/guard/APS tests before the full JS suite.

## 3. Measurement implementation and comparison

- Add a small native Rust exporter using the real HTML transform, registry and script generation.
- Export baseline and changed documents from their exact source trees. Include build fingerprints and served-byte equality.
- Add loopback Playwright runner with parser-created tags and deterministic fixture transport/creative behavior.
- Capture baseline, loading-only, reservation-only and combined repeated alternating runs on mobile/desktop.
- Preserve raw PerformanceObserver data, session-window CLS, FCP/LCP, DOM timings, request/targeting snapshots, first visible ad, whitespace and raw/gzip/Brotli artifact sizes.
- Run boundary/no-fill/unreserved/absent-slot/framework-replacement correctness cases independently from repeated timing runs.
- Summarize medians/ranges and regressions without invented pass thresholds or field-CWV claims.

## Review checkpoint and regression fixes

The first four-lane review reproduced a default loading regression under an old-hash-plus-same-origin CSP and found that the framework renderer-absence control intercepted only the old bundle URL. Fixes add conservative enforced-response-CSP fallback, true deliberate renderer removal and a distinct failed-runtime case retaining early rendering. Six focused Rust CSP checks and three focused actual-browser regressions passed. The final exporter produces byte-identical ordinary HTML/assets to the 58-case timing snapshot, so those passing measurements remain applicable. The stable post-fix checkpoint passed all 21 backend gates, 40 actual Rust/Next.js cases, 61 lab cases and 109 offline contracts. Exact commands, fingerprints and baseline limitations are in the validation record. Hosted CI remains pending after push; real GPT/GAM staging and field CWV remain unverified.

Premerge transfer cost was an explicit tradeoff: combined HTML/runtime gzip estimate grew by 7,513 bytes. After merging main, the corresponding increase is 8,479 bytes and includes upstream/build-dependency changes. A runtime-only entry was evaluated and deferred because it adds another lifecycle contract; no total-byte reduction is claimed.

## Main integration checkpoint

Merged main `21f43aa0138498023455789f152f5a864bbefe24` into the implementation in commit `7b83a8ce87e773e2234a6dde7380a123f36e7d1f`. Preserved upstream APS failure relay, iframe/timeout ownership, asynchronous HTML EOF and origin-header rewriting. Threaded actual path/CSP through the new EOF driver.

Fresh Node 24.20.0/Rust 1.98.1/Viceroy 0.21.0 checks passed 1,519 JS tests, 3,197 native core tests plus 20 native integrations, 61 lab cases, 109 offline contracts, 40 actual Next.js cases, six real-PUC APS cases and all 27 backend/EOF/URL gates. The new Node-labelled image and all 14 actual embedded JS module strings were verified. Independent merge correctness/architecture reviews accepted the resolutions and narrow host-lint exceptions. Standalone TypeScript and unrelated whole-workflow actionlint failures remain disclosed. Hosted CI and real GPT/GAM/field validation are not inferred from these local results.

The unchanged baseline archive and all failed browser/quota/lint attempts remain distinct from successful post-merge runs. The first hosted run then flagged four fixture HTML-regex operations. A parser/source-range follow-up replaces them without changing production assets or measured HTML bytes. It passes 31 Node units, actual framework and focused browser checks; independent review accepts the source-order, overlap and EOF handling. Hosted scanning must assess that pushed fix separately. The validation record contains final measurements, hashes, commands and evidence limits.

## 4. Stable combined verification

After runtime and fixture sources stop changing:

1. Build the complete JS distribution once; fingerprint it.
2. Run full Vitest, type checks, lint, JS/build-script formatting and docs formatting.
3. Run offline publisher-registration contracts and existing initial-render browser scenarios against those exact bundles.
4. Run the production Rust HTML browser measurement path and inspect output, geometry and visible content.
5. Run actual Rust/Next.js registration integration with fresh current WASM and artifact provenance.
6. Run repository-required target-matched adapter tests/Clippy, parity, native build-digest test/lint and WASM clock lint. Serialize Cargo on one build directory. Run docs checks with supported feature combinations and disclose the repository's all-features platform constraint.
7. Investigate failures once against the frozen baseline when attribution is uncertain. Keep failed commands and excluded coverage in the validation record.
8. Request independent correctness/architecture review, verify findings, fix confirmed defects and rerun affected checks only.

## 5. Staging and handoff

Real GPT staging and production activation require separate approval. Record missing approval/environment as unverified, not as passing. Lab evidence cannot establish field CWV or customer ad-request effects.

Update spec/plan status and a committed validation record with commands, environment, fingerprints, results and remaining limits. Update old registration documents to point at the expanded scope without rewriting historical evidence. Commit scoped changes, push the existing branch, and update PR1460's description. Report new hosted check status after the push and distinguish pending CI from completed local checks.

## Related

- Spec: [Client CWV completion](../specs/2026-10-29-854-client-cwv-completion-design.md).
- Validation: [Local outcomes and evidence limits](../reviews/2026-10-29-854-client-cwv-validation.md).
- Prior plan: [Publisher registration](2026-10-27-854-publisher-registered-initial-delivery-plan.md).
