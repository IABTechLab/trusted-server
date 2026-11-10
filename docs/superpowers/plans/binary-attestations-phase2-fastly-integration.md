# Phase 2 Fastly release integration — Gate 1

**Status:** Approved on 2026-10-26 for the ordinary-release adapter, writer, transfer binding and additive provenance subject, subject to the conditions below. The concrete implementation plan was written, independently reviewed and implemented.

**Current state (2026-11-10):** this is the approved Gate 1 record, implemented in #1449, which is open for review. Since approval, Phase 1 narrowed the release to four binaries and 18 files and removed the publisher's post-publish byte comparison, and release CI runs the gate on every push to `main` and every `v*` tag. Rehearsal sections below are dated history for the source they name; the latest rehearsal before the fourth review round is `.30` at `44557229a`, and current rehearsal status lives in #1449 comments and #1235.

**Goal:** Require a fresh Fastly A/B build comparison in the existing release path, publish an authenticated `trusted-server-fastly.wasm.rebuild-check.json`, and preserve the Phase 1 verification commands and identities.

## Decision requested

Approve or revise the ordinary-release comparison adapter below, including its explicitly narrower claim, receipt fields, transfer binding and additive signing inputs. This is a real equality gate for the existing ordinary release builds. It is **not** qualification of #1262's controlled recipes, sandbox, materials or alternate-user environment. If Phase 2 must ship the full controlled-recipe profile instead, defer wiring until its bootstrap/material/tool acquisition and unqualified execution authority have a separately approved design; do not open the existing qualified selectors to satisfy a deadline.

Gate 1 approval authorizes writing/reviewing the concrete implementation plan and then implementing this ordinary adapter within the approved design; a material design change requires renewed approval. No adapter code may land before that approval. Approval of this design alone does not approve a new trigger, IAB setting change, production/qualified selection or qualification resource. The integration must first be implemented and rehearsed in the fork. **Merging Phase 2 is the maintainer activation decision:** release CI then runs the Fastly gate on every push to `main` and every `v*` tag, without a separate switch. Each run costs one additional fresh Fastly build plus comparison, and signing/publication waits for both builds. Budget up to the 90-minute build ceiling plus ten-minute comparison ceiling; observed added wall time must be reported from rehearsal. A mismatch means no release.

## Source findings and alternatives

Source: #1262 at `122b78664b1235c717e9208fd200f9a9b0c335df`; Phase 1 baseline `a501d1ba6bde2db82546d7e16c93facab8c613e6`.

| Approach                                      | Benefit                                                                                              | Cost / restriction                                                                                                                         |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| **Recommended: ordinary release A/B adapter** | Adds a genuine final-byte gate around the current producer, without constructing qualified authority | Proves observed ordinary-build equality, not the complete controlled-recipe assurance profile; this narrower scope needs explicit approval |
| Wire controlled recipes directly              | Uses owning recipe/composition receipts and the full checked-input comparison                        | Blocked: bootstrap, qualified tool/JS/alternate types and production execution remain closed; enabling them is outside this approval       |
| Run ignored candidate tests as the publisher  | Reuses existing diagnostic code quickly                                                              | Rejected: test-only constructors and fixtures cannot establish release authority or replace the real release path                          |

`recipe/compare.rs::check_fastly_equality` validates roles, source, lockfile, materials, JS, Rust tools, ambient pairing and retained final bytes. Its `FastlyEqualityChecked` token has private fields and stronger semantics than a bare file comparison. `recipe/tools.rs`, `preparation.rs` and `recipe/ambient.rs` expose ordinary selectors only under `cfg(test)`; qualified types are uninhabited and reproduction selection fails closed. Do not move test constructors into production or forge those receipts.

Reuse the reviewed strict document parser, bounded I/O/process primitives and final-byte diagnostic logic where their preconditions hold. If needed, factor the byte-comparison primitive beneath the existing recipe checker while preserving all of that checker's input checks and tests. An ordinary receipt must have its own explicit type; it cannot masquerade as `RecipeCompleted` or `FastlyEqualityChecked`.

