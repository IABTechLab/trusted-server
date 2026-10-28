# Binary attestations Phase 2: ordinary Fastly release equality

**Issue:** #1235. **Date:** 2026-10-28.

**Status:** Implementation and follow-up correction reviewed; current-source rehearsal is blocked by hosted runner capacity. Historical successful `.16` remains exclusively at `5fa3843364f247fdc261c438ab9257323637a42c`, with its [exact published-byte runtime supplement](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6471330921). This phase-specific spec retains the approved Gate 1 design and opens no later gates.

**Current verification:** The supervision correction at `36454c526f70f145eb42a7994a8b6e74dc1df135` passed independent review and exact-head CI: 29 successful checks and four expected skips. [Rehearsal .17](https://github.com/prk-Jr/trusted-server/actions/runs/40710926728) and [rehearsal .18](https://github.com/prk-Jr/trusted-server/actions/runs/40714430928) could not acquire a macOS ARM64 runner; no successful current-source release rehearsal is claimed. A fresh successful cohort rehearsal remains required before readiness. This record changes documentation only after that implementation. Keep #1449 draft, without reviewers.

## Goal and scope

Before signing a release, build `trusted-server-fastly.wasm` twice in separate fresh hosted jobs and require equality of every final byte. Publish the real result as `trusted-server-fastly.wasm.rebuild-check.json`. The canonical build remains the released binary.

The [approved Gate 1 design](../plans/binary-attestations-phase2-fastly-integration.md) defines the contract, trust flow and failure bounds. The [implementation plan](../plans/binary-attestations-phase2-fastly-integration-implementation.md) defines the concrete joins and tests. The [phase assignment](../plans/binary-attestations-phases-2-4.md) records the source-only producer port and later boundaries. These documents form this spec's detailed design. The [Phase 1 spec](2026-10-09-1235-release-attestation-design.md) remains the baseline; the Phase 3 Gate 2/3 proposals require separate approval.

## Release path and authority

Canonical and reproduction jobs execute the same ordinary release procedure from the exact source commit. They vary physical roots, start time, job count and umask. Approved target-scoped compiler path remapping normalizes source identity before compilation; comparison retains debug information and excludes no final bytes.

Read-only comparison checks current run/attempt, expected successful jobs, direct upload-action outputs, artifact API identities/digests and bounded archive contents. It joins retained build observations to both actual binaries, compares complete bytes and writes only a genuine matched companion. Failure cannot leave a publishable matched output. Observation, comparison and writer types confer no controlled-recipe qualification.

All signing waits for Fastly equality. The isolated reusable `release-build.yml` signer performs no source checkout or producer execution. Fastly provenance adds the companion as a subject; binary-only SPDX attestations and both existing bundle names remain unchanged. Before creating a release, the publisher checks exact handoff sets and signed-input receipts. Mutations remain single-shot and use the release ID; published verification remains bounded and read-only.

## Compatibility, adapters and configuration

The seven binary names, SPDX 2.3 predicates/companions, reusable signer identity and existing binary verification commands remain stable. Publication adds one companion: 30 files, with `SHA256SUMS` covering the other 29. The companion has version 1, profile `ordinary-release-fastly-byte-equality-v1` and `release_qualification:false`. Consumers must check the profile. Later controlled evidence may use the same permanent filename only under its separately approved compatibility contract.

Fastly alone gains an equality gate. Axum and CLI retain their native build/smoke requirements; native reproduction remains a closed-producer measurement capability rather than an ordinary-release second-build gate. The ordinary release matrix builds each native asset once. Cloudflare and Spin are outside the published cohort and gain no release gate. No runtime configuration, consent decision or request-data collection changes. Build observations use an explicit metadata allowlist, never the inherited environment or operator secrets. The public companion includes hosted checkout/target roots, UID, start time, umask/job count and locale/timezone as build metadata.

Build-only configuration remains separate from runtime config. Ordinary releases build fresh private JS bundles. If release builds select `TSJS_PREBUILT_DIR` or `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY`, both are required and the inventory path must be absolute; the ordinary producer refuses `TSJS_PREBUILT_DIR`. The opt-in `TRUSTED_SERVER__RELEASE_JS_TESTS` flag defaults to absent, accepts only `1` when present, and rejects the unsupported current esbuild lock before controlled tests run.

The existing `RELEASE_PUBLICATION_ENABLED` variable still gates signing/publication. Merging Phase 2 activates equality on the next eligible real tag; there is no separate equality switch. Maintainers must accept the cost of a second Fastly build plus comparison. A mismatch prevents release. Individual build and comparison ceilings remain 90 and 10 minutes; actual timing belongs to the exact rehearsal record.

## Limits and failure handling

The closed producer graph is ported from #1262 without opening qualified selectors. Ordinary equality does not establish controlled material/tool/boundary qualification, complete composition, a release index or SLSA Level 3. The controlled JS observer still accepts esbuild 0.27.3 while the current lock has 0.28.2; its full collector remains visibly ignored and guarded. Ordinary builds do not use it. GNU masking WIP is excluded.

Input, transfer, comparison or signing failure prevents release. A real mismatch requires a decision; exclusions and weaker equality are prohibited. Existing drafts/releases are refused rather than overwritten. A failure after publication leaves an immutable release and requires the guide's read-only recovery commands, never a replacement under the same tag.

## Verification and readiness

Pure tests cover strict documents, source/tool/JS/environment joins, archive validation, atomic output and failure cleanup. Hosted process tests cover subprocess behavior, cancellation and negative comparator/canary behavior; local process tests are not substituted for hosted evidence. CI builds all seven binaries and the additional Fastly reproduction; PR signing/publication remain skipped. The pinned Fastly packaging route runs read-only in CI without deployment or local configuration changes.

The release rehearsal must verify all 30 files, immutable/prerelease/not-Latest state, actual A/B joins, both predicates and identities online/offline for all seven binaries, companion provenance, SPDX equality, sums, native smoke and pinned Fastly packaging. Historical `.13` proves only `770a425df`. Rehearsal `.16` covers the later application inputs and structured diagnostics at `5fa3843364f247fdc261c438ab9257323637a42c`; all 18 release jobs and independent verification passed. The [compatibility record](../plans/binary-attestations-phase1-compatibility.md) retains exact-source evidence and the rerun-all recovery proof. The later supervision correction is not covered by `.16`; its exact-head CI passed, but `.17`/`.18` could not finish the seven-binary rehearsal because their ARM64 macOS jobs never acquired runners.

The user approved completing Gate 1’s structured failure diagnostics before the fresh rehearsal. Typed rejection checks classify a separate canonical diagnostic, capped at 4,096 bytes, with actual measured facts or explicit `unavailable` fields. It remains a CI-only failure artifact alongside bounded stderr, never a signed or published companion. Writer failure cannot change the failed result. The negative hosted fixture must observe `mismatched` at stage `bytes`.

Two fresh source-checked spec-review rounds, risk-based independent/security implementation reviews, exact-implementation green CI, historical `.16` rehearsal and unsuccessful current-source attempts are recorded. Any subsequent evidence-record commit is docs-only; its hosted checks are assessed separately. The existing parent issue #1235 tracks this phase; no separate tracking issue is needed. Keep #1449 draft until its outstanding gates are closed and the user authorizes the transition.

## Review log

- Round 1 (2026-10-28, fresh independent AI source review of implementation `8a797dbcb` and this phase spec): found the inherited structured-failure-diagnostic promise unimplemented and the pagination claim inconsistent with the fail-closed single-page limit. The user approved implementing the diagnostic requirement before rehearsal; corrected the plan to its actual 100-record bound. Clarified native reproduction, build-only JS settings and public build metadata. Historical `.13` remains attributed only to its exact source.

- Round 2 (2026-10-28, fresh independent AI source review of `8a797dbcb` plus the structured-diagnostic changes): no blocking spec findings. Confirmed the complete single-page transfer bound, native-once build scope, build-only configuration/privacy disclosure and typed diagnostic contract against source. Tests, final implementation/security review, exact-head CI and the current-input rehearsal remain separately recorded gates.
