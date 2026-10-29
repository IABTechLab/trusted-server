# Gate 3 implementation plan: versioned seven-artifact index

**Status:** Step 1 approved on 2026-10-28 for frozen-v1 regression checks, the assigned source port and restored assertions only. The Gate 3 design is approved with a new neutral v2 cohort ID, `trusted-server-binaries`. No v2 schema/policy/cohort/descriptor/admission/assembly, workflow, release script, signing, production selector or rehearsal tag is authorized. Consumer implementation remains Phase 4; live signing/publication and fork trust remain Gates 4/6.

**Maintainer decision pending:** The user endorses the [issue-scoped all-seven Build-derived Cargo/npm SBOM option](binary-attestations-phase3-issue-scoped-sbom-option.md), profile `issue-cargo-npm-build-v1`, in principle as the proposed replacement target for strict Gate 2. It is not adopted and authorizes no implementation. Maintainers decide whether the precision upgrade belongs in #1235 or a follow-up; strict closure remains a tracked future enhancement. Phase 3 is frozen at completed Step 1. Gate 3 Steps 2–5, and every later production/publication/consumer gate, remain held. Draft #1463 stays draft, with no reviewers.

**Tracking:** Part of #1235; draft #1463. Read with the [approved design](binary-attestations-phase3-seven-artifact-index.md), [Gate 2 coverage/rollout decision](binary-attestations-phase3-complete-spdx-implementation.md) and [phase assignment](binary-attestations-phases-2-4.md).

## Frozen and new identities

| Contract            | Frozen v1                                                         | Proposed v2                                                                               |
| ------------------- | ----------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Index version       | Integer `schema_version: 1`, four entries                         | Integer `schema_version: 2`, exactly the seven approved identities                        |
| Cohort              | `phase1-fastly-cli`                                               | `trusted-server-binaries`; no phase or count in the ID                                    |
| Policy              | Existing four-row `crates/trusted-server-cli/release-policy.json` | Additive, separately approved policy version 2 with only the new cohort ID                |
| Schema              | Existing `release-index-v1.schema.json`                           | Additive `release-index-v2.schema.json`                                                   |
| Published filenames | Existing filenames                                                | Same binary/signer/companion names; index/evidence assets are additive                    |
| Execution authority | Four controlled target selectors remain closed                    | Seven metadata descriptors do not expand controlled execution or inhabit a qualified type |

Existing v1 policy and schema currently match frozen source `122b78664b1235c717e9208fd200f9a9b0c335df` byte-for-byte. Their SHA-256s are respectively `f08ce10cd16ff2d1acc482db2b85eb53b667ec2b4753792f277d12f41d1f636c` and `06a7e598f1d14b499d087ec0c894829b3aab26a690b35e5922d96987a877bb9e`. Keep their fixtures and historical-policy list unchanged. Do not rename existing files or mutate the existing ordinary cohort file to masquerade as v2.

## Explicit meaning of `provenance-sbom-repeatable`

**Approved v2 definition:** authenticated provenance and the approved composition/SPDX evidence, plus mandatory matched complete-final-byte A/B equality under an explicitly approved comparison profile. For the Fastly default row, the accepted ordinary profile is exactly `ordinary-release-fastly-byte-equality-v1`.

That ordinary profile proves the project's observed ordinary-build repeatability with its recorded source/input/tool/environment/run/transfer joins. It does **not** establish controlled-recipe, acquisition, confinement, alternate-user or independent-builder qualification. Policy and public verification documentation must state this definition before v2 is accepted.

This is a versioned policy amendment. The original destination spec and frozen v1 policy's stronger controlled-recipe requirements do not change. A consumer requiring those stronger guarantees must still reject the ordinary receipt. If the user chooses to retain controlled qualification as v2's definition instead, the present Fastly row cannot pass it: its receipt explicitly has `release_qualification:false`, and qualified selection stays closed.

Under the recommended definition, today's matched ordinary receipt can satisfy **the repeatability condition**. It cannot make a complete v2 release pass today: admissible complete evidence for all seven binaries, genuine companions and live publication do not yet exist. Gate 2 and Gate 4 remain prerequisites. No JSON boolean or profile label grants missing production authority.

## Planned source boundaries

These are proposed future paths, not files created by this plan. Adding a file under `scripts/release/` or any policy/config file requires approval of that exact addition before implementation.