The frozen candidate rebuild renderer in `distribution/entry.rs` emits null workflow/transfer IDs and gap-labelled candidate evidence. It is not the release writer. The spec's R-INDEX table requires more facts; no closed standalone rebuild-check schema is retained in #1262. This proposal defines the Phase 2 writer contract below; it does not amend the four-entry release-index schema.

## Execution and trust flow

1. Keep the current canonical Fastly output as the released binary. Run canonical and reproduction builds as distinct fresh hosted jobs within the existing reusable `release-build.yml` invocation, from the same exact source commit. Neither job has signing, publication or write credentials. No persistent build cache or artifact from an earlier run supplies A or B.
2. Both jobs execute the same reviewed build procedure and flags. Preserve `--locked`, default features, immutable build facts and current Fastly target. Materialize separate checkouts/output roots. Use the same pinned Rust and Node versions, lockfiles and independently generated JS input measurements. No sandbox/qualified-material guarantee is inferred from hosted-runner isolation.
3. Record actual checkout/root identifiers, start times, umask, job count, locale/timezone and tool identity. Vary host roots and job count/umask deliberately; normalize compiler locale/timezone identically. Record any dimension that cannot be varied. Container-internal identical paths are not described as compiler-visible root variation. Same UID is an unsupported variation, not a passed alternate-user test.
4. Measure JS during each actual build: sorted bundle names, byte lengths and SHA-256s, plus source/package-lock identity. Both builds must retain the same prepared bundle set and digests before the final comparison. Reuse the reviewed inventory/material parsing when available; the interim Phase 1 CycloneDX-to-SPDX output is not a JS inventory. Do not claim an npm resolver graph proves embedded membership. A digest-only measurement cannot become the Phase 3 complete composition inventory.
5. Retain original runner/build observations and exact final artifacts. Current-run artifact IDs, immutable transfer digests, job IDs and source commit must be obtained from runner/API/upload outputs and cross-checked against GitHub's current-run job/artifact records. Never accept a build's self-reported job identity as authoritative. Download A and B by exact names into distinct directories and reject missing, extra, aliased, empty, oversized or digest-mismatched files.
6. A read-only comparison job checks identity, source, lock, recipe, tools and JS measurements before reading bounded complete final bytes. No excluded or normalized Wasm regions are allowed. Compare size, SHA-256 and bytes; on inequality emit bounded diagnostics including first offset/byte windows, without dumping environment secrets. A byte match over mismatched inputs still fails.
7. Only a matched comparison can produce the release companion. Hash and rehash the canonical artifact throughout the handoff. The companion refers to the exact canonical bytes that the existing isolated sign job receives, not another copy selected by filename alone.

The ordinary adapter's transfer checks protect this run's A/B selection; they do not claim the later complete transfer-binding slice is shipped for every existing artifact. Both builds still execute reviewed source and dependencies; equality is not proof that source or dependencies are benign.

## Rebuild-check writer contract

Write strict, deterministic version-1 JSON with sorted keys and one trailing newline. Reject duplicate keys, unknown fields, non-finite numbers, invalid digest formats, excessive lengths, aliases and source/asset mismatches. Values below describe fields, not fabricated example observations.

| Field                                                         | Required meaning                                                                                                                                          |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `version`                                                     | Integer `1`                                                                                                                                               |
| `profile`                                                     | `ordinary-release-fastly-byte-equality-v1`; explicitly distinct from the destination controlled-recipe profile                                            |
| `release_qualification`                                       | Always `false`                                                                                                                                            |
| `source_commit`, `asset`, `target`, `variant`                 | Exact source SHA, `trusted-server-fastly.wasm`, `wasm32-wasip1`, `default`                                                                                |
| `recipe_sha256`                                               | Digest of the exact reviewed build procedure and its flags; both sides must match                                                                         |
| `cargo_lock_sha256`, `npm_lock_sha256`, `js_inventory_sha256` | Observed locked input identities and the shared measured prepared-JS identity; both sides must match                                                      |
| `tools`                                                       | Exact measured Rust/Node tool identities and acquisition source; no qualified status inferred from a version string                                       |
| `canonical`, `reproduction`                                   | Each contains current workflow run ID/attempt, producing job ID, artifact ID/name, transfer SHA-256, observed environment, final size and final SHA-256   |
| `outcome`                                                     | Closed enum `matched`, `mismatched`, `failed`, `unavailable`; only `matched` is publishable for Fastly                                                    |
| `canonical_sha256`, `reproduction_sha256`                     | Top-level join fields retained for existing informational consumers; must equal nested final digests                                                      |
| `gaps`                                                        | Honest remaining assurances, including unqualified tools/materials/boundary and unsupported environment variation; never empty merely because bytes match |

