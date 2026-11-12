# Binary attestations Phase 2: ordinary Fastly release equality

**Issue:** #1235. **Date:** 2026-10-28.

**Status:** Gate 1 design approved and implemented in #1449. This phase-specific spec records the design contract only; rehearsal transcripts, current verification and approval notes live in #1449 comments and #1235. It opens no later gates.

## Goal and scope

Before signing a release, build `trusted-server-fastly.wasm` twice in separate fresh hosted jobs and require equality of every final byte. Build and compare `trusted-server-fastly.tar.gz` as well, using the fixed service-neutral manifest and normalized archive metadata. Publish the real result as `trusted-server-fastly.wasm.rebuild-check.json`. The canonical build remains the released binary.

The [approved Gate 1 design](../plans/binary-attestations-phase2-fastly-integration.md) defines the contract, trust flow and failure bounds. The [implementation plan](../plans/binary-attestations-phase2-fastly-integration-implementation.md) defines the concrete joins and tests. The [phase assignment](../plans/binary-attestations-phases-2-4.md) records the source-only producer port and later boundaries. These documents form this spec's detailed design. The [Phase 1 spec](2026-10-09-1235-release-attestation-design.md) remains the baseline; the Phase 3 Gate 2/3 proposals require separate approval.

## Release path and authority

Canonical and reproduction jobs execute the same ordinary release procedure from the exact source commit. They vary physical roots (including a private Cargo home per role), start time, job count and umask. Approved target-scoped compiler path remapping normalizes source identity before compilation; comparison retains debug information and excludes no final bytes.

Read-only comparison checks current run/attempt, expected successful jobs, direct upload-action outputs, artifact API identities/digests and bounded archive contents. It joins retained build observations to both actual binaries, compares complete bytes and writes only a genuine matched companion. Failure cannot leave a publishable matched output. Observation, comparison and writer types confer no controlled-recipe qualification.

All signing waits for Fastly equality. The isolated reusable `release-build.yml` signer performs no source checkout or producer execution. Fastly provenance names the wasm, SPDX, package and companion. Its SPDX attestation names both wasm and package; both existing bundle names remain unchanged. Before creating a release, the publisher checks exact handoff sets and signed-input receipts. Mutations remain single-shot and use the release ID; published verification remains bounded and read-only.

## Compatibility, adapters and configuration

The four binary names, SPDX 2.3 predicates/companions, reusable signer identity and existing binary verification commands remain stable. Publication adds the package and companion: 19 files, with `SHA256SUMS` covering the other 18. The companion has version 1, profile `ordinary-release-fastly-byte-equality-v1` and `release_qualification:false`. Consumers must check the profile. Later controlled evidence may use the same permanent filename only under its separately approved compatibility contract.

Fastly alone gains an equality gate. The CLI binaries retain their native build/smoke requirements; native reproduction remains a closed-producer measurement capability rather than an ordinary-release second-build gate. The ordinary release matrix builds each native asset once. Cloudflare and Spin are outside the published cohort and gain no release gate. No runtime configuration, consent decision or request-data collection changes. Build observations use an explicit metadata allowlist, never the inherited environment or operator secrets. The public companion includes hosted checkout/target roots and Cargo homes, UID, start time, umask/job count and locale/timezone as build metadata.

Build-only configuration remains separate from runtime config. Ordinary releases build fresh private JS bundles. If release builds select `TSJS_PREBUILT_DIR` or `TRUSTED_SERVER__TSJS_PREBUILT_INVENTORY`, both are required and the inventory path must be absolute; the ordinary producer refuses `TSJS_PREBUILT_DIR`. The opt-in `TRUSTED_SERVER__RELEASE_JS_TESTS` flag defaults to absent, accepts only `1` when present, and rejects the unsupported current esbuild lock before controlled tests run.

The existing `RELEASE_PUBLICATION_ENABLED` variable still gates signing/publication only. Release CI runs the equality gate on every push to `main` and every `v*` tag; there is no separate equality switch. Maintainers accept the cost of a second Fastly build plus comparison on each of those runs. A mismatch fails the run: on `main` the commit lacks the passing release checks required before tagging, and on a tag it prevents release. Individual build and comparison ceilings remain 90 and 10 minutes; actual timing belongs to the exact rehearsal record.

## Limits and failure handling

The unused controlled producer graph, its tests, schemas, fixtures and `jsonschema` dependency are deferred to the phase that enables them. Phase 2 retains ordinary measurement/comparison, strict JSON and bounded I/O/process helpers, Cargo-message/path checks and Fastly package production/inspection. The lock starts from `main`, preserving its shipped dependency versions. Ordinary equality does not establish controlled material/tool/boundary qualification, complete composition, a release index or SLSA Level 3. The retained diagnostic JS scripts and their tests are separate from the deferred Rust controlled producer. Their full collector is skipped and guarded against the unsupported esbuild lock; ordinary builds do not use that collector. GNU masking WIP is excluded. Each role builds with its own fresh Cargo home, remapped to `/cargo-home`, so dependency source paths do not depend on the runner's home directory; both jobs still share the runner user and hosted image.

Input, transfer, comparison or signing failure prevents release. A real mismatch requires a decision; exclusions and weaker equality are prohibited. Existing drafts/releases are refused rather than overwritten. A failure after publication leaves an immutable release and requires the guide's read-only recovery commands, never a replacement under the same tag.