| Future change                                                                                                 | Purpose                                                                                                    | Scope boundary                                                                                                      |
| ------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `docs/superpowers/specs/schemas/release-index-v2.schema.json`                                                 | Closed seven-identity schema and exact fields/bounds                                                       | Keep frozen v1 schema and validation intact                                                                         |
| `crates/trusted-server-cli/release-policy-v2.json` — proposed path requiring approval                         | Source-owned policy version 2, neutral cohort ID, seven exact tuples and explicit profile semantics        | Keep the existing CLI v1 policy bytes and historical list intact; no new file now                                   |
| `crates/trusted-server-release/src/policy/index_v2.rs`, `documents.rs`, `distribution/v2.rs` — proposed paths | Add explicit v2 metadata descriptors, offline schema dispatch, strict joins and exact-byte index assembly  | Keep four-target controlled execution tables separate; do not manufacture `SignedCompanions` or qualified authority |
| `scripts/release/release-cohort-v2.json` — proposed future source                                             | Producer-owned v2 metadata cohort; cross-check exactly against the CLI-owned policy                        | Do not repurpose existing cohort or select controlled recipes; held with Steps 2–5                                  |
| Existing Phase 3 assigned source and test splits                                                              | Port reviewed SPDX/collection/distribution primitives and restore staged assertions as separately approved | Closed complete generator and production adapters remain closed; no test constructor becomes production authority   |
| Guide/compatibility/spec notes                                                                                | Define v2 policy identity, ordinary repeatability and unchanged manual binary verification                 | No edits to carried normative v1 sentences; add versioned notes                                                     |
| Phase 4 consumer, later                                                                                       | Dispatch approved v2 policy/schema and enforce the narrow receipt rule                                     | No consumer code or `readonly-verify` changes in Phase 3                                                            |

The v2 policy stays beside v1 at the CLI-owned tracked policy location, consistent with R-POLICY. Its eventual embedding and trusted fixed-path/version dispatch belong to the Phase 4 consumer implementation; Phase 3 does not add consumer code. It must be approved with this plan; choosing a different location is a plan decision, not a downloaded-index choice. Later Phase 4 dispatches only reviewed fixed paths selected by trusted local policy/version rules. An index cannot supply a URL, path, policy hash or version that selects its own authority. Compute the new raw-byte policy hash only after approved policy bytes exist; do not introduce a placeholder pin.

## Producer cohort and authority sources

The proposed producer cohort source is `scripts/release/release-cohort-v2.json`, separate from the existing `release-cohort.json`. It carries the new neutral ID and the seven exact metadata tuples, with assurance profiles finalized only after the Gate 2 rollout decision. The CLI-owned `release-policy-v2.json` governs trusted policy identity, approved cohort/profile requirements and bounds; it is not replaced by the producer cohort. Cross-validation must reject any row, target, profile or count drift between those approved sources. Neither file is created by this plan or by Step 1. Existing v1 policy, ordinary cohort file and historical-policy list remain unchanged.

## Implementation sequence after plan approval

**Sequencing:** Step 1 is approved and its source port, restored assertions and local execution are recorded in the [Step 1 ledger](binary-attestations-phase3-step1-port.md). Steps 2–5, including v2 policy/cohort/schema files, metadata descriptors, ordinary-receipt admission and assembly, wait for the **final Gate 2 rollout/assurance decision**. Choosing the current full-cohort cost assessment is not that final decision. Step 6 integration follows those held steps; planning documents may still be revised now. A target-specific choice could change the v2 rows’ assurance profiles, so do not freeze them prematurely.

1. **After revised-plan approval:** freeze v1 bytes/fixtures with regression checks. Complete the approved source port and restore Phase 3 assertions in separate coherent commits. Prove Phase 2 plus Phase 3 dependency closure with host all-target check/clippy before integrating v2.
2. **Held for Gate 2 decision:** add the separately approved v2 policy/cohort sources and a metadata-only seven-row descriptor matching every tuple in the approved design: filename, package/bin, role, kind, target, runtime/minimum/architecture, variant, capability and assurance. Never extend `FIXED_ARTIFACTS` to select Axum controlled recipes.
3. **Held for Gate 2 decision:** add offline strict v2 schema/document validation, including duplicate-key, unknown-field, count and cross-field checks. Version/policy dispatch must be explicit; a v1 document remains governed by v1.
4. **Held for Gate 2 decision:** add exact-byte ordinary Fastly receipt admission for the v2 informational slot. Require the exact supported receipt version/profile, Fastly default identity, `matched`, false controlled qualification, source/recipe/lock/JS joins, canonical/reproduction sizes/digests, distinct current-run/attempt job and artifact bindings, signed membership and binary/SPDX agreement. Index the retained original bytes rather than a re-rendered replacement.
5. **Held for Gate 2 decision:** extend metadata assembly and its tests for the exact destination inventory. Keep genuine signed-companion acquisition unavailable until its separately approved integration. Candidate diagnostics, empty signatures and test fixtures cannot authorize publication.
6. Document the v2 definition and compatibility dispatch. Run source/closure tests, formatting/domain lint, independent schema/authority review and hosted CI. Keep the PR draft. Activation, production signing/publication and live rehearsal are **not** part of approving this schema plan.

Gate 2's coverage/rollout decision can block complete evidence independently. Successful metadata tests or carrying a schema must never bypass that stop point.