For failed attempts, retain a separate bounded CI diagnostic using the same outcome vocabulary, error stage/code and available observed facts; unavailable facts are explicit and never replaced by invented IDs or digests. Failure diagnostics are not release companions. Never upload a placeholder/empty rebuild-check to satisfy the release file set.

Recipe hashing needs an explicit canonical encoding of source-owned build inputs and flags. JS and tool digests must be measured in the actual jobs, with strict retention/rehash checks. Finalize this encoding and its discriminating tests in the implementation plan after design approval; do not infer it from mutable environment text. No new qualification pins/resources are silently supplied by this proposal.

## Workflow changes after approval

No `on:` trigger changes. Keep `release.yml` calling `release-build.yml`; keep the `sign` job in that reusable workflow, including its `RELEASE_PUBLICATION_ENABLED`/tag gate and permissions. Keep signing isolated from checkout or execution of build inputs. Ordinary PR builds never sign or publish.

Inside `release-build.yml`, the fixed build cohort is four binaries (seven before the Phase 1 narrowing). Canonical Fastly becomes a dedicated job so its upload-action outputs bind unambiguously to its artifacts; the three CLI binaries remain in the matrix. Add the reproduction/comparison dependency only for Fastly. Keep the existing `sign` job and matrix; add the Fastly comparison as a job-level dependency, so every signing entry waits for the Fastly gate. Failure of either Fastly build or comparison prevents signing/publication for the release. This adds no native reproduction gate. Native builds retain their existing success requirements; native reproduction differences or absence do not become a new gate. Do not emit native rebuild companions until their later approved phase.

For Fastly only, expand the exact unsigned input set from binary + SPDX to binary + SPDX + rebuild-check. Rehash all three inputs before/after signing and in the signed transfer receipt. The existing provenance attestation gains the rebuild-check as an additional subject; keep its bundle named `<asset>.provenance.sigstore.json`. Keep the binary-only SPDX attestation and `<asset>.sbom.sigstore.json` unchanged. All other assets' exact input/subject sets remain unchanged.

In `release.yml`, preserve exact-name downloads into separate directories and validate the expanded Fastly receipt before release creation. Add only `trusted-server-fastly.wasm.rebuild-check.json` to publication: **17 existing assets become 18**. `SHA256SUMS` continues to cover every other published asset and never itself. Existing binary, SPDX and bundle names, SLSA/SPDX predicate URIs, reusable signer workflow/certificate identity, source pins and Phase 1 verification commands continue to work unchanged.

Preserve single-shot listing/refusal, draft creation, uploads and publication by release ID; never resume or overwrite an existing tag/draft. Preserve prerelease/Latest behavior, immutable readback, strict release-attestation validation, bounded read-only retries, and release membership checks. Expand exact asset membership/digest expectations to 18. Existing immutable releases are untouched.

## Failure behavior and bounds

- A/B build, measurement, source/tool/input join, transfer check, runtime acceptance or equality failure prevents the companion/signing/publication path. There is no downgrade to unmatched or canonical-only Fastly release.
- Cancellation/timeouts fail closed and clean up owned work; they do not yield a successful receipt. Bound each build to the existing 90-minute build-job ceiling; comparison to ten minutes and bounded input sizes. Preserve the existing publication/verification deadlines rather than granting unlimited new retries.
- Retain bounded failure evidence as CI artifacts with existing seven-day retention. Logs name failed checks and observed digests/IDs; never print full inherited environments or tokens.
- Native reproduction remains observational. A native build/startup failure still fails its existing release checks; an attempted native reproduction reports its actual result without blocking on byte equality.
- Before publication, failures may leave a refused draft requiring explicit human inspection; never retry a mutation or delete automatically. After publication, a verification failure means the release is already immutable: report its release ID/tag and exact read-only recovery commands. Use the existing recovery guide; do not attempt a replacement under the same tag.