## Reproducible deployment package amendment

The 2026-11-12 review response adds an operator-downloadable package while preserving every wasm name, verification command and the permanent rebuild-check filename. This changes the earlier wasm-only Gate 1 asset/subject sets; the amendment governs those sets. No later qualification, composition or index gate opens.

The package writer uses pinned Rust `tar` and `flate2`, ustar headers, gzip level 9 with timestamp zero and OS 255, numeric owner/group zero, modes 0755 for directories and 0644 for files, and fixed member order: `trusted-server-fastly/`, `trusted-server-fastly/bin/`, `trusted-server-fastly/bin/main.wasm`, `trusted-server-fastly/fastly.toml`. The last file is the exact tracked two-line `scripts/release/fastly.toml`; the module is the exact role's final wasm. No filesystem metadata enters the archive. Writes refuse an existing destination, persist through a private temporary file and verify input/output readback.

Private observations now require a package size/digest. Each authenticated build ZIP contains exactly wasm, SPDX and package. Comparison checks both observed package identities and compares complete archive bytes. The version-1 companion keeps all wasm fields and adds `package` with `asset`, `size`, `manifest_sha256`, `canonical_sha256` and `reproduction_sha256`; each build record also carries the observed package size/digest. Both package digests must match. This is additive ordinary evidence, never a reinterpretation as controlled qualification.

The isolated signer admits four Fastly inputs, rechecks the package against the comparison and fixed manifest identity, attests all four in provenance and both deployable files in SPDX, and retains the same two bundle names. Native inputs remain binary plus SPDX. The publisher checks package handoff hashes and verifies its provenance and SPDX before any release mutation. The fixed set is 19 release files and 18 checksum entries. The operator deploys the verified package unchanged; local repacking remains a fallback.

Phase 3 #1463 must restore its required deferred producer helpers in its own scope, reconcile the additive package evidence and update the index before landing. Packages PR #1543 must reconcile the extra release asset/subjects. Neither dependent branch is modified by this amendment. The optional GitHub custom-predicate redesign is deferred: it would change the reviewed transport/observation trust boundary and permanent companion contract.

Regression coverage must reject a package-only mismatch despite equal wasm, changed observed package bytes, omitted package inputs/subjects and post-signing corruption. Metadata variation must preserve complete package bytes. The pinned Fastly CLI must accept the new writer's output; hosted CI and exact-input signed publication rehearsal remain separate verification obligations. Earlier `.39` evidence applies only to its original head and wasm-only release contract.

## Verification and readiness

Pure tests cover strict documents, source/tool/JS/environment joins, archive validation, atomic output and failure cleanup. Hosted process tests cover subprocess behavior, cancellation and negative comparator behavior, and contract tests require the real sign and publication jobs to skip after a failed comparison; local process tests are not substituted for hosted evidence. Release CI (every push to `main` and every `v*` tag) builds all four binaries and the additional Fastly reproduction; pull requests run the release Node suite and producer tests only. The pinned Fastly packaging route runs read-only in CI without deployment or local configuration changes.

The release rehearsal must verify all 19 files, immutable/prerelease/not-Latest state, actual A/B joins, both predicates and identities online/offline for all four binaries, companion provenance, SPDX equality, sums, native smoke and pinned Fastly packaging. Exact-source rehearsal evidence is recorded in #1449 comments and the [compatibility and rehearsal record](../plans/binary-attestations-phase2-fastly-integration.md#compatibility-and-rehearsal-record).

The user approved completing Gate 1’s structured failure diagnostics before the fresh rehearsal. Typed rejection checks classify a separate canonical diagnostic, capped at 4,096 bytes, with actual measured facts or explicit `unavailable` fields. It remains a CI-only failure artifact alongside bounded stderr, never a signed or published companion. Writer failure cannot change the failed result. The negative hosted fixture must observe `mismatched` at stage `bytes`.

The existing parent issue #1235 tracks this phase; no separate tracking issue is needed.

## Review log

- Round 1 (2026-10-28, fresh independent AI source review of implementation `8a797dbcb` and this phase spec): found the inherited structured-failure-diagnostic promise unimplemented and the pagination claim inconsistent with the fail-closed single-page limit. The user approved implementing the diagnostic requirement before rehearsal; corrected the plan to its actual 100-record bound. Clarified native reproduction, build-only JS settings and public build metadata. Historical `.13` remains attributed only to its exact source.

- Round 2 (2026-10-28, fresh independent AI source review of `8a797dbcb` plus the structured-diagnostic changes): no blocking spec findings. Confirmed the complete single-page transfer bound, native-once build scope, build-only configuration/privacy disclosure and typed diagnostic contract against source. Tests, final implementation/security review, exact-head CI and the current-input rehearsal remain separately recorded gates.

- Round 3 (2026-11-12, independent AI source review of the deployment-package amendment): found that package-only inequality lost structured mismatch classification, retained Cargo-parser tests were missing, and unused fixture files survived the removal. Added package-specific measured failure facts and a command regression, ported the relevant parser tests, and removed the remaining fixtures. Added exact SPDX-subject coverage and offline package verification.
- Round 4 (2026-11-12, independent AI follow-up source review): no remaining blocking design findings. Confirmed both role validation calls, four-input Fastly provenance, two-subject SPDX, 19-file publication and the deferred-producer boundary. Local checks, current-head hosted CI and signed-publication rehearsal remain separate evidence.
