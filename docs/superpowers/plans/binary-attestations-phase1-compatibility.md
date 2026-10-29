# Phase 1 compatibility audit and approved alignment

**Audit baseline:** reviewed #1262 at `122b78664b1235c717e9208fd200f9a9b0c335df` versus #1435 at `4dd217125c6e38055e4bcb2cba32c0b95e739914`. The table records the audit of those commits. The approved status below supersedes earlier implementation suggestions in the audit; work in progress is not a claim of completed release qualification. The published SBOM format is **SPDX 2.3**. Historical rehearsal `.5` passed at `937214a632f9387601dba94f996de78b2e6ee424`; the rehearsal history below records earlier outcomes.

**Conclusion:** the original Phase 1 signer path and CycloneDX public format would have required a later breaking policy replacement. Fix those now. Phase 1 is the first implementation slice of the existing contract: retain its public names, signer/caller distinction, SPDX semantics and original policy files. Introduce no competing release-index schema or weakened consumer policy.

Frozen source references below mean:

- [Spec](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md), especially R-COHORT/R-PLATFORM/R-SOURCE/R-SIGN/R-PUBLISH/R-INDEX/R-EVIDENCE.
- [Plan Tasks 15–16](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/plans/2026-10-13-attested-release-implementation.md#task-15-orchestrate-independent-builds-and-isolated-signing).
- [Cohort](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/scripts/release/release-cohort.json), [tools](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/scripts/release/release-tools.json), [consumer policy](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/release-policy.json).
- [Index v1 schema](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/specs/schemas/release-index-v1.schema.json), [distribution names](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-release/src/distribution.rs#L55), [consumer companion rules](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/src/release/documents.rs#L236), [SPDX joins](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/src/release/evidence/joins.rs#L188).

### Phase 1 review notes — rehearsal .9 passed

Phase 1 is **part of #1235**, pending D7 maintainer settings. These notes supersede
historical phase applicability, not the retained destination requirements.
Goals, Non-goals, R-COHORT and R-BOUNDS refer to the original four-artifact design;
this slice explicitly includes seven binaries / 29 files, including Axum and
Linux ARM64. Its original four public names stay unchanged; no v1 index is emitted.
The Node producer helpers and inline publisher are interim exceptions to the
retained Rust-producer constraint, replaced by slices 2.5 (recipes/inspection),
2.6 (composition/SPDX) and 2.8 (publication). The frozen Status and tool versions
below describe #1262, not current Phase 1 implementation status.

Current tool pins: Rust 1.98.1, Node 24.20.0, Fastly CLI 16.0.0, Viceroy 0.21.0;
verification uses GitHub CLI 2.102.0. The reusable release-build.yml is the signer. Rehearsals use GitHub CLI 2.102.0, Fastly CLI 16.0.0 and Viceroy 0.21.0; the carried destination spec/plan still mention GitHub CLI 2.97.0 and Fastly CLI 15.1.0. Those carried versions are unchanged.
[Rehearsal `.9`](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6364827112) passed at exact implementation SHA `149754d357901ec874eab6140e92bce3d8613085`. Subsequent review-head updates are **docs-only since rehearsed `149754d35`**. Rehearsal `.8` remains historical evidence for its own implementation. Rehearsal `.6` at `39a769c3e` failed after immutable publication despite successful independent verification; `.7` at `7a05940e2` was cancelled before signing/publication because GitHub could not assign a macOS ARM runner. The compatibility audit retains those failed outcomes.

Slice 2.3 immutable build facts are reused from introducing commit
`bfb697404551a28217af8906024506095bcd9559` and revision
`6d0efda7210983021ab3c9efe8a0a3190e9eb81b`, as retained at the frozen source.
Core's resolver and new CLI/Fastly owner build scripts validate
`TRUSTED_SERVER__RELEASE_BUILD=1` and the full lowercase source SHA.
Cargo supplies the owning target/package; features supply the variant. Tags and
run IDs stay external. CLI now labels package/source/target/variant; Fastly queues
the same immutable facts through existing startup diagnostics in reuse mode only; single mode retains the source response header without per-request build-fact logs. The source audit
permits exactly this release control, GIT_VERSION and Cargo-owned BUILD_TARGET,
not deployment values. CLI source-resource cleanup remains deferred.

Axum binding is runtime-only: `EDGEZERO__ADAPTER__HOST` defaults to loopback;
containers can set `0.0.0.0`, with runtime `EDGEZERO__ADAPTER__PORT` or the compatible `PORT` override. Both values are validated, and conflicting values fail startup. The smoke uses
bridge networking and reaches the process through a published host port.
TLS termination, graceful shutdown and request/resource limits remain follow-up.

Publication requires `RELEASE_PUBLICATION_ENABLED=true` only after maintainers
confirm immutable releases and the v\* tag ruleset. There is no settings token or
prepublication settings API call, following the reviewed signing boundary;
postpublication immutable=true readback fails loudly. Hyphenated tags are
prereleases and never Latest. Stable tags are normal releases with `make_latest: legacy`; GitHub selects Latest by version/date, so backports do not force Latest. Eligibility of
rc-branch tags is an unresolved maintainer decision; Phase 1 does not implement
the destination branch-ancestry/ruleset validator or authorize rc tags.

## Approved scope after audit

This document records the alignment decisions. Carried requirements remain
unchanged; the phasing sections describe this slice and its rehearsal outcome. The frozen source remains the destination contract. The pre-correction
Phase 1 audit baseline is `4dd217125c6e38055e4bcb2cba32c0b95e739914`; the
following corrections are implemented in this slice, with verification and
rehearsal tracked in the PR description at its current commit. Signing and
immutable publication rehearsal `.9` passed at `149754d357901ec874eab6140e92bce3d8613085`; see the
[full verification transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6364827112).

The current correction aligns the local input-free `release-build.yml` signer
with the `release.yml` caller/publisher, real SPDX 2.3 documents, fourteen
named bundles, seven binary names and `SHA256SUMS`: 29 assets. Each binary has
provenance and an SPDX predicate attestation; each signed SPDX predicate matches
its published companion. No missing evidence
subjects, index, inventory, recipe or rebuild results are fabricated. Exact
artifact selection and signing-approved digest checks replace wildcard merging.
The publisher lists all releases including drafts, refuses a matching tag,
retains the created release ID for mutations, performs no automatic mutation
retries and verifies immutable membership after publication.

The tools and consumer policy files remain unchanged. The cohort adds the
three descriptive Axum entries below; all three policies still have **no Phase 1
readers**. Checked cohort selection, a supplemental tool policy, environment
normalization/remapping, stricter tag/source validation and full platform qualification remain additive later work. Post-publication asset re-download and byte comparison against signed `SHA256SUMS` shipped and passed in rehearsal `.9`. This corrective batch adds immutable build facts and a verified macOS 13.0 deployment target; UBI GNU glibc 2.34 checks and
startup smokes remain. Copying a
destination policy alone never establishes its qualification requirements. Native reproduction is a later measurement, not a
Phase 1 release gate. Full Task 16 resume/reconciliation remains deferred.

The [operator guide](/guide/release-verification) describes the corrected
verification procedure and qualification limits. Its commands pin the reusable
signer and exact tag certificate in separate mutually exclusive flag checks.
Future companions and stronger verification remain additive; existing immutable
releases and approved tags are never overwritten or reinterpreted as complete
qualified evidence.

## Stable public contracts: audit and adoption

| Contract point                    | Frozen #1262 value                                                                                                                                       | #1435 value at audit                                                             | Alignment / adoption                                                                                                                                                            |
| --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Release caller                    | `release.yml`; local reusable call at the same source revision (R-SIGN, Task 15, consumer policy identity)                                               | `release.yml` owns all jobs                                                      | Keep caller/publication in `release.yml`; move build orchestration and signing into local input-free `release-build.yml`.                                                       |
| Signing workflow identity         | SAN/builder `https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/<tag>`                                           | Direct `release.yml` signer and guide pins                                       | Correct now; never broaden frozen policy to accept the wrong caller SAN. Fork rehearsal substitutes only the sandbox repository identity.                                       |
| Caller identity / parameters      | `buildConfigURI` names `release.yml`; external workflow `{ref, repository, path}`; no arbitrary commands/inputs (consumer `evidence.rs:128–140,213–240`) | No caller/signer distinction                                                     | Input-free `workflow_call`; preserve caller/source/run binding.                                                                                                                 |
| Workflow job IDs                  | No prescribed spelling; topology/identity is normative                                                                                                   | `build`, `sign`, `release`                                                       | Keep compatible job IDs; file ownership and permissions are the contract.                                                                                                       |
| Provenance predicate / build type | SLSA `https://slsa.dev/provenance/v1`; `https://actions.github.io/buildtypes/workflow/v1`                                                                | SLSA default from pinned action                                                  | Preserve; inspect actual rehearsal statements rather than supplying custom provenance.                                                                                          |
| Signer permissions                | Build read-only; signer contents-read/OIDC/attestations-write; publisher contents-write only; no artifact-metadata write                                 | Already split correctly                                                          | Preserve across reusable boundary; caller grants only needed maxima and called jobs narrow privileges.                                                                          |
| Run/source binding                | Same approved source, caller/signer revision and repository run/attempt (R-EVIDENCE)                                                                     | Same run, no custom evidence joins                                               | Preserve invocation shape now; full graph joins deferred, not substituted by metadata claims.                                                                                   |
| Exact four raw executable assets  | `trusted-server-fastly.wasm`, `ts-aarch64-apple-darwin`, `ts-x86_64-apple-darwin`, `ts-x86_64-unknown-linux-gnu`                                         | Same                                                                             | Preserve exact names and unarchived executable subjects.                                                                                                                        |
| Cohort identity / selection       | `phase1-fastly-cli`, schema 1, exact package/bin/target/default-feature/variant/role/capability combinations                                             | Names/packages/targets duplicated in YAML/shell; no cohort metadata              | Carry original cohort unchanged with no reader. Checked declaration-based selection is additive later work, never an arbitrary command interface.                               |
| SPDX document / predicate         | SPDX 2.3; `https://spdx.dev/Document/v2.3`                                                                                                               | CycloneDX 1.5 / `https://cyclonedx.org/bom`                                      | User selected SPDX: generate real SPDX 2.3 and restore the source predicate before publication. <!-- allow-domain: cyclonedx.org -->                                            |
| SBOM companion name               | `<asset>.spdx.json` (distribution/consumer hardwired suffix)                                                                                             | `<asset>.cdx.json`                                                               | Align now; never rename CycloneDX bytes to SPDX.                                                                                                                                |
| SBOM root/binary join             | `SPDXRef-DOCUMENT`; exactly one described root package and one `SHA256` checksum equal to executable bytes                                               | CycloneDX metadata hash                                                          | Validate real converted SPDX against the existing root/hash contract. Root ID spelling itself is not hardwired.                                                                 |
| SBOM predicate/document equality  | Executable subject; predicate structurally equals distributed parsed SPDX; file bytes independently hashed                                               | Executable subject is correct; no signed-predicate/file comparison               | Preserve subject semantics, reject malformed JSON before signing (duplicate-key rejection is not claimed), and verify the shipped SPDX against the attested predicate.          |
| Provenance bundle name            | `<asset>.provenance.sigstore.json`                                                                                                                       | Not shipped                                                                      | Save action bundle and ship under the existing name now.                                                                                                                        |
| SBOM bundle name                  | `<asset>.sbom.sigstore.json`                                                                                                                             | Not shipped                                                                      | Save and ship now; do not introduce a competing suffix.                                                                                                                         |
| Provenance subject set            | Executable, SPDX, build-evidence, build-recipe, rebuild-check (Task 15/R-EVIDENCE)                                                                       | Executable only                                                                  | Sign executable + real SPDX in this slice. Add only the real evidence/recipe/rebuild subjects when their owning slices land; do not fabricate companions.                       |
| Remaining per-asset names         | `<asset>.build-evidence.json`, `.build-recipe.json`, `.rebuild-check.json`                                                                               | Absent                                                                           | Reserve exact existing names; defer contents until the reviewed producers own them.                                                                                             |
| Release-wide names                | `release-index.json`, `release-index.provenance.sigstore.json`, `tsjs-inventory.json`, `SHA256SUMS`                                                      | Only `SHA256SUMS`                                                                | Preserve names; index/inventory remain additive later assets. No alternative Phase 1 index.                                                                                     |
| Release layout                    | Four binaries + six companions each + four release-wide assets = 32                                                                                      | Four binaries + four CDX + checksums = 9                                         | First corrected slice: 4 binaries + 4 SPDX + 8 saved bundles + checksums = 17. Later 12 evidence/recipe/rebuild and 3 index/inventory assets complete the same 32-asset layout. |
| Checksums                         | `SHA256SUMS` covers every other published asset, never itself                                                                                            | Correct smaller-set rule                                                         | Preserve rule, expanding the exact set as real companions are added.                                                                                                            |
| Index structural contract         | Unchanged closed v1 schema; `{name,size,sha256}` file records and six fixed companion keys                                                               | Schema retained, no index emitted                                                | Keep schema bytes unchanged; later emit the existing index, not a replacement schema.                                                                                           |
| Index source/policy joins         | Source/recipe/cohort commits equal; producer policy version/hash; bounded exact names/digests                                                            | No index or policy identity claim                                                | Defer these graph joins. No invented policy hash or assertion that the full source consumer accepts an index-less release.                                                      |
| Consumer policy / actions         | Official repository/IDs, readonly verify, fixed bounds and identities                                                                                    | Custom verifier absent                                                           | Carry source policy unchanged as destination contract; manual `gh` verification remains supported for this first slice.                                                         |
| Candidate qualification markers   | Candidate sentinel and `release_qualification:false` are explicitly rejected for official consumption                                                    | No candidate records published                                                   | Preserve this separation; never repurpose candidate documents as official evidence.                                                                                             |
| Tag syntax / object               | Strict schema SemVer, including prerelease/build metadata; lightweight official tags (R-SOURCE)                                                          | Any `v*`; guide discusses peeled SHA; first canceled rehearsal tag was annotated | Strict schema/tag-object validation is additive later work. Use a separately approved fresh tag for rehearsal; never reuse `.1`.                                                |
| Eligible source                   | Exactly `main`, actual remote ancestry/governance plus same-commit validation (R-SOURCE/Task 14)                                                         | Manual source approval; no trusted Task 14 execution                             | Preserve policy `main` unchanged with no reader. Remote ancestry/governance and required-job validation are deferred; maintainer source approval remains required.              |

### Build/policy contracts and explicit scope limits

| Contract point                  | Frozen #1262 value                                                                                                                               | #1435 value at audit                                                         | Alignment / adoption                                                                                                                                                                                               |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Policy file paths               | `scripts/release/release-cohort.json`, `scripts/release/release-tools.json`, `crates/trusted-server-cli/release-policy.json` (`policy.rs:23–27`) | All absent                                                                   | Carry all three unchanged with no Phase 1 readers. Later checked adoption must not execute source-supplied commands.                                                                                               |
| Policy qualification            | Tools, environments and recipes remain `pending`, acquisition/digest fields null                                                                 | Ordinary hosted build                                                        | Preserve all pending/null state. A Phase 1 checksum is not a claim of authenticated Phase 2 qualification.                                                                                                         |
| Existing tool versions          | Rust 1.98.1, Node 24.20.0, npm 11.19.0, gh 2.97.0; attest 4.2.2 at `1e69f48acb82d1966a394da916b4c1698aa569d6`                                    | Rust/Node match; npm unchecked; ambient gh; attest matches                   | Preserve current tracked Rust/Node/action pins. Additional policy readers and exact gh/npm enforcement remain additive later work; original tool policy is unchanged.                                              |
| New SBOM tool acquisition       | No cargo-cyclonedx/converter entries; strict producer rejects unknown tool names                                                                 | Cargo generator version/hash hardcoded                                       | Supplemental tool-policy adoption is deferred. Keep existing Phase 1 acquisition checks and carry the original strict tools records unchanged.                                                                     |
| Package/bin/target/features     | Exact cohort entries, default features, no extras                                                                                                | Package/target agree; bin implicit                                           | Preserve current fixed selections. A checked cohort reader and explicit-bin reconciliation are additive later work.                                                                                                |
| Profile and Cargo flags         | Release, debug 1, strip none; `--frozen`, structured messages, fixed jobs (`recipe/plan.rs:175–197`)                                             | Release/debug agree; `--locked`, online build, implicit bin/jobs             | Keep online locked acquisition/build as the current Phase 1 boundary. Structured output/jobs, explicit bin and source normalization are additive later work; frozen confined execution follows its reviewed owner. |
| Normalized environment          | C locale, UTC, source-commit epoch, immutable source facts, controlled AWS-LC/SQLite selection (`preflight/child.rs:82–115`)                     | Full SHA source fact only                                                    | Environment normalization and checked commit epoch are additive later work; no Phase 1 reproducibility claim.                                                                                                      |
| Rust/C path remapping           | Stable `/trusted-server`, `/cargo-vendor`, `/cargo-home`, `/rust-tools`, `/cargo-target` roots (`recipe/remap.rs`)                               | None                                                                         | All additional remapping follows later owned inputs; it does not change names or verification identities.                                                                                                          |
| Fresh/private inputs            | Private prepared state and target; denied network; authenticated paths                                                                           | Fresh target; hosted HOME/CARGO_HOME/PATH and network                        | Keep fresh target and scrubbed environment; prepared/offline/sandbox/tool closure remains the reviewed later slice.                                                                                                |
| JS preparation                  | Checked prebuilt inventory, shared twice-built modules, no Vite env/config loading                                                               | Fresh npm per artifact; private Cargo output; no shared comparison           | Keep fresh npm/private Cargo outputs. Additional environment controls, authenticated inventory, two builds and shared contribution evidence remain later work; never invent equality claims.                       |
| macOS baseline                  | Deployment target **13.0**, hosted `macos-15` / `macos-15-intel`                                                                                 | Same runners, deployment target **15.0**                                     | This corrective batch targets 13.0 and checks Mach-O CPU/load-command minimum OS. Actual older-host runtime qualification remains later work.                                                                      |
| Linux baseline                  | GNU builder maximum **glibc 2.34**, UBI9/Debian12/Ubuntu22 compatibility evidence                                                                | Ordinary Ubuntu24 build; 2.39 environment, older compatibility unestablished | Implemented: UBI 9.6 GNU builds, final-byte GLIBC ceiling 2.34, and digest-pinned Debian 12/Ubuntu 22.04/UBI 9 startup checks. Full compiler/native composition qualification remains deferred.                    |
| Runtime/startup/file inspection | Owning runtime/startup acceptance and exact target classification                                                                                | Build-only + config canaries                                                 | Additional binary type/architecture checks and full runtime qualification remain additive later work with the source inspection owner.                                                                             |
| CLI immutable build facts       | Source-controlled release/source/target/variant facts in CLI output (Task 4)                                                                     | Existing package-version CLI output                                          | Port the reviewed slice 2.3 immutable facts now, preserving the labelled source-identity schema.                                                                                                                   |
| SBOM precision                  | Complete Cargo/native/JS composition and retained-byte joins                                                                                     | Conservative Cargo target/npm build-tree inventory; native/std/runtime gaps  | Keep gaps explicit in SPDX and docs. Enrich the same SPDX companion later; do not label the smaller inventory complete.                                                                                            |
| Assurance profiles              | Fastly `provenance-sbom-repeatable`; native `provenance-sbom` with full evidence                                                                 | No repeatability/full evidence                                               | Preserve destination profiles in cohort; do not emit an index/profile claim until its gates hold. Native reproduction remains a measurement; Fastly reproducibility is a later gate.                               |

### Publication contract and nonbreaking recovery plan

| Contract point                  | Frozen #1262 value                                                           | #1435 value at audit                                | Alignment / adoption                                                                                                                                                                                                                                                                                                                                                    |
| ------------------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Publication owner / permissions | `release.yml`, contents-write only (Task 16)                                 | Matches                                             | Preserve; no signer/OIDC/settings credential in publisher.                                                                                                                                                                                                                                                                                                              |
| Artifact transfer               | Validated current-run outputs, exact bytes/sets (Tasks 12/15/16)             | Wildcard `release-*` merge                          | Fix now: exact named downloads in separate directories, signing-owned digest manifests, reject missing/extra/changed files before creation.                                                                                                                                                                                                                             |
| Draft lookup/recovery           | Published refused; matching draft resumable only after source/assets match   | Tag lookup + refuse existing; no full draft listing | List all releases including drafts and refuse every matching tag in Phase 1. Capture created ID for mutations and identity-bound verification. Later matching-draft resume is an additive recovery capability.                                                                                                                                                          |
| Prepublication validation       | Upload, re-download and validate intended set                                | Upload then publish                                 | Upload responses already validate size and SHA-256 digest before publication. Re-downloading bytes before publication remains destination work; post-publication asset re-download and byte comparison against signed `SHA256SUMS` are implemented and proven in rehearsal `.9`. Exact signing-approved local assets and mutations are bound to the created release ID. |
| Checks after publish            | Immutable=true; signed release/membership and exact intended source/bytes    | Immutable check and normal gh verification          | Preserve and bind checks to created release ID. `gh release verify` accepts tags, so bracket it with created-ID/tag correspondence checks rather than invent an ID attestation endpoint.                                                                                                                                                                                |
| Mutable/lost/unknown outcomes   | Controlled cleanup for confirmed mutable; read-only reconciliation otherwise | Loud failure, manual recovery                       | Planned narrower Phase 1 behavior: leave failed/unknown state for explicit inspection, no blind retry/deletion. Later owner adds safe reconciliation without changing immutable assets or user verification commands.                                                                                                                                                   |
| Concurrency                     | Tag scoped; never cancel publishing run                                      | Same rule also applies to PR builds                 | Preserve tags; cancel superseded PR builds only.                                                                                                                                                                                                                                                                                                                        |
| Public deployment route         | Verified raw Fastly bytes; prepared EdgeZero context/recovery later          | Manual clean package/member comparison              | Preserve manual verification/deployment while later adding prepared-payload APIs; #1235 does not wait for boundaries 5–6.                                                                                                                                                                                                                                               |

**Nonbreaking adoption rule:** existing `gh` checks remain valid when later releases add real companions, index and richer evidence. The full custom verifier is introduced for index-bearing releases; it must not reinterpret the first slice as having complete graph/reproducibility/qualification evidence. No fake index or placeholder successful companions will be published. The unavoidable missing-evidence/recovery/platform-qualification limits are explicit; signer path, cohort, SPDX predicate/names and reserved companion names are aligned before the next publication attempt.

### Approved seven-binary Axum extension

This implementation extends the frozen cohort from four
to **seven binaries / 29 release files**, retaining all four original names.
Rehearsal `.4` at `922c2211c` proves only the original four-binary/17-file
implementation. Rehearsal `.9` passed for the seven-binary implementation at
`149754d357901ec874eab6140e92bce3d8613085`; the `.4` evidence is not reused for the expanded cohort.

The exact cohort diff is three appended records, all default features, empty
extra features, `variant: default`, `assurance_profile: provenance-sbom`,
`role: server`, `bin: trusted-server-axum`, package
`trusted-server-adapter-axum`, and descriptive capability `axum-production`:

| Added name / target                                                           | Compatibility record                 |
| ----------------------------------------------------------------------------- | ------------------------------------ |
| `trusted-server-axum-x86_64-unknown-linux-gnu` / `x86_64-unknown-linux-gnu`   | `x86_64`, `linux-gnu`, `glibc-2.34`  |
| `trusted-server-axum-x86_64-unknown-linux-musl` / `x86_64-unknown-linux-musl` | `x86_64`, `linux-musl`, `static`     |
| `trusted-server-axum-aarch64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu` | `aarch64`, `linux-gnu`, `glibc-2.34` |

This adds production Axum distributions by explicit scope decision. `static`
means no dynamic libc/loader requirement, not qualification of every Linux
kernel. The cohort’s `eligible_branches: ["main"]` is a destination value, not enforced by Phase 1; rc-branch eligibility remains a maintainer decision. Phase 1 ships seven artifacts. The historical cohort identifier `phase1-fastly-cli` is a destination-only label for the original contract; it is not the current file count or a qualification claim. The Fastly `provenance-sbom-repeatable` profile is destination-only: Phase 1 makes no repeatability claim. Its original four entries remain unchanged. **No Phase 1 reader** enforces these records or
their destination assurance profiles/capability labels. `release-tools.json` and
consumer `release-policy.json` remain byte-for-byte unchanged. The workflow's
explicit seven selections own the Phase 1 builds; no supplemental tool file or
policy reader is introduced.

Every binary gets `<asset>.spdx.json`, `<asset>.provenance.sigstore.json` and
`<asset>.sbom.sigstore.json`; only actual actions/attest outputs are retained.
`SHA256SUMS` covers the other **28** files and never itself. Exact-name downloads,
signing receipts and release membership checks cover the entire 29-file set.
Signer identity stays at reusable `release-build.yml` with the exact tag ref.
Existing users' four asset names and verification commands continue to work.

GNU compilation runs in digest-pinned UBI 9.6 with pinned Rust installed through
rustup, explicit target installation and `--locked`; final ELF requirements above
glibc 2.34 fail. Node is the existing pinned distribution mounted read-only.
The builder's system packages are acquired through DNF and are not a fully
qualified or pinned native tool closure. Musl uses Ubuntu's exact `1.2.4-2`
`musl`, `musl-dev` and `musl-tools` packages, the explicit `musl-gcc` linker and
GCC 13 driver, whose actual version is logged. Target-specific C/linker/static
flags leave host SBOM-tool builds unchanged. No allocator changes are included; allocator/load qualification remains later work.

Builds scan the final bytes for config canaries and check locked inputs are
unchanged. Before upload they check wasm magic/version, each CLI's labelled package/source/target/variant output, GNU symbol ceilings and static-musl loader
requirements. GNU x86 binaries start on Debian 12-slim, Ubuntu 22.04 and UBI 9.6
minimal; ARM Axum starts on Debian 12-slim on the ARM runner; musl starts on
pinned Alpine and a scratch-like root filesystem with a read-only runtime CA
bundle. The locked adapter initializes a TLS client before dispatching requests,
so an entirely empty trust store fails even a health request. Every Axum smoke mounts
the runner's system certificates at `/ca.pem` and sets `SSL_CERT_FILE`; operators
must supply runtime trust data independently. No dynamic libraries are added.
Acquired builder/runtime image IDs
are logged. Axum has no help/version mode; its bridge-networked, read-only, nonroot
container receives only a fictional runtime envelope/secrets and must return
`200 ok` from its configured health router. This does not establish publisher
proxy/load qualification or complete native composition.

The carried **v1 index schema remains unchanged**: its closed four-entry graph
cannot represent this cohort. Phase 1 publishes no index or deferred companions.
Phase 2 must review a seven-entry schema/policy amendment including musl/ARM
compatibility and `axum-production` capability forms before reopening its custom
consumer. The [slice-map annotations](binary-attestations-slice-map.json) mark
producer/consumer `FIXED_ARTIFACTS`, `cohort_order`, descriptor slots, bounds,
qualification enums, candidate workflows and fixtures as **update for Axum**.
The full frozen graph would have 53 files for seven artifacts; Phase 1's 29 is
not a replacement for those deferred bounds. Richer companions and consumer
support remain additive later work; existing manual GitHub verification stays
valid.

## Rehearsal history

- `v0.0.0-rehearsal.1` at `4dd217125`: Fastly build passed; native builds
  were canceled after review. Signing and publication executed no steps, so
  the run produced no attestations or release. The original annotated tag remains
  unused; later rehearsal tags are lightweight and never reused.
- `v0.0.0-rehearsal.2` at `41896cb07`: all builds and signing passed and
  an immutable 17-asset release was published. The final API read failed because
  of a stale pooled connection after synchronous verification. Independent
  verification passed; see the [verification transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6341078344).
- `v0.0.0-rehearsal.3` at `ca902b6fe`: all builds, signing, publication
  and the final API read passed. Independent verification with GitHub CLI 2.102.0
  passed release verification, all 17 asset checks, both predicates and both
  signer identity checks for each binary, checksums and SPDX predicate equality;
  see the [full verification transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6343374330).

- `v0.0.0-rehearsal.4` at `922c2211c0cc7941bfc8ffa9f775ef8ac0971232`:
  all four builds, signing, immutable publication and post-publish checks passed.
  Independent GitHub CLI 2.102.0 verification passed all 17 asset checks,
  checksums, both predicates and signer checks, and SPDX equality. Offline
  verification also passed with OS-enforced network denial; pinned Viceroy 0.21.0
  served a configured page from the verified wasm with the exact source header.
  See the [online transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6346483024)
  and [offline/local runtime proof](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6346428668).
  Bounded read-only retries were present, but no transient retry was observed.

- `v0.0.0-rehearsal.5` at `937214a632f9387601dba94f996de78b2e6ee424`:
  all seven builds/signing jobs, immutable publication and post-publish checks
  passed. Independent GitHub CLI 2.102.0 verification passed all 29 asset checks,
  28 checksum entries, both predicates and signer checks for all seven binaries,
  and seven SPDX predicate comparisons. Offline verification passed under
  OS-enforced network denial; Viceroy 0.21.0 served a configured page from the
  verified wasm with the exact source header. GNU runtime image checks, native
  ARM startup and static-musl Alpine/scratch-like checks passed with explicit
  runtime CA data. See the [online transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6357357438),
  [offline proof](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6357351810) and
  [build/local runtime proof](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6357352104).

- `v0.0.0-rehearsal.6` at `39a769c3e3d6476395fa7a215bd0a5e74a760f3e`: failed after immutable prerelease publication at the release-attestation lookup. Independent verification passed all 29 assets, online/offline predicates and signer pins, SPDX equality and the manual Fastly route. [Failure and independent proof](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6360012578).

- `v0.0.0-rehearsal.7` at `7a05940e2f7b10c459bdd2d3949f265b755ce42c`: cancelled after the macOS ARM job could not acquire a hosted runner. The remaining build was cancelled; signing/publication executed no steps and no release was created. Its lightweight tag remains unchanged. [Failure record](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6361386927).

- `v0.0.0-rehearsal.8` at `7a05940e2f7b10c459bdd2d3949f265b755ce42c`: all seven builds/signing jobs, immutable prerelease publication and complete independent online/offline/runtime verification passed. All 29 assets, 28 checksum entries, both predicates and signer forms, online store lookup without bundles, SPDX equality and the pinned Fastly pack/validate/inspect/cmp route passed. [Full transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6362236955).

- `v0.0.0-rehearsal.9` at `149754d357901ec874eab6140e92bce3d8613085`: all seven builds/signing jobs, immutable prerelease publication and complete independent verification passed. The publisher found the release attestation available on its first check (no live availability wait), then re-downloaded and byte-compared all 29 assets. All bundle/online-store/offline predicates and signer pins, SPDX comparisons, checksums, macOS floor and CLI source checks passed. Axum smokes proved 0.0.0.0:8080 from outside each container and rejected primary/legacy port zero. Pinned Fastly pack/validate/inspect/cmp and Viceroy runtime checks passed, with visible no-Build-facts check/result. [Full transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6364827112).

The original Phase 1 batch was rehearsed at `149754d357901ec874eab6140e92bce3d8613085` in `.9`.
Later source updates and their exact rehearsal attributions are recorded below.

The immutable `.2`, `.3`, `.4`, `.5`, `.6`, `.8` and `.9` releases remain intact; no drafts remain. The `.7` tag has no release. No upstream tags, releases or repository settings were changed.

### Main merge and exact-head rehearsal

- `.12` passed at `ab6e5493636fd3d1db4b22a071189070f90a189a`. Its [verification transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6440880360) remains evidence for that source only.
- Main at `fd2bf10e709092a796ebad2f6de371b2ea8dfd56` was merged, without rebasing, into Phase 1 at `68eef902a8bddd9024676cc3806fa5377ed67fee`. [Exact-head CI passed](https://github.com/IABTechLab/trusted-server/actions/runs/40524031740), including all seven release builds; PR signing/publication were skipped. Release workflows, producer scripts, build facts and tool pins were unchanged by this merge; application code changed.
- `.14` at that merge failed because GitHub could not acquire a hosted ARM64 macOS runner. Six started builds passed; signing/publication were skipped, and no release or draft was created. Its tag remains untouched. See the [failure record](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6446482013).
- Fresh lightweight `.15` passed at `68eef902a8bddd9024676cc3806fa5377ed67fee`: [all 16 workflow jobs passed](https://github.com/prk-Jr/trusted-server/actions/runs/40536697334/attempts/1). Independent verification passed all 29 immutable prerelease assets (not Latest), 28 checksum entries, all seven binaries with both predicates and both signer identities online and offline, 14 SPDX comparisons, native CI smokes, macOS 13.0 checks and CLI source identity. See [transcript part 1](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6460657878) and [part 2](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6460658274).

The latest rehearsed Phase 1 implementation is `68eef902a8bddd9024676cc3806fa5377ed67fee` from `.15`. This evidence update is docs-only since that exact SHA; it does not extend `.12` to later source. No IAB tags, releases or settings changed. Both PRs remain draft with no reviewers.

### Phase 1 review boundary notes

The reusable caller declares the maximum signing permissions required by its
called workflow. GitHub does not allow a read-only caller to invoke a nested
workflow that declares greater permissions, even for conditionally skipped jobs.
Actual build/check jobs narrow permissions to contents-read; signing remains
tag-only and requires the publication enablement variable. Splitting a new build-only workflow is deferred to preserve this slice.

The configuration scanner covers literal/raw/concatenated macros and literal
build-script deployment prefixes. It is a best-effort source regression audit,
not Rust expansion or dataflow analysis: `include!`, raw byte strings (`br"…"` /
`rb"…"`), computed names, procedural macros and third-party build scripts are not
completely analyzed. Planted canary scans and
clean inherited environments provide additional checks, not hermeticity.

### Rehearsal .6 outcome

Rehearsal `.6` at `39a769c3e3d6476395fa7a215bd0a5e74a760f3e` failed after immutable publication at the release-attestation lookup. [Independent verification](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6360012578) passed for all seven binaries / 29 assets, online and offline checks, SPDX equality and the manual Fastly pack/validate/inspect/cmp route; this does not turn the failed workflow green. Fresh `.8` subsequently passed at `7a05940e2f7b10c459bdd2d3949f265b755ce42c`; see the [full transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6362236955).

The failed publisher check ran at 07:19:53.744 UTC; the independently verified release attestation has a trusted timestamp of 07:19:58 UTC. The pinned CLI hides underlying lookup errors, so the original HTTP status is unknown. The immutable prerelease and its 29 assets remain unchanged. Before cryptographic release verification, the availability check makes at most five release-attestation GETs, with 2s/5s/10s/20s backoff (37s total waiting). Only this endpoint treats HTTP 404 as no record yet; a valid HTTP 200 with no current-tag record may also wait. Each GET times out after at most 4s, and a separate 60s deadline bounds availability checking within the shared three-minute scheduling window. Every other non-200 status fails immediately. Malformed records, duplicate current-tag records, identity/source/asset mismatches and definitive cryptographic failures stop immediately. Well-formed records for other tags, such as an earlier prerelease of the same commit, are logged and ignored without cryptographic verification: their state does not affect this release, and a deleted or slow earlier release must not fail an already published one. Current-tag cryptographic verification is single-shot. Logs show every wait or availability on the first check. No mutation is retried.

### Corrective batch publication checks

Post-publish downloads use fresh temporary directories and bounded transient read retries. The complete 29-file inventory and every byte, including SHA256SUMS, must match the cryptographically verified release/handoff. Missing, extra or changed files fail immediately. The release-body verification commands are tested against the operator guide’s canonical block. Axum uses the pinned EdgeZero resolver with every warning fatal; ports must be 1–65535, and primary `EDGEZERO__ADAPTER__PORT` and legacy `PORT` must agree. Container smokes use 0.0.0.0:8080 with host-side reachability.

### Axum diagnostic correction

Invalid host and port errors now name the actual supplied environment variable
and state that startup is refused. They no longer forward EdgeZero's fallback
warnings. Each port alias is still validated with the pinned EdgeZero resolver;
existing defaults, accepted values and conflict rejection are preserved.

This correction and the current-main integration changed the binary inputs.
Rehearsal `.9` remains evidence for its recorded implementation only; `.12` and
the fresh main-merge rehearsal `.15` are attributed separately above.

## Approved Phase 2 rebuild-check compatibility

Gate 1's ordinary-release Fastly adapter was approved on 2026-10-26; implementation follows a separately reviewed concrete plan. The asset name `trusted-server-fastly.wasm.rebuild-check.json` stays fixed. Its version-1 `ordinary-release-fastly-byte-equality-v1` profile proves only the documented ordinary A/B comparison, with `release_qualification:false`. Consumers must check `profile`; matching digests do not imply controlled-recipe qualification.

The approved build-side normalization amendment maps both actual checkout roots to `/trusted-server` and both target roots to `/cargo-target`, passing the identical four-map target-scoped Cargo argument pair to each build. Filesystem roots still differ; normalized compiler source-path identity deliberately does not. The exact argument pair is retained before compilation, checked against both observations and kept out of the public receipt and semantic recipe identity. The recipe identity includes the stable normalization policy. Complete final bytes retain their debug/name sections and must contain no physical root. Comparison excludes and rewrites no bytes; controlled tool/material/path closure and alternate-user coverage remain unqualified. A remaining real mismatch stops the attempt.

Phase 3 will index and digest this same companion. Later controlled-recipe evidence may supersede the ordinary profile in the same filename using a distinct profile value while preserving compatible version-1 source, outcome and digest joins. Incompatible informational changes need an explicit reviewed version change; neither a filename change nor silently stronger interpretation is allowed.

The Fastly provenance attestation adds the rebuild-check subject while retaining the existing reusable signer identity and bundle name. Rehearsal must prove that existing binary verification commands pass unchanged and separately verify the rebuild-check's provenance, release membership and checksum. Merging Phase 2 activates the gate on the next real tag, adding a second Fastly build and comparison before signing. A mismatch prevents release; retain bounded diagnostics and ask for a decision without exclusions or weaker comparison.

### Manual Fastly package directory headers

[CI at `a8f8bcb81`](https://github.com/IABTechLab/trusted-server/actions/runs/40469201263/job/122071972950) confirmed that the pinned Fastly CLI emits exactly two zero-sized type-5 directory headers named `example-app` and `example-app/bin`, without trailing slashes. The other two headers are the regular wasm and manifest files; prefixes and link names are empty, and no hidden nonzero padding or metadata headers were present.

The private read-only package inspector accepts those two known directory names with or without one trailing slash, only with type 5 and size zero. It deduplicates their logical identities, so mixing the two spellings cannot bypass duplicate rejection. File names/types, links, prefixes, metadata rejection, four-member requirement, expansion/trailer bounds and independent manifest/wasm hashes remain unchanged. Other path aliases remain rejected. This transport compatibility correction grants no recipe, runtime, official-release or deployment qualification.

### Phase 2 ordinary metadata and closed destination policy

Phase 2 validates all seven current ordinary cohort rows strictly. The retained controlled recipes, consumer policy and index schema still cover four destination artifacts. Projection uses those four source-owned names in fixed order; the three ordinary Axum rows grant no controlled recipe or seven-asset index authority. The separate Phase 3 index amendment remains required. No carried cohort, tools, consumer-policy or schema bytes are changed by this split.

The carried tools and consumer policy name GitHub CLI 2.97.0 as a destination pin. Current operator instructions use 2.102.0, and the ordinary publisher records the installed version. Qualification pins, controlled reproducibility profiles and the destination GitHub CLI pin are not enforced by the ordinary producer. Its existing publication, signing and verification checks remain in force.

### Controlled JS observer gap

Controlled JS preparation does not support the current npm lock: the frozen observer admits esbuild 0.27.3, while the lock selects 0.28.2. The observer is ported unchanged. Ordinary release builds and the Gate 1 Fastly A/B adapter do not use it. Adapting the observer belongs to the gated controlled-recipe work and requires its own review of the esbuild change.

The full collector test stays ported with an explicit skipped reason naming both versions; no CI job claims it passes. A cheap configuration guard rejects `TRUSTED_SERVER__RELEASE_JS_TESTS=1` against an unsupported lock, and a pure regression test checks that refusal. Existing observer fixture tests and the ordinary/checked-prebuilt build fixtures remain separate.

Unlike the frozen controlled producer, ordinary release mode may select neither prebuilt input and build fresh private bundles with npm. Explicit prebuilt release selection still requires both the absolute bundle directory and inventory, with source/hash, strict-copy and race checks unchanged. Phase 1 continues to refuse all prebuilt overrides. The build-input audit allows the inventory metadata name only in the exact JS build script, never in other build scripts or compile-time application reads.

### Online SBOM records after a signing rerun

Reruns or retries of the signing workflow can leave more than one valid SBOM attestation for the same binary digest, in no guaranteed order. In rehearsal `.10` at `3ccfe29715a073bd37010ea86037b9c8f89801c7`, the second attempt passed archive binding and equality, then refused the existing immutable release. Both signing attempts remain visible online; their SPDX creation timestamps and document namespaces differ, while the published release and its assets remain unchanged.

Online consumers keep every repository, workflow/certificate, tag/ref, source/signer digest and predicate pin. Only after `gh attestation verify` succeeds may `jq -e --slurpfile document <asset>.spdx.json` require `any(.[]; .verificationResult.statement.predicate == $document[0])`. The document must first pass authenticated release/asset verification. This selects a predicate exactly equal to the published SPDX without choosing the first/newest record. An empty result or no equal predicate evaluates false and exits non-zero. Published-bundle checks retain their strict single-result comparison; no workflow or signing behavior changes.

### Exact-head rehearsal evidence

| Phase | Tag   | Implementation source                      | Evidence                                                                                                                                                                                                                                                                                              |
| ----- | ----- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1     | `.12` | `ab6e5493636fd3d1db4b22a071189070f90a189a` | [Passed: 29 immutable prerelease files and full independent verification](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6440880360)                                                                                                                                             |
| 2     | `.10` | `3ccfe29715a073bd37010ea86037b9c8f89801c7` | [Passed: 30 files, unchanged binary verification and real companion](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6438603951); [rerun-all transfer passed, existing immutable release refused](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6439423827) |
| 2     | `.13` | `770a425df4b28caaaef68dfd930bceabd9928046` | [Passed: full 30-file publication and independent verification of the merged implementation](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6441704601)                                                                                                                          |

Phase 1 `.11` failed at the single draft-create POST with HTTP 403; no release or draft existed. Fresh `.12` placed the same reviewed source at the fork Phase 1 branch head as well as a new lightweight tag and passed, without source, settings or token-permission changes. The failure log did not retain the response body, so the exact permission cause remains unconfirmed. The [failed attempt](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6439851715) remains recorded and its tag was not reused. Phase 1's bundle-based single-result examples remain unchanged.

The Phase 2 `.10` reproduction job took 293 seconds and comparison 91 seconds, overlapping native builds. Its [online-SBOM positive and one-byte-negative checks](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6439619901) passed against fully verified pinned results. The negative copy remained valid JSON, changed exactly one byte, and returned false with `jq -e` exit 1; the authenticated published document remained untouched. The merged head's `.13` reproduction took 378 seconds and comparison 116 seconds, also overlapping native builds. These records identify their exact implementation SHAs; the completion commit `2ed1b941a212aeb27e08f8d1e27cb8353f40f535` was docs-only since rehearsed `770a425df4b28caaaef68dfd930bceabd9928046`.

### Phase 2 merge after rehearsal .13

This merge joins Phase 2 parent `2ed1b941a212aeb27e08f8d1e27cb8353f40f535` with updated Phase 1 parent `20152315c01dd5fddcfc84b969f8e9993b0bfc69`. Phase 1 carries the main integration rehearsed in `.15` at `68eef902a8bddd9024676cc3806fa5377ed67fee`, followed by its docs-only evidence update.

The source-changing Phase 2 integration is `81020c0929bdc29ee4102e6516b7a75cc2cfe810`, a **no-release-machinery merge**: application code from main changes binary inputs, while release workflows, producer scripts, build facts, manifests/lock and tool pins retain their Phase 2 values. No new Phase 2 rehearsal is performed, as requested. `.13` stays attributed exclusively to `770a425df4b28caaaef68dfd930bceabd9928046`; its release is not evidence for binaries from this new merge head. Gate 1 activation and all closed qualification/design gates remain unchanged.

This follow-up records only the exact merge SHA and is docs-only after `81020c0929bdc29ee4102e6516b7a75cc2cfe810`. The application changes since rehearsed `770a425df` remain unrehearsed.

### Newer stack trunk integration

After main advanced, GitHub's stack CI evaluated Phase 2 against the newer trunk and found a conflict in `prebid_bundle.rs`. Integration `7d551777daeaee09e24bce9bf4f96edac6e5e0df` merged main `054c7b777ce67db34744cb1f4100b14e59c4bc5a` into Phase 2 parent `3ae0e191f321ed1d796af0106b9043804d7af452` only. The resolution retains both the upstream Unix permissions-test import and Phase 2's resource-discovery tests; upstream security fixes remain intact.

Phase 1 stays at docs head `20152315c01dd5fddcfc84b969f8e9993b0bfc69`, with `.15` still attributed to `68eef902a8bddd9024676cc3806fa5377ed67fee`. The additional Phase 2 integration changes application inputs, with no release-machinery changes and no new Phase 2 rehearsal. `.13` remains exclusively at `770a425df4b28caaaef68dfd930bceabd9928046`.

This exact-SHA record is docs-only after `7d551777daeaee09e24bce9bf4f96edac6e5e0df`; the later application inputs remain unrehearsed.

## Phase 2 final review follow-up (2026-10-28)

The parent tracking issue is #1235; the earlier separate-issue draft is historical. Phase 2 now has its own [spec and review log](../specs/2026-10-28-1235-binary-attestations-phase2-design.md), distinct from the Phase 1 review rounds. Current readiness work does not expand historical `.13` attribution. Fresh rehearsal `.16` subsequently passed at `5fa3843364f247fdc261c438ab9257323637a42c`, as recorded below.

The structured failure diagnostic completion preserves all release contracts: canonical JSON is capped at 4,096 bytes, classified at actual typed checks and retained only as `ordinary-diagnostics/ordinary-comparison-diagnostic.json` in the failed comparison CI artifact, alongside bounded stderr. Unobserved facts are explicitly `unavailable`. A failure writes no matched companion; a diagnostic-writer error remains a failed job. These diagnostics are never signed or published. The existing companion filename, profile and exact-byte comparison are unchanged.

The final risk-based security review also identified a dormant limitation in macOS survivor enumeration: a saturated PID buffer can be treated as a complete snapshot, and failed per-process identity queries become nonmatches. This is not part of the ordinary Fastly release path; it must be addressed before any later macOS containment qualification opens. Darwin startup currently provides scrubbed/supervised host execution, not filesystem/network confinement. Qualified selectors remain uninhabited; this batch does not enable or modify that dormant path.

## Phase 2 historical rehearsal .16

Rehearsal `.16` passed at `5fa3843364f247fdc261c438ab9257323637a42c` with all 18 jobs successful. It covers the later application inputs and structured-diagnostic completion; the historical `.13` remains exclusively at `770a425df4b28caaaef68dfd930bceabd9928046`. The subsequent `fb4843355` completion record was docs-only since that exact rehearsed implementation; the later process-supervision correction is a separate source change.

The [full transcript](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6467368408) records 30 immutable prerelease assets, not Latest; 29 checksum entries; all seven binaries' SLSA/SPDX verification with reusable workflow and exact certificate/source/tag pins, online and offline; 14 SPDX equalities plus an exact one-byte negative; the genuine companion's provenance, source-owned recipe/lock hashes and all four upload/API/job joins; release download, membership and byte readback; macOS 13.0 metadata, CLI full commit, Axum non-loopback smoke and pinned Fastly pack/validate/inspect/cmp without deployment. Reproduction took 4m29s; comparison 1m26s, overlapping native builds.

An initial independent download hit a connection timeout. The 28 complete local files were checked against API sizes/digests; the two missing files were recovered from the same immutable release with bounded read-only retries. The workflow, release and lightweight tag were not reused or changed. Hosted source tests also passed the real negative comparator's structured `mismatched` / `bytes` assertions and later-stage sentinel refusal.

## Phase 2 review follow-up and current-source rehearsal

**Current verification:** Rehearsal `.20` passed at exact reviewed source `5163241b947b1aa335987bd832b9205e4364e4bb`: [all 18 jobs](https://github.com/prk-Jr/trusted-server/actions/runs/40812416995) succeeded, publishing 30 immutable prerelease assets, not Latest. Independent [transcript parts 1](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487647974), [2](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487649518) and [3](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487649861) verify all release memberships and checksums, both predicates and reusable signer identities online/offline for all seven binaries, 14 SPDX equalities and negative controls, companion joins, native smoke and pinned Fastly packaging. The exact published Wasm also passed the local Viceroy 0.21.0 publisher/JS/source-header/logging check, with zero build-fact lines and owned-listener cleanup. Current-source CI passed 29 checks with four expected skips. This completion record is docs-only since rehearsed `5163241b9`; its subsequent hosted checks are assessed separately. Keep #1449 draft, without reviewers.

The [`.16` runtime supplement](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6471330921) closes the original missing execution evidence for unchanged published bytes at `5fa384336`: three actual publisher responses and the injected JS route returned 200 with that full source header under pinned Viceroy 0.21.0. Actual request/response logs were visible and contained zero build-facts lines. Wrong-source and log-detector controls passed, the binary digest stayed unchanged, and owned listeners stopped. This is local Viceroy evidence, not deployed Fastly/browser qualification. The packaging inspector's `runtime_validated:false` still correctly describes that inspector; the separate execution check does not rewrite its output or any immutable asset.

The optional process finding was confirmed and corrected in `927109fa1`: initial group killing occurs while the unreaped leader pins its identity, and all post-reap confirmation uses read-only signal zero. Numeric reuse can cause a bounded false cleanup failure but cannot receive another destructive signal. Five pure regressions and real hosted Linux/macOS process tests pass. Shared metadata consistently identifies Phase 3 draft #1463.

Fresh `.17` and `.18` preserve the seven-binary cohort and reviewed source `36454c526`, but their ARM64 macOS jobs never acquired runners. This is external scheduling evidence, not an established code failure and not a successful publication rehearsal. Both tags are preserved. Existing `.16` signed/publication proof stays attributed only to its original source. No approval or implementation for later qualified, SPDX/index or consumer gates follows from these corrections.

Fresh `.20` closes the post-correction rehearsal gate at `5163241b947b1aa335987bd832b9205e4364e4bb`. Its Wasm SHA-256 is `4fe7204f17df07a392f4e4b64f31eebf3b02179e57abee689ab0094990d0a572`. Reproduction took 282 seconds; comparison 122 seconds. Attempt `.19` at that same source had the same ARM64 runner-acquisition cancellation, with no release ([failure record](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6487087085)); it is not claimed as successful. Historical `.16`, `.17` and `.18` retain their existing source/result attribution. No later production/qualified or SPDX/index/consumer gate is opened.