## Verification and rehearsal plan

1. Pure tests: writer positive contract; duplicate/malformed/extra fields; missing observations; wrong source, role, tool/JS/recipe identity; byte/size/digest mismatches; canonical changed after comparison; exact 18-file membership; checksum coverage; no successful receipt on failure. Every negative test must reach the intended validation rather than failing on an unrelated missing input.
2. CI-only process tests: real fresh A/B builds, altered ambient settings, deliberate mismatch/cancellation/output bounds and exact current-run transfer selection. Keep local tests pure and explicitly selected with a nonzero guard. All existing Phase 1 tests remain, including mutation/retry failure paths.
3. Independent implementation and security review of ordinary authority, observations, transfer binding, permissions, receipt signing, cleanup and publish failure paths. Confirm no qualified/test-only constructor is newly reachable.
4. After upstream CI is green on the exact reviewed head, use the next unused sequential lightweight fork rehearsal tag under the standing approval. Never tag, release or change settings in IAB; changes to fork settings still require approval.
5. Positive rehearsal: all four binaries build; show A/B source/input identity, measured environment variation and complete final-byte match; print the real receipt and verify its digest/provenance subject. Confirm 18 exact release assets, immutable and prerelease/not-Latest behavior, release download, checksums, all release memberships, both binary provenance/SPDX verifications offline and online with exact `release-build.yml` pins, and shipped SPDX equality. Repeat the manual pinned Fastly package/validate/inspect/byte comparison and runtime/source-header/no-request-facts-log checks without deploying.
6. Rehearsal failure proof: exercise a deterministic comparison mismatch in a CI-only negative fixture, proving no sign/publication dependent job runs. Do not push an intentionally altered source commit as a release rehearsal or add a debug bypass to the release path. A failed real rehearsal retains its tag/release and uses a fresh number only after fixing and obtaining exact-head green CI.
7. Post the transcript, tag/run link and pass/fail result on the Phase 2 PR. Do not claim the full controlled-recipe profile or Phase 2 readiness if only the ordinary bridge is approved/proven.

## Approval checklist

- [x] User accepts the ordinary-release adapter's narrower equality claim for this Phase 2 slice.
- [x] User approves the concrete informational writer/measurement/transfer contract and additive Fastly provenance subject.
- [x] User approves implementation after a concrete plan is written and independently reviewed within this design; qualification types/selectors and new triggers remain closed.
- [x] Rehearse the exact reviewed head and report time/cost and unchanged Phase 1 verification before merge. Merging Phase 2 runs the gate on every push to `main` and every `v*` tag; no separate switch.

## Approved forward-compatibility and mismatch conditions

The filename `trusted-server-fastly.wasm.rebuild-check.json` is permanent. Phase 3 indexes and digests these exact bytes as a rebuild-check companion. A later controlled-recipe implementation supersedes the `ordinary-release-fastly-byte-equality-v1` profile in the same asset, using a distinct profile value while retaining version-1 shared identity/outcome/digest fields where compatible. Consumers must check the profile before interpreting assurance; an ordinary profile never proves controlled-recipe qualification. Any incompatible informational format change needs a separately reviewed version change, not an implicit reinterpretation.

Rehearsal must verify the rebuild-check file itself against its SLSA provenance subject using the same reusable signer/source pins as the binary, then verify release membership and its checksum. It must also run the existing binary provenance/SPDX commands unchanged after adding the subject. A real nondeterministic mismatch is a stop-and-report event: retain bounded diagnostics and ask for a decision. Never weaken byte comparison, normalize regions or add exclusions to obtain a match.

## Compatibility and rehearsal record

These sections moved here from the Phase 1 compatibility record, which Phase 1
removed after its scope was narrowed. Rehearsals up to `.20` covered the earlier
seven-binary release (30 files with the rebuild check).

### Phase 1 narrowing merge

