# Phase 2 Fastly release integration — Gate 1

**Status:** Approved on 2026-10-26 for the ordinary-release adapter, writer, transfer binding and additive provenance subject, subject to the conditions below. Write and independently review the concrete implementation plan before implementation. Continue the frozen-source port meanwhile.

**Goal:** Require a fresh Fastly A/B build comparison in the existing release path, publish an authenticated `trusted-server-fastly.wasm.rebuild-check.json`, and preserve the Phase 1 verification commands and identities.

## Decision requested

Approve or revise the ordinary-release comparison adapter below, including its explicitly narrower claim, receipt fields, transfer binding and additive signing inputs. This is a real equality gate for the existing ordinary release builds. It is **not** qualification of #1262's controlled recipes, sandbox, materials or alternate-user environment. If Phase 2 must ship the full controlled-recipe profile instead, defer wiring until its bootstrap/material/tool acquisition and unqualified execution authority have a separately approved design; do not open the existing qualified selectors to satisfy a deadline.

Gate 1 approval authorizes writing/reviewing the concrete implementation plan and then implementing this ordinary adapter within the approved design; a material design change requires renewed approval. No adapter code may land before that approval. Approval of this design alone does not approve a new trigger, IAB setting change, production/qualified selection or qualification resource. The integration must first be implemented and rehearsed in the fork. **Merging Phase 2 is the maintainer activation decision:** the next real tag enforces the Fastly gate, without a separate switch. Each release costs one additional fresh Fastly build plus comparison, and signing/publication waits for both builds. Budget up to the 90-minute build ceiling plus ten-minute comparison ceiling; observed added wall time must be reported from rehearsal. A mismatch means no release.

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

Inside `release-build.yml`, the fixed build cohort remains seven binaries. Canonical Fastly becomes a dedicated job so its upload-action outputs bind unambiguously to its artifacts; the other six binaries remain in the matrix. Add the reproduction/comparison dependency only for Fastly. Keep the existing `sign` job and matrix; add the Fastly comparison as a job-level dependency, so every signing entry waits for the Fastly gate. Failure of either Fastly build or comparison prevents signing/publication for the release. This adds no native reproduction gate. Native builds retain their existing success requirements; native reproduction differences or absence do not become a new gate. Do not emit native rebuild companions until their later approved phase.

For Fastly only, expand the exact unsigned input set from binary + SPDX to binary + SPDX + rebuild-check. Rehash all three inputs before/after signing and in the signed transfer receipt. The existing provenance attestation gains the rebuild-check as an additional subject; keep its bundle named `<asset>.provenance.sigstore.json`. Keep the binary-only SPDX attestation and `<asset>.sbom.sigstore.json` unchanged. All other assets' exact input/subject sets remain unchanged.

In `release.yml`, preserve exact-name downloads into separate directories and validate the expanded Fastly receipt before release creation. Add only `trusted-server-fastly.wasm.rebuild-check.json` to publication: **29 existing assets become 30**. `SHA256SUMS` continues to cover every other published asset and never itself. Existing binary, SPDX and bundle names, SLSA/SPDX predicate URIs, reusable signer workflow/certificate identity, source pins and Phase 1 verification commands continue to work unchanged.

Preserve single-shot listing/refusal, draft creation, uploads and publication by release ID; never resume or overwrite an existing tag/draft. Preserve prerelease/Latest behavior, immutable readback, strict release-attestation validation, bounded read-only retries, release membership checks and post-publish byte comparison. Expand exact asset membership/digest expectations to 30. Existing immutable releases are untouched.

## Failure behavior and bounds

- A/B build, measurement, source/tool/input join, transfer check, runtime acceptance or equality failure prevents the companion/signing/publication path. There is no downgrade to unmatched or canonical-only Fastly release.
- Cancellation/timeouts fail closed and clean up owned work; they do not yield a successful receipt. Bound each build to the existing 90-minute build-job ceiling; comparison to ten minutes and bounded input sizes. Preserve the existing publication/verification deadlines rather than granting unlimited new retries.
- Retain bounded failure evidence as CI artifacts with existing seven-day retention. Logs name failed checks and observed digests/IDs; never print full inherited environments or tokens.
- Native reproduction remains observational. A native build/startup failure still fails its existing release checks; an attempted native reproduction reports its actual result without blocking on byte equality.
- Before publication, failures may leave a refused draft requiring explicit human inspection; never retry a mutation or delete automatically. After publication, a verification failure means the release is already immutable: report its release ID/tag and exact read-only recovery commands. Use the existing recovery guide; do not attempt a replacement under the same tag.