## Narrow false-qualification handling

The proposed exception is only the **rebuild-check slot** for the exact Fastly default identity under the supported v2 contract and exact ordinary profile, after authenticated membership and all strict joins. It is not a generic permission to accept false qualification.

False qualification in build evidence or build recipe remains rejected under their governing contracts; other assets, unknown profiles, candidates and stronger controlled policies do not gain an exception. Native reproduction remains observational, with explicit terminal outcomes and no new native equality gate. The permanent Fastly rebuild-check filename and its original version-1 bytes remain unchanged.

Phase 4 must implement and test this policy dispatch explicitly. Frozen consumer `check_release_qualification` currently rejects false in all three companion slots; Phase 3 must not weaken or port a public consumer shortcut to make a fixture pass.

## Destination inventory and cycle-free order

The future complete cohort has exactly **53 files**: for each of seven binaries, the binary, SPDX, provenance bundle, SBOM bundle, build evidence, build recipe and rebuild check (49); plus `release-index.json`, its genuine provenance bundle, `tsjs-inventory.json` and `SHA256SUMS` (four). Checksums cover the other **52**, never themselves. This is a destination contract, not today's 30-file release.

Keep the cycle-free dependency order: retain actual binary/evidence/inventory bytes; obtain their genuine required bundles; assemble the exact index without including its own digest, its bundle digest or `SHA256SUMS`; attest the index as `release-index.provenance.sigstore.json`; calculate checksums over the resulting other files; then publish only through the separately approved owner. Gate 4 must review exact subject membership, input isolation and failure behavior before any workflow/signing implementation. Gate 6 must approve fork trust without loosening official source/repository/signer policy.

## Discriminating tests and completion gates

- Preserve raw v1 policy/schema hashes and existing fixtures; old consumers reject v2 until explicitly updated.
- Accept exactly seven metadata tuples; reject each missing, duplicate, extra or cross-field-mutated row. Metadata cannot select a controlled Axum build or inhabit a qualified type.
- Ordinary Fastly admission accepts only the supported informational slot/profile and complete joins. Reject false in either other companion, any other asset/profile, unknown version, stale/shared identities, changed source/recipe/locks/JS/size/digest/SPDX or missing signed proof.
- Retained companion bytes must match the indexed digest exactly; even a whitespace-only replacement needs independent admission and changes its digest.
- Reject empty or placeholder bundles, candidates, missing authentic proof and self-referential digest construction. Require exactly 53 safe unique filenames and checksum membership exactly 52.
- Measure larger-cohort resource bounds without silently expanding them. No process-spawning tests locally; actual process/release checks remain separately authorized hosted work.

For each coherent metadata/schema/join change, first add a discriminating failing test in proposed `documents/tests/index_v2.rs` or `distribution/tests/index_v2.rs`; confirm its intended rejection, implement the smallest change, and require the exact selected test to pass before a separate sentence-case signed commit. These test paths are proposals for approval, not new files in this PR head.

Use the host target explicitly for producer checks, for example `cargo +1.98.1 check -p trusted-server-release --target aarch64-apple-darwin --all-targets --locked` and the matching `clippy --all-targets --all-features -- -D warnings`. List each audited pure test by its full name, then use `cargo +1.98.1 test -p trusted-server-release --target aarch64-apple-darwin --locked <full-test-name> -- --exact` with a nonzero/one-passing-test guard. Hosted Linux CI must exercise Linux-only code and process fixtures; do not run the whole producer suite locally. Finish each approved chunk with Rust/docs formatting, domain lint and independent review before pushing.

Step 1 is approved with the explicit Gate 4 workflow-contract deferral recorded in the assignment. The repeatability definition, frozen v1 hashes, narrow rebuild-check exception and 53/52 contract are already approved. Before Steps 2–5, obtain the final Gate 2 rollout/assurance decision; this plan alone does not open them. Before complete production/index release, close Gate 2 coverage for the selected cohort and separately approve Gate 4 publication and Gate 6 rehearsal trust. Consumer implementation remains Phase 4. No schema, policy, source, workflow or tag is created by this plan.

## Plan review

Independent read-only plan review on 2026-10-28 found no blocking findings against Phase 2 source `5fa384336` and frozen source `122b78664`. It confirmed the strict all-seven coverage stop, frozen v1 bytes, narrow ordinary-profile handling and cycle-free inventory. A policy-source wording inconsistency in the design was corrected to refer to a separately approved v2 policy. No tests, implementation or runtime qualification were performed by this review. Step 1 subsequently received user approval with the workflow-contract unit deferred to Gate 4. The final Gate 2 rollout/assurance decision and later implementation remain pending.

A subsequent independent review checked the user-requested CLI policy location, separate producer cohort source, Step-1-only approval boundary and hold on Steps 2–5. It found no blocking findings; no source or configuration implementation was performed.
