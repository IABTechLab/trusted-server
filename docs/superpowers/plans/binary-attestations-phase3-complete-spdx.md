# Gate 2 proposal: complete SPDX from owning build evidence

**Status:** Gate 2 design approved on 2026-10-28 for strict admission from retained, artifact-bound owning evidence. Plan only: production generation stays closed until all seven binaries pass. The [coverage matrix](binary-attestations-phase3-complete-spdx-implementation.md) and [full-cohort cost assessment](binary-attestations-phase3-full-cohort-feasibility.md) expose costly evidence gaps before any implementation; no code, qualification or rehearsal tag is authorized.

**New option for decision:** The user has not approved target-specific activation. The [issue-scoped all-seven build-derived Cargo/npm SBOM option](binary-attestations-phase3-issue-scoped-sbom-option.md) proposes an explicit narrower coverage amendment, with per-SBOM/guide limits and a separate estimate. It is not adopted; strict Gate 2 still governs and Gate 3 Steps 2–5 remain held.

**Historical reviewed baseline:** Phase 2 `8a797dbcb0467b867dfbcc571597110b9254d672`, tree-equivalent to `08e1c33b8` after application integration `7d551777daeaee09e24bce9bf4f96edac6e5e0df`. Rehearsal `.13` remains evidence only for `770a425df4b28caaaef68dfd930bceabd9928046`.