## Verification and rehearsal plan

1. Pure tests: writer positive contract; duplicate/malformed/extra fields; missing observations; wrong source, role, tool/JS/recipe identity; byte/size/digest mismatches; canonical changed after comparison; exact 30-file membership; checksum coverage; no successful receipt on failure. Every negative test must reach the intended validation rather than failing on an unrelated missing input.
2. CI-only process tests: real fresh A/B builds, altered ambient settings, deliberate mismatch/cancellation/output bounds and exact current-run transfer selection. Keep local tests pure and explicitly selected with a nonzero guard. All existing Phase 1 tests remain, including mutation/retry failure paths.
3. Independent implementation and security review of ordinary authority, observations, transfer binding, permissions, receipt signing, cleanup and publish failure paths. Confirm no qualified/test-only constructor is newly reachable.
4. After upstream CI is green on the exact reviewed head, use the next unused sequential lightweight fork rehearsal tag under the standing approval. Never tag, release or change settings in IAB; changes to fork settings still require approval.
5. Positive rehearsal: all seven binaries build; show A/B source/input identity, measured environment variation and complete final-byte match; print the real receipt and verify its digest/provenance subject. Confirm 30 exact release assets, immutable and prerelease/not-Latest behavior, release download, checksums, all release memberships, both binary provenance/SPDX verifications offline and online with exact `release-build.yml` pins, and shipped SPDX equality. Repeat the manual pinned Fastly package/validate/inspect/byte comparison and runtime/source-header/no-request-facts-log checks without deploying.
6. Rehearsal failure proof: exercise a deterministic comparison mismatch in a CI-only negative fixture, proving no sign/publication dependent job runs. Do not push an intentionally altered source commit as a release rehearsal or add a debug bypass to the release path. A failed real rehearsal retains its tag/release and uses a fresh number only after fixing and obtaining exact-head green CI.
7. Post the transcript, tag/run link and pass/fail result on the Phase 2 PR. Keep it draft without reviewers. Do not claim the full controlled-recipe profile or Phase 2 readiness if only the ordinary bridge is approved/proven.

## Approval checklist

- [x] User accepts the ordinary-release adapter's narrower equality claim for this Phase 2 slice.
- [x] User approves the concrete informational writer/measurement/transfer contract and additive Fastly provenance subject.
- [x] User approves implementation after a concrete plan is written and independently reviewed within this design; qualification types/selectors and new triggers remain closed.
- [ ] Rehearse the exact reviewed head and report time/cost and unchanged Phase 1 verification before merge. Merging Phase 2 activates the gate on the next real tag; no separate switch.

**Current verification:** The supervision correction at `36454c526f70f145eb42a7994a8b6e74dc1df135` passed independent review and exact-head CI: 29 successful checks and four expected skips. [Rehearsal .17](https://github.com/prk-Jr/trusted-server/actions/runs/40710926728) and [rehearsal .18](https://github.com/prk-Jr/trusted-server/actions/runs/40714430928) could not acquire a macOS ARM64 runner; no successful current-source release rehearsal is claimed. A fresh successful cohort rehearsal remains required before readiness. This record changes documentation only after that implementation. Keep #1449 draft, without reviewers. Historical `.16` at `5fa384336` now has its [separate runtime supplement](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6471330921); that proof is not reassigned to the later correction. The approved design requirements above are unchanged.

## Approved forward-compatibility and mismatch conditions

The filename `trusted-server-fastly.wasm.rebuild-check.json` is permanent. Phase 3 indexes and digests these exact bytes as a rebuild-check companion. A later controlled-recipe implementation supersedes the `ordinary-release-fastly-byte-equality-v1` profile in the same asset, using a distinct profile value while retaining version-1 shared identity/outcome/digest fields where compatible. Consumers must check the profile before interpreting assurance; an ordinary profile never proves controlled-recipe qualification. Any incompatible informational format change needs a separately reviewed version change, not an implicit reinterpretation.

Rehearsal must verify the rebuild-check file itself against its SLSA provenance subject using the same reusable signer/source pins as the binary, then verify release membership and its checksum. It must also run the existing binary provenance/SPDX commands unchanged after adding the subject. A real nondeterministic mismatch is a stop-and-report event: retain bounded diagnostics and ask for a decision. Never weaken byte comparison, normalize regions or add exclusions to obtain a match.