Merging the narrowed Phase 1 removes the three Axum binaries, the Rust source
audit and the publisher's post-publish re-download. The release is now four
binaries plus the rebuild check and `SHA256SUMS`: 18 files, with `SHA256SUMS`
covering the other 17. The publisher verifies every signing bundle before it
creates a draft, including the rebuild check against the Fastly provenance
bundle.

Rehearsal `.26` of the merge commit `3ddcd1996` failed the canonical Fastly
build before anything was signed: Phase 1 had stopped writing
`cargo-metadata.json`, which the ordinary capture reads. `9eeccbb88` writes it
again and tests that the capture reads the file the acquisition step writes.
Rehearsal [`.27`](https://github.com/prk-Jr/trusted-server/actions/runs/41987840964)
at `9eeccbb88` then passed all 13 jobs: both Fastly builds matched
(`8fc36bb49a06f2182512d57976b26ee763f0ef06e9a40635f24540ef7c7980eb`; canonical
399s, reproduction 443s, comparison 129s), the publisher verified every bundle
and the rebuild check before creating the release, and it published 18
immutable prerelease assets. The guide's verification block passed against
the published release. Offline verification and the local Viceroy runtime
check were not repeated.

### Approved Phase 2 rebuild-check compatibility

Gate 1's ordinary-release Fastly adapter was approved on 2026-10-26; implementation follows a separately reviewed concrete plan. The asset name `trusted-server-fastly.wasm.rebuild-check.json` stays fixed. Its version-1 `ordinary-release-fastly-byte-equality-v1` profile proves only the documented ordinary A/B comparison, with `release_qualification:false`. Consumers must check `profile`; matching digests do not imply controlled-recipe qualification.

The approved build-side normalization amendment maps both actual checkout roots to `/trusted-server` and both target roots to `/cargo-target`, passing the identical four-map target-scoped Cargo argument pair to each build. Filesystem roots still differ; normalized compiler source-path identity deliberately does not. The exact argument pair is retained before compilation, checked against both observations and kept out of the public receipt and semantic recipe identity. The recipe identity includes the stable normalization policy. Complete final bytes retain their debug/name sections and must contain no physical root. Comparison excludes and rewrites no bytes; controlled tool/material/path closure and alternate-user coverage remain unqualified. A remaining real mismatch stops the attempt.

Phase 3 will index and digest this same companion. Later controlled-recipe evidence may supersede the ordinary profile in the same filename using a distinct profile value while preserving compatible version-1 source, outcome and digest joins. Incompatible informational changes need an explicit reviewed version change; neither a filename change nor silently stronger interpretation is allowed.

The Fastly provenance attestation adds the rebuild-check subject while retaining the existing reusable signer identity and bundle name. Rehearsal must prove that existing binary verification commands pass unchanged and separately verify the rebuild-check's provenance, release membership and checksum. After merge, every push to `main` and every `v*` tag adds a second Fastly build and comparison before signing. A mismatch prevents release; retain bounded diagnostics and ask for a decision without exclusions or weaker comparison.

#### Manual Fastly package directory headers

[CI at `a8f8bcb81`](https://github.com/IABTechLab/trusted-server/actions/runs/40469201263/job/122071972950) confirmed that the pinned Fastly CLI emits exactly two zero-sized type-5 directory headers named `example-app` and `example-app/bin`, without trailing slashes. The other two headers are the regular wasm and manifest files; prefixes and link names are empty, and no hidden nonzero padding or metadata headers were present.

The private read-only package inspector accepts those two known directory names with or without one trailing slash, only with type 5 and size zero. It deduplicates their logical identities, so mixing the two spellings cannot bypass duplicate rejection. File names/types, links, prefixes, metadata rejection, four-member requirement, expansion/trailer bounds and independent manifest/wasm hashes remain unchanged. Other path aliases remain rejected. This transport compatibility correction grants no recipe, runtime, official-release or deployment qualification.

#### Phase 2 ordinary metadata and closed destination policy

Phase 2 validates the ordinary cohort strictly. Since Phase 1 narrowed the release to four binaries, the ordinary cohort, the retained controlled recipes, the consumer policy and the index schema all cover the same four artifacts in fixed order. A row outside that set, such as a former Axum row, is refused as unknown. Earlier Phase 2 revisions validated seven ordinary rows; the rehearsals below that name seven binaries or 30 files record that earlier set.

The carried tools and consumer policy name GitHub CLI 2.97.0 as a destination pin. Current operator instructions use 2.102.0, and the ordinary publisher records the installed version. Qualification pins, controlled reproducibility profiles and the destination GitHub CLI pin are not enforced by the ordinary producer. Its existing publication, signing and verification checks remain in force.

#### Controlled JS observer gap

Controlled JS preparation does not support the current npm lock: the frozen observer admits esbuild 0.27.3, while the lock selects 0.28.2. The observer is ported unchanged. Ordinary release builds and the Gate 1 Fastly A/B adapter do not use it. Adapting the observer belongs to the gated controlled-recipe work and requires its own review of the esbuild change.

The full collector test stays ported with an explicit skipped reason naming both versions; no CI job claims it passes. A cheap configuration guard rejects `TRUSTED_SERVER__RELEASE_JS_TESTS=1` against an unsupported lock, and a pure regression test checks that refusal. Existing observer fixture tests and the ordinary/checked-prebuilt build fixtures remain separate.

Unlike the frozen controlled producer, ordinary release mode may select neither prebuilt input and build fresh private bundles with npm. Explicit prebuilt release selection still requires both the absolute bundle directory and inventory, with source/hash, strict-copy and race checks unchanged. Phase 1 continues to refuse all prebuilt overrides. The build-input audit allows the inventory metadata name only in the exact JS build script, never in other build scripts or compile-time application reads.

#### Online SBOM records after a signing rerun

Reruns or retries of the signing workflow can leave more than one valid SBOM attestation for the same binary digest, in no guaranteed order. In rehearsal `.10` at `3ccfe29715a073bd37010ea86037b9c8f89801c7`, the second attempt passed archive binding and equality, then refused the existing immutable release. Both signing attempts remain visible online; their SPDX creation timestamps and document namespaces differ, while the published release and its assets remain unchanged.

Online consumers keep every repository, workflow/certificate, tag/ref, source/signer digest and predicate pin. Only after `gh attestation verify` succeeds may `jq -e --slurpfile document <asset>.spdx.json` require `any(.[]; .verificationResult.statement.predicate == $document[0])`. The document must first pass authenticated release/asset verification. This selects a predicate exactly equal to the published SPDX without choosing the first/newest record. An empty result or no equal predicate evaluates false and exits non-zero. Published-bundle checks retain their strict single-result comparison; no workflow or signing behavior changes.

#### Exact-head rehearsal evidence

| Phase | Tag   | Implementation source                      | Evidence                                                                                                                                                                                                                                                                                              |
| ----- | ----- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1     | `.12` | `ab6e5493636fd3d1db4b22a071189070f90a189a` | [Passed: 29 immutable prerelease files and full independent verification](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6440880360)                                                                                                                                             |
| 2     | `.10` | `3ccfe29715a073bd37010ea86037b9c8f89801c7` | [Passed: 30 files, unchanged binary verification and real companion](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6438603951); [rerun-all transfer passed, existing immutable release refused](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6439423827) |
| 2     | `.13` | `770a425df4b28caaaef68dfd930bceabd9928046` | [Passed: full 30-file publication and independent verification of the merged implementation](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6441704601)                                                                                                                          |

Phase 1 `.11` failed at the single draft-create POST with HTTP 403; no release or draft existed. Fresh `.12` placed the same reviewed source at the fork Phase 1 branch head as well as a new lightweight tag and passed, without source, settings or token-permission changes. The failure log did not retain the response body, so the exact permission cause remains unconfirmed. The [failed attempt](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6439851715) remains recorded and its tag was not reused. Phase 1's bundle-based single-result examples remain unchanged.

The Phase 2 `.10` reproduction job took 293 seconds and comparison 91 seconds, overlapping native builds. Its [online-SBOM positive and one-byte-negative checks](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6439619901) passed against fully verified pinned results. The negative copy remained valid JSON, changed exactly one byte, and returned false with `jq -e` exit 1; the authenticated published document remained untouched. The merged head's `.13` reproduction took 378 seconds and comparison 116 seconds, also overlapping native builds. These records identify their exact implementation SHAs; the completion commit `2ed1b941a212aeb27e08f8d1e27cb8353f40f535` was docs-only since rehearsed `770a425df4b28caaaef68dfd930bceabd9928046`.

#### Phase 2 merge after rehearsal .13

This merge joins Phase 2 parent `2ed1b941a212aeb27e08f8d1e27cb8353f40f535` with updated Phase 1 parent `20152315c01dd5fddcfc84b969f8e9993b0bfc69`. Phase 1 carries the main integration rehearsed in `.15` at `68eef902a8bddd9024676cc3806fa5377ed67fee`, followed by its docs-only evidence update.

The source-changing Phase 2 integration is `81020c0929bdc29ee4102e6516b7a75cc2cfe810`, a **no-release-machinery merge**: application code from main changes binary inputs, while release workflows, producer scripts, build facts, manifests/lock and tool pins retain their Phase 2 values. No new Phase 2 rehearsal is performed, as requested. `.13` stays attributed exclusively to `770a425df4b28caaaef68dfd930bceabd9928046`; its release is not evidence for binaries from this new merge head. Gate 1 activation and all closed qualification/design gates remain unchanged.

This follow-up records only the exact merge SHA and is docs-only after `81020c0929bdc29ee4102e6516b7a75cc2cfe810`. The application changes since rehearsed `770a425df` remain unrehearsed.

#### Newer stack trunk integration

After main advanced, GitHub's stack CI evaluated Phase 2 against the newer trunk and found a conflict in `prebid_bundle.rs`. Integration `7d551777daeaee09e24bce9bf4f96edac6e5e0df` merged main `054c7b777ce67db34744cb1f4100b14e59c4bc5a` into Phase 2 parent `3ae0e191f321ed1d796af0106b9043804d7af452` only. The resolution retains both the upstream Unix permissions-test import and Phase 2's resource-discovery tests; upstream security fixes remain intact.

Phase 1 stays at docs head `20152315c01dd5fddcfc84b969f8e9993b0bfc69`, with `.15` still attributed to `68eef902a8bddd9024676cc3806fa5377ed67fee`. The additional Phase 2 integration changes application inputs, with no release-machinery changes and no new Phase 2 rehearsal. `.13` remains exclusively at `770a425df4b28caaaef68dfd930bceabd9928046`.

This exact-SHA record is docs-only after `7d551777daeaee09e24bce9bf4f96edac6e5e0df`; the later application inputs remain unrehearsed.

### Phase 2 final review follow-up (2026-10-28)

The parent tracking issue is #1235; the earlier separate-issue draft is historical. Phase 2 now has its own [spec and review log](../specs/2026-10-28-1235-binary-attestations-phase2-design.md), distinct from the Phase 1 review rounds. Current readiness work does not expand historical `.13` attribution. Fresh rehearsal `.16` subsequently passed at `5fa3843364f247fdc261c438ab9257323637a42c`, as recorded below.

The structured failure diagnostic completion preserves all release contracts: canonical JSON is capped at 4,096 bytes, classified at actual typed checks and retained only as `ordinary-diagnostics/ordinary-comparison-diagnostic.json` in the failed comparison CI artifact, alongside bounded stderr. Unobserved facts are explicitly `unavailable`. A failure writes no matched companion; a diagnostic-writer error remains a failed job. These diagnostics are never signed or published. The existing companion filename, profile and exact-byte comparison are unchanged.

The final risk-based security review also identified a dormant limitation in macOS survivor enumeration: a saturated PID buffer can be treated as a complete snapshot, and failed per-process identity queries become nonmatches. This is not part of the ordinary Fastly release path; it must be addressed before any later macOS containment qualification opens. Darwin startup currently provides scrubbed/supervised host execution, not filesystem/network confinement. Qualified selectors remain uninhabited; this batch does not enable or modify that dormant path.

### Phase 2 historical rehearsal .16

Rehearsal `.16` passed at `5fa3843364f247fdc261c438ab9257323637a42c` with all 18 jobs successful. It covers the later application inputs and structured-diagnostic completion; the historical `.13` remains exclusively at `770a425df4b28caaaef68dfd930bceabd9928046`. The subsequent `fb4843355` completion record was docs-only since that exact rehearsed implementation; the later process-supervision correction is a separate source change.

The [full transcript](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6467368408) records 30 immutable prerelease assets, not Latest; 29 checksum entries; all seven binaries' SLSA/SPDX verification with reusable workflow and exact certificate/source/tag pins, online and offline; 14 SPDX equalities plus an exact one-byte negative; the genuine companion's provenance, source-owned recipe/lock hashes and all four upload/API/job joins; release download, membership and byte readback; macOS 13.0 metadata, CLI full commit, Axum non-loopback smoke and pinned Fastly pack/validate/inspect/cmp without deployment. Reproduction took 4m29s; comparison 1m26s, overlapping native builds.

An initial independent download hit a connection timeout. The 28 complete local files were checked against API sizes/digests; the two missing files were recovered from the same immutable release with bounded read-only retries. The workflow, release and lightweight tag were not reused or changed. Hosted source tests also passed the real negative comparator's structured `mismatched` / `bytes` assertions and later-stage sentinel refusal.

### Phase 2 review follow-up and rehearsal .20

**Verification at `5163241b9`:** Rehearsal `.20` passed at exact reviewed source `5163241b947b1aa335987bd832b9205e4364e4bb`: [all 18 jobs](https://github.com/prk-Jr/trusted-server/actions/runs/40812416995) succeeded, publishing 30 immutable prerelease assets, not Latest. Independent [transcript parts 1](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487647974), [2](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487649518) and [3](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487649861) verify all release memberships and checksums, both predicates and reusable signer identities online/offline for all seven binaries, 14 SPDX equalities and negative controls, companion joins, native smoke and pinned Fastly packaging. The exact published Wasm also passed the local Viceroy 0.21.0 publisher/JS/source-header/logging check, with zero build-fact lines and owned-listener cleanup. Current-source CI passed 29 checks with four expected skips. This completion record is docs-only since rehearsed `5163241b9`; its subsequent hosted checks are assessed separately. Keep #1449 draft, without reviewers.

The [`.16` runtime supplement](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6471330921) closes the original missing execution evidence for unchanged published bytes at `5fa384336`: three actual publisher responses and the injected JS route returned 200 with that full source header under pinned Viceroy 0.21.0. Actual request/response logs were visible and contained zero build-facts lines. Wrong-source and log-detector controls passed, the binary digest stayed unchanged, and owned listeners stopped. This is local Viceroy evidence, not deployed Fastly/browser qualification. The packaging inspector's `runtime_validated:false` still correctly describes that inspector; the separate execution check does not rewrite its output or any immutable asset.

The optional process finding was confirmed and corrected in `927109fa1`: initial group killing occurs while the unreaped leader pins its identity, and all post-reap confirmation uses read-only signal zero. Numeric reuse can cause a bounded false cleanup failure but cannot receive another destructive signal. Five pure regressions and real hosted Linux/macOS process tests pass. Shared metadata consistently identifies Phase 3 draft #1463.

Fresh `.17` and `.18` preserve the seven-binary cohort and reviewed source `36454c526`, but their ARM64 macOS jobs never acquired runners. This is external scheduling evidence, not an established code failure and not a successful publication rehearsal. Both tags are preserved. Existing `.16` signed/publication proof stays attributed only to its original source. No approval or implementation for later qualified, SPDX/index or consumer gates follows from these corrections.

Fresh `.20` closes the post-correction rehearsal gate at `5163241b947b1aa335987bd832b9205e4364e4bb`. Its Wasm SHA-256 is `4fe7204f17df07a392f4e4b64f31eebf3b02179e57abee689ab0094990d0a572`. Reproduction took 282 seconds; comparison 122 seconds. Attempt `.19` at that same source had the same ARM64 runner-acquisition cancellation, with no release ([failure record](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487087085)); it is not claimed as successful. Historical `.16`, `.17` and `.18` retain their existing source/result attribution. No later production/qualified or SPDX/index/consumer gate is opened.
