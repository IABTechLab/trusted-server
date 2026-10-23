# Phase 1 compatibility audit and approved alignment

**Audit baseline:** reviewed #1262 at `122b78664b1235c717e9208fd200f9a9b0c335df` versus #1435 at `4dd217125c6e38055e4bcb2cba32c0b95e739914`. The table records the audit of those commits. The approved status below supersedes earlier implementation suggestions in the audit; work in progress is not a claim of completed release qualification. The published SBOM format is **SPDX 2.3**. Rehearsal `.4` passed at `922c2211c`; the rehearsal history below records earlier outcomes.

**Conclusion:** the original Phase 1 signer path and CycloneDX public format would have required a later breaking policy replacement. Fix those now. Phase 1 is the first implementation slice of the existing contract: retain its public names, signer/caller distinction, SPDX semantics and original policy files. Introduce no competing release-index schema or weakened consumer policy.

Frozen source references below mean:

- [Spec](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md), especially R-COHORT/R-PLATFORM/R-SOURCE/R-SIGN/R-PUBLISH/R-INDEX/R-EVIDENCE.
- [Plan Tasks 15–16](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/plans/2026-10-13-attested-release-implementation.md#task-15-orchestrate-independent-builds-and-isolated-signing).
- [Cohort](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/scripts/release/release-cohort.json), [tools](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/scripts/release/release-tools.json), [consumer policy](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/release-policy.json).
- [Index v1 schema](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/specs/schemas/release-index-v1.schema.json), [distribution names](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-release/src/distribution.rs#L55), [consumer companion rules](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/src/release/documents.rs#L236), [SPDX joins](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/crates/trusted-server-cli/src/release/evidence/joins.rs#L188).

## Approved scope after audit

This document records the alignment decisions. Carried requirements remain
unchanged; the phasing sections describe this slice and its rehearsal outcome. The frozen source remains the destination contract. The pre-correction
Phase 1 audit baseline is `4dd217125c6e38055e4bcb2cba32c0b95e739914`; the
following corrections are implemented in this slice, with verification and
rehearsal tracked in the PR description at its current commit. Signing and
immutable publication rehearsal `.4` passed at `922c2211c`; see the
[full verification transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6346483024).

The current correction aligns the local input-free `release-build.yml` signer
with the `release.yml` caller/publisher, real SPDX 2.3 documents, all eight
named bundles, the existing four binary names and `SHA256SUMS`: 17 assets.
Provenance covers each real binary and SPDX document; no missing evidence
subjects, index, inventory, recipe or rebuild results are fabricated. Exact
artifact selection and signing-approved digest checks replace wildcard merging.
The publisher lists all releases including drafts, refuses a matching tag,
retains the created release ID for mutations, performs no automatic mutation
retries and verifies immutable membership after publication.

The tools and consumer policy files remain unchanged. The cohort adds the
three descriptive Axum entries below; all three policies still have **no Phase 1
readers**. Checked cohort selection, a supplemental tool policy, environment
normalization/remapping, stricter tag/source validation, uploaded-byte re-download,
macOS 13 targeting, full platform qualification and CLI immutable build facts
are additive later work. The next batch adds UBI GNU glibc 2.34 checks and
startup smokes, while retaining the macOS 15.0 deployment target; copying a
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
| SPDX document / predicate         | SPDX 2.3; `https://spdx.dev/Document/v2.3`                                                                                                               | CycloneDX 1.5 / `https://cyclonedx.org/bom`                                      | User selected SPDX: generate real SPDX 2.3 and restore the source predicate before publication.                                                                                 |
| SBOM companion name               | `<asset>.spdx.json` (distribution/consumer hardwired suffix)                                                                                             | `<asset>.cdx.json`                                                               | Align now; never rename CycloneDX bytes to SPDX.                                                                                                                                |
| SBOM root/binary join             | `SPDXRef-DOCUMENT`; exactly one described root package and one `SHA256` checksum equal to executable bytes                                               | CycloneDX metadata hash                                                          | Validate real converted SPDX against the existing root/hash contract. Root ID spelling itself is not hardwired.                                                                 |
| SBOM predicate/document equality  | Executable subject; predicate structurally equals distributed parsed SPDX; file bytes independently hashed                                               | Executable subject is correct; no signed-predicate/file comparison               | Preserve subject semantics, reject malformed/duplicate JSON before signing, and verify the shipped SPDX against the attested predicate.                                         |
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

| Contract point                  | Frozen #1262 value                                                                                                                               | #1435 value at audit                                                         | Alignment / adoption                                                                                                                                                                                                                                                       |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Policy file paths               | `scripts/release/release-cohort.json`, `scripts/release/release-tools.json`, `crates/trusted-server-cli/release-policy.json` (`policy.rs:23–27`) | All absent                                                                   | Carry all three unchanged with no Phase 1 readers. Later checked adoption must not execute source-supplied commands.                                                                                                                                                       |
| Policy qualification            | Tools, environments and recipes remain `pending`, acquisition/digest fields null                                                                 | Ordinary hosted build                                                        | Preserve all pending/null state. A Phase 1 checksum is not a claim of authenticated Phase 2 qualification.                                                                                                                                                                 |
| Existing tool versions          | Rust 1.98.1, Node 24.20.0, npm 11.19.0, gh 2.97.0; attest 4.2.2 at `1e69f48acb82d1966a394da916b4c1698aa569d6`                                    | Rust/Node match; npm unchecked; ambient gh; attest matches                   | Preserve current tracked Rust/Node/action pins. Additional policy readers and exact gh/npm enforcement remain additive later work; original tool policy is unchanged.                                                                                                      |
| New SBOM tool acquisition       | No cargo-cyclonedx/converter entries; strict producer rejects unknown tool names                                                                 | Cargo generator version/hash hardcoded                                       | Supplemental tool-policy adoption is deferred. Keep existing Phase 1 acquisition checks and carry the original strict tools records unchanged.                                                                                                                             |
| Package/bin/target/features     | Exact cohort entries, default features, no extras                                                                                                | Package/target agree; bin implicit                                           | Preserve current fixed selections. A checked cohort reader and explicit-bin reconciliation are additive later work.                                                                                                                                                        |
| Profile and Cargo flags         | Release, debug 1, strip none; `--frozen`, structured messages, fixed jobs (`recipe/plan.rs:175–197`)                                             | Release/debug agree; `--locked`, online build, implicit bin/jobs             | Keep online locked acquisition/build as the current Phase 1 boundary. Structured output/jobs, explicit bin and source normalization are additive later work; frozen confined execution follows its reviewed owner.                                                         |
| Normalized environment          | C locale, UTC, source-commit epoch, immutable source facts, controlled AWS-LC/SQLite selection (`preflight/child.rs:82–115`)                     | Full SHA source fact only                                                    | Environment normalization and checked commit epoch are additive later work; no Phase 1 reproducibility claim.                                                                                                                                                              |
| Rust/C path remapping           | Stable `/trusted-server`, `/cargo-vendor`, `/cargo-home`, `/rust-tools`, `/cargo-target` roots (`recipe/remap.rs`)                               | None                                                                         | All additional remapping follows later owned inputs; it does not change names or verification identities.                                                                                                                                                                  |
| Fresh/private inputs            | Private prepared state and target; denied network; authenticated paths                                                                           | Fresh target; hosted HOME/CARGO_HOME/PATH and network                        | Keep fresh target and scrubbed environment; prepared/offline/sandbox/tool closure remains the reviewed later slice.                                                                                                                                                        |
| JS preparation                  | Checked prebuilt inventory, shared twice-built modules, no Vite env/config loading                                                               | Fresh npm per artifact; private Cargo output; no shared comparison           | Keep fresh npm/private Cargo outputs. Additional environment controls, authenticated inventory, two builds and shared contribution evidence remain later work; never invent equality claims.                                                                               |
| macOS baseline                  | Deployment target **13.0**, hosted `macos-15` / `macos-15-intel`                                                                                 | Same runners, deployment target **15.0**                                     | Keep current 15.0 setting. Lowering to the destination 13.0 floor and older-host runtime qualification are additive later work.                                                                                                                                            |
| Linux baseline                  | GNU builder maximum **glibc 2.34**, UBI9/Debian12/Ubuntu22 compatibility evidence                                                                | Ordinary Ubuntu24 build; 2.39 environment, older compatibility unestablished | Explicit temporary limitation if source GNU builder/sysroot cannot be adopted without deferred platform machinery. Do not claim 2.34 merely by copying policy. Lowering the future requirement to 2.34 broadens support without renaming assets or breaking Phase 1 users. |
| Runtime/startup/file inspection | Owning runtime/startup acceptance and exact target classification                                                                                | Build-only + config canaries                                                 | Additional binary type/architecture checks and full runtime qualification remain additive later work with the source inspection owner.                                                                                                                                     |
| CLI immutable build facts       | Source-controlled release/source/target/variant facts in CLI output (Task 4)                                                                     | Existing package-version CLI output                                          | Mark as additive later CLI metadata, preserving existing commands; do not substitute a different source-identity schema.                                                                                                                                                   |
| SBOM precision                  | Complete Cargo/native/JS composition and retained-byte joins                                                                                     | Conservative Cargo target/npm build-tree inventory; native/std/runtime gaps  | Keep gaps explicit in SPDX and docs. Enrich the same SPDX companion later; do not label the smaller inventory complete.                                                                                                                                                    |
| Assurance profiles              | Fastly `provenance-sbom-repeatable`; native `provenance-sbom` with full evidence                                                                 | No repeatability/full evidence                                               | Preserve destination profiles in cohort; do not emit an index/profile claim until its gates hold. Native reproduction remains a measurement; Fastly reproducibility is a later gate.                                                                                       |

### Publication contract and nonbreaking recovery plan

| Contract point                  | Frozen #1262 value                                                           | #1435 value at audit                                | Alignment / adoption                                                                                                                                                                                                            |
| ------------------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Publication owner / permissions | `release.yml`, contents-write only (Task 16)                                 | Matches                                             | Preserve; no signer/OIDC/settings credential in publisher.                                                                                                                                                                      |
| Artifact transfer               | Validated current-run outputs, exact bytes/sets (Tasks 12/15/16)             | Wildcard `release-*` merge                          | Fix now: exact named downloads in separate directories, signing-owned digest manifests, reject missing/extra/changed files before creation.                                                                                     |
| Draft lookup/recovery           | Published refused; matching draft resumable only after source/assets match   | Tag lookup + refuse existing; no full draft listing | List all releases including drafts and refuse every matching tag in Phase 1. Capture created ID for mutations and identity-bound verification. Later matching-draft resume is an additive recovery capability.                  |
| Prepublication validation       | Upload, re-download and validate intended set                                | Upload then publish                                 | Upload responses already validate size and SHA-256 digest before publication. Re-downloading uploaded bytes remains additive later work. Exact signing-approved local assets and mutations are bound to the created release ID. |
| Checks after publish            | Immutable=true; signed release/membership and exact intended source/bytes    | Immutable check and normal gh verification          | Preserve and bind checks to created release ID. `gh release verify` accepts tags, so bracket it with created-ID/tag correspondence checks rather than invent an ID attestation endpoint.                                        |
| Mutable/lost/unknown outcomes   | Controlled cleanup for confirmed mutable; read-only reconciliation otherwise | Loud failure, manual recovery                       | Planned narrower Phase 1 behavior: leave failed/unknown state for explicit inspection, no blind retry/deletion. Later owner adds safe reconciliation without changing immutable assets or user verification commands.           |
| Concurrency                     | Tag scoped; never cancel publishing run                                      | Same rule also applies to PR builds                 | Preserve tags; cancel superseded PR builds only.                                                                                                                                                                                |
| Public deployment route         | Verified raw Fastly bytes; prepared EdgeZero context/recovery later          | Manual clean package/member comparison              | Preserve manual verification/deployment while later adding prepared-payload APIs; #1235 does not wait for boundaries 5–6.                                                                                                       |

**Nonbreaking adoption rule:** existing `gh` checks remain valid when later releases add real companions, index and richer evidence. The full custom verifier is introduced for index-bearing releases; it must not reinterpret the first slice as having complete graph/reproducibility/qualification evidence. No fake index or placeholder successful companions will be published. The unavoidable missing-evidence/recovery/platform-qualification limits are explicit; signer path, cohort, SPDX predicate/names and reserved companion names are aligned before the next publication attempt.

### Approved seven-binary Axum extension

The next implementation batch deliberately extends the frozen cohort from four
to **seven binaries / 29 release files**, retaining all four original names.
Rehearsal `.4` at `922c2211c` proves only the original four-binary/17-file
implementation. The seven-binary batch requires fresh rehearsal `.5`; it is not
claimed rehearsed by the `.4` evidence.

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
kernel. The historical cohort identifier `phase1-fastly-cli` and its original
four entries remain unchanged. **No Phase 1 reader** enforces these records or
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
flags leave host SBOM-tool builds unchanged. No allocator or Rust source changes
are included; consider mimalloc/jemalloc for musl under load later.

Builds scan the final bytes for config canaries and check locked inputs are
unchanged. Before upload they check wasm magic/version, each CLI's existing
`ts 0.1.0` package-version output, GNU symbol ceilings and static-musl loader
requirements. GNU x86 binaries start on Debian 12-slim, Ubuntu 22.04 and UBI 9.6
minimal; ARM Axum starts on Debian 12-slim on the ARM runner; musl starts on
pinned Alpine and a scratch-like root filesystem with a read-only runtime CA
bundle. The locked adapter initializes a TLS client before dispatching requests,
so an entirely empty trust store fails even a health request. Every Axum smoke mounts
the runner's system certificates at `/ca.pem` and sets `SSL_CERT_FILE`; operators
must supply runtime trust data independently. No dynamic libraries are added.
Acquired builder/runtime image IDs
are logged. Axum has no help/version mode; its isolated, read-only, nonroot
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

The exact rehearsed implementation SHA above is retained even when later
commits change documentation only. This documentation update does not change
workflows, scripts, tests or release bytes.

The immutable `.2`, `.3` and `.4` releases remain intact; no drafts remain from
these rehearsals. No upstream tags, releases or repository settings were changed.