**Verification baseline:** [Historical Phase 2 `8a797dbcb` CI passed](https://github.com/IABTechLab/trusted-server/actions/runs/40654745582); signing/publication and manual negative canaries were skipped as configured.

**Current integration baseline:** Phase 2 `5fa3843364f247fdc261c438ab9257323637a42c`; [exact-head CI](https://github.com/IABTechLab/trusted-server/actions/runs/40664514307) and readiness work are tracked in #1449. Rehearsal `.16` covers this exact implementation; [full transcript](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6467368408). Phase 2 `fb4843355` is the later docs-only evidence record.

**Tracking:** Part of #1235.

## Approved design scope

The user approved strict admission of complete composition from retained, artifact-bound evidence, reusing the reviewed projections and renderer from [frozen source `122b78664`](https://github.com/IABTechLab/trusted-server/tree/122b78664b1235c717e9208fd200f9a9b0c335df). The approved direction is an ordinary-build capture approach that supplies equivalent ownership without pretending to be a controlled recipe. A concrete implementation plan and provider-coverage review must follow approval; current receipts cannot yet satisfy this contract.

The goal is a complete SPDX 2.3 build-composition document for each of the seven selected binaries. Completeness is relative to an explicitly approved input scope. It does not establish hermetic execution, independent reproduction, vulnerability absence or the contents of an operator's eventual runtime environment.

| Approach                                                 | Benefit                                                                               | Cost / disposition                                                                                                |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| **Strict admission from owning evidence — recommended**  | Preserves the reviewed complete-composition requirement and one shared implementation | Native/platform/tool evidence gaps must actually close before production switches                                 |
| Target-specific rollout                                  | Could admit a supported target earlier                                                | Requires a separate cohort/rollout decision; cannot silently omit incomplete members of the seven-artifact cohort |
| Port candidates while complete generation remains closed | Preserves useful diagnostics and source-port progress                                 | Does not complete Gate 2 or justify a complete-SBOM claim                                                         |

## What exists and what is missing

The frozen `crates/trusted-server-release/src/spdx.rs::generate_from_completed` always returns an error. It is a closed integration point. The existing `spdx/candidate.rs::candidate_from_completed` already joins four projections from one `RecipeCompleted`, verifies the selected root/source, reads the final bytes and renders a gap-bearing candidate. It rejects a gap-free candidate and does not convert to `GeneratedSpdx`.

Phase 2 owns `recipe/completed.rs` and the non-SPDX composition capture modules. Its completed build retains source/artifact identities, Cargo observations, JS contributions and measured tool/material context; accessors rehash retained inputs. Owning bytes is necessary but does not prove complete coverage.

| Evidence class                | Existing source                                                 | Admission requirement / current gap                                                                                                                              |
| ----------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cargo                         | `composition/{capture,origin,graph}.rs`                         | Actual compiler units, features, host/target roles and verified originals; metadata alone is insufficient                                                        |
| JavaScript/npm                | `javascript/contributions.rs`, `composition/javascript.rs`      | Retained installed-package origins, lifecycle/generated inputs and final-module joins; minifier contributors remain conservative                                 |
| Native sources/static outputs | `composition/native.rs`                                         | Identify contributing implementation/source versions and measured outputs; wrapper crate versions alone are insufficient; coverage is currently false            |
| Platform/build tools          | `composition/platform.rs`, `recipe/{fastly_tools,apple,gnu}.rs` | Close required material, acquisition, helper/load and SDK coverage; current Fastly/Apple/GNU gaps remain blocking                                                |
| Runtime requirements          | `composition/external.rs`                                       | Record artifact-bound logical loader/ABI requirements; do not invent identities for installed operator-runtime bytes                                             |
| Licenses                      | Frozen `spdx/{cargo,javascript,native,platform}.rs`             | Preserve observed declarations and their origin. npm capture currently has no declaration field; missing/unsupported declarations remain explained `NOASSERTION` |

The current native wrapper pins match the reviewed wrappers (`aws-lc-sys` 0.42.0, `ring` 0.17.14, `libsqlite3-sys` 0.38.2), but that does not close their material/tool coverage. The controlled JS observer still accepts esbuild 0.27.3 while the current lock uses 0.28.2. Its separately reviewed adaptation remains a prerequisite wherever that observer is used; no ignored collector test becomes a pass.

## Proposed admission and data flow

1. Start with one sealed owner of an actually completed build. Bind approved source, asset/package, target, variant, final binary, Cargo lock, JS inventory, native outputs and tool/material observations to that owner. Re-measure retained inputs at handoff.
2. Project the owned observations through the existing Cargo, JS, native and platform modules. Preserve the distinction between observed build membership and exact retained machine code; selected inputs removed by linking/minification must not be misrepresented as retained code.
3. Admit complete output only when every required class has positive evidence and no unresolved **in-scope** coverage gap. Unknown contributors, unsupported wrappers/tools, mixed owners, changed originals, missing modules and altered final bytes fail. Existing false flags cannot simply be flipped; candidate gaps cannot be deleted to obtain admission.
4. Supply the shared renderer with validated composition and root context. Keep candidate diagnostics and complete output as distinct types and paths; schema/graph validity alone grants no complete-generation, signing or publication authority.
5. Validate with the retained offline SPDX 2.3 schema, graph/identity checks and bounded strict JSON handling. Recheck binary and document digests before handoff to the existing signer.

The input scope must explicitly include contributing Rust/npm/native material and the selected compiler, linker, SDK, helper and build-tool inputs required by the approved profile. Actual installed operator-runtime bytes remain outside that build-composition scope and are represented as logical external requirements. Every exclusion must be explicit and reviewed; unresolved contributing native/tool material cannot be relabelled as an exclusion to make a release pass.

## Ordinary release integration

The Phase 2 ordinary A/B receipt proves equality under its stated conditions. It supplies neither a `RecipeCompleted` nor complete composition. The ordinary path must not fabricate that private receipt or acquire a `Qualified*` capability.

The proposed bridge captures the real ordinary Cargo stream, verified source/material originals, fresh JS observations, native outputs and selected tools into a retained owner. It uses the existing capture types and the same admission/projection/renderer core; it does not introduce another SBOM design. Its private interface and acquisition/coverage rules must be specified and independently reviewed in the implementation plan before code is written. In particular, the plan must separate seven ordinary artifact identities from the frozen four controlled-recipe selectors rather than enabling new qualified recipes.

Until that bridge and every required coverage provider pass admission, production keeps the honestly documented interim inventory. A source port or diagnostic candidate is not completion of Gate 2. Connecting a qualified controlled recipe to production remains its separate approval decision.

## Compatibility requirements

- Retain SPDX **2.3**, predicate `https://spdx.dev/Document/v2.3`, `<asset>.spdx.json`, `<asset>.sbom.sigstore.json` and binary SHA-256 binding. Preserve the seven asset names and existing reusable `release-build.yml` binary signer/job.
- Preserve the published binary root identity `SPDXRef-Artifact` and its approved release-tag version. Validate contributing Cargo package versions against their approved manifests separately. The frozen candidate derives a Cargo root version; the implementation plan must explicitly map that difference without conflating the release tag with a crate version.
- Preserve observed license declarations without asserting legal conclusions. The frozen renderer recognizes MIT, Apache-2.0 and their two ordered OR combinations; other/missing values remain explanatory `NOASSERTION` unless a separately reviewed parser/capture extension is approved.
- A complete document may add components and relationships; the current manual verification commands still authenticate the exact shipped document. Predicate identity alone does not establish coverage completeness.
- No new trigger, policy reader, index, publication adapter, public offline verifier, deployment path or production/qualified selector is opened. GNU masking WIP remains excluded.

## Failure behavior and acceptance evidence

Admission failure occurs before attestation and publication. It blocks the selected complete-SBOM cohort; it cannot drop an asset, downgrade a profile or publish a candidate/placeholder under a release filename. Existing single-shot mutations, release-ID handling, immutable readback and read-only recovery remain unchanged.

The implementation plan must provide tests for each missing coverage class, changed binary/retained bytes, wrong source/root/target, mixed receipts, unknown contributors, fabricated license declarations and candidate-to-complete rejection. Positive fixtures must demonstrate actual ownership and complete evidence, not a caller-supplied completeness boolean. Existing candidate gap assertions and closed qualification tests remain.

Gate 2 is complete only after real selected builds yield admissible documents for all seven assets, with honest coverage and license metadata. A future approved rehearsal must show unchanged signer/source commands, exact SPDX-predicate equality online and from published bundles, real file/bundle inventory and failure before signing for incomplete evidence. Live signing/publication and sandbox-verifier trust remain subject to their separate gates; this proposal authorizes no rehearsal tag or release.

## Approval choices

- Approve the recommended strict scope and admission architecture, including explicit external-runtime boundaries and no implicit native/tool exclusions.
- Approve planning the ordinary owning-capture bridge using the reviewed common core, while keeping production closed until evidence is sufficient.
- Confirm honest `NOASSERTION` license handling and the distinct release-tag/Cargo-version mapping.
- Any narrower scope, target subset, new qualified selector or observer adaptation needs an explicit alternative decision.

## Review log

- **Round 1 (2026-10-28, fresh independent AI design review):** No blocking findings. Checked both Gate 2/3 approval proposals against the approved phase assignment, frozen source and current ordinary release contract. Confirmed ownership/completeness boundaries, the seven identities and 53-file destination, narrow ordinary-profile handling, and separate consumer/publication/trust gates. Coverage closure, observer adaptation and concrete implementation plans remain prerequisites. Read-only source review; no tests or runtime qualification were performed.

- **Round 2 (2026-10-28, a second fresh independent AI design review):** No blocking findings or required corrections. Rechecked the proposals against the approved phase assignment, frozen source and current ordinary release contract. Confirmed owning-evidence admission and its unresolved coverage prerequisites, version/license compatibility, exact seven-artifact membership, v2 separation from frozen v1, the 53-file inventory and narrow informational ordinary-profile handling. Concrete implementation plans and separate publication, consumer and trust approvals remain required. Read-only source review; no tests or runtime qualification were performed.
