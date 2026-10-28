# Gate 2 option: build-derived Cargo/npm SBOMs for all seven binaries

**Status:** Proposed scope amendment for the user and maintainers, requested on 2026-10-28. Docs only; not approved or implemented. The approved strict Gate 2 remains the governing contract until this amendment is accepted. The target-specific rollout recommendation has not been accepted. Gate 3 Steps 2–5 remain held. Draft #1463 stays draft, without reviewers.

**Issue:** [#1235](https://github.com/IABTechLab/trusted-server/issues/1235). **Source baseline:** Phase 3 `4741ab712365efa9f578a9b461f9a26a48bb9c5d`; frozen port source `122b78664b1235c717e9208fd200f9a9b0c335df`. No implementation, resources, acquisition, workflow/signing changes or rehearsal tags follow from this proposal.

## Decision and recommended claim

Adopt, if approved, an **issue-scoped build-derived Cargo/npm SBOM** for every existing release binary, together with artifact-bound logical runtime requirements. Keep all seven assets in one admission/publication cohort. This aligns the next SBOM milestone with #1235's release-build Cargo/npm requirement; it explicitly amends the broader strict-composition scope rather than claiming its native/platform gaps are solved.

Recommended public document name: **Build-derived Cargo/npm SBOM**. Recommended design title after approval: **Gate 2: build-derived Cargo/npm SBOMs and runtime requirements**. Use **admitted under the issue-scoped build profile**, never “complete SBOM,” “complete composition,” “fully covered,” or “qualified build.” Keep SPDX 2.3, `https://spdx.dev/Document/v2.3`, `<asset>.spdx.json`, all bundle names, source pins and signer identity unchanged. Renaming or superseding the existing `complete-spdx` planning documents would be a later documentation migration; this proposal renames no existing file or release asset.

| Direction                                                  | Coverage and cohort                                                                                                  | Decision status                                               |
| ---------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
| **Issue-scoped all-seven — recommended for this decision** | Actual Cargo/npm build evidence, declared licences and logical runtime requirements; explicit native/platform limits | Proposed amendment in this document                           |
| Strict all-seven                                           | Also requires retained native/tool/sysroot/SDK input closure and corresponding licence evidence                      | Previously approved Gate 2; expensive unresolved cells remain |
| Target-specific strict rollout                             | Changes when individual targets could activate                                                                       | Not approved; not used by this option                         |

The previous [80–136 person-day estimate](binary-attestations-phase3-full-cohort-feasibility.md) prices unresolved strict native/platform/licence closure. It explicitly excludes Cargo/JS/runtime capture and generator integration. It is not the cost of satisfying the issue-scoped Cargo/npm requirement, and cannot be reduced to this option by subtracting an arbitrary percentage.

## Common scope for all seven

| Release binary                                  | Required evidence                                               | Existing execution location                                           |
| ----------------------------------------------- | --------------------------------------------------------------- | --------------------------------------------------------------------- |
| `trusted-server-fastly.wasm`                    | C + J + Cargo/npm declarations + RW + existing identity context | Ubuntu Fastly canonical job; retain the existing independent A/B gate |
| `ts-x86_64-unknown-linux-gnu`                   | C + J + Cargo/npm declarations + RG + existing identity context | Actual x64 UBI build container                                        |
| `ts-aarch64-apple-darwin`                       | C + J + Cargo/npm declarations + RM + existing identity context | Actual Apple Silicon macOS job                                        |
| `ts-x86_64-apple-darwin`                        | C + J + Cargo/npm declarations + RM + existing identity context | Actual Intel macOS job                                                |
| `trusted-server-axum-x86_64-unknown-linux-gnu`  | C + J + Cargo/npm declarations + RG + existing identity context | Actual x64 UBI build container                                        |
| `trusted-server-axum-x86_64-unknown-linux-musl` | C + J + Cargo/npm declarations + RS + existing identity context | Existing static musl job                                              |
| `trusted-server-axum-aarch64-unknown-linux-gnu` | C + J + Cargo/npm declarations + RA + existing identity context | Actual native ARM UBI build container                                 |

Every record must join the actual successful invocation, source commit, package/bin, target, features, profile, private output set and final executable SHA-256. Evidence is retained before the producing container/private directory disappears and rehashed at handoff. Missing or ambiguous evidence **inside this scope** fails the entire cohort; a limit on native/platform closure does not excuse a missing Cargo unit, unknown npm origin or wrong binary. No new builders, images, original-acquisition service, qualification resource or local capture experiment is required or authorized.

### C: actual Cargo compilation

Capture the successful Cargo JSON message stream at the real invocation for every target, including inside each GNU container. Capture an **unfiltered** same-execution metadata identity catalog and explicit-target non-dev tree/query context. Catalog/tree membership identifies possible packages and edges; it does not prove compilation. Select compiled package/unit membership from observed compiler/build-script messages, retaining package ID, source, target kind, host/target context and effective unit features. A package used both as a host build dependency and a target library retains both roles. Uncompiled dev-only units are not binary contributors.

Join observed package identities to the source/lock/workspace or installed Cargo manifests actually used by that invocation, retaining available archive checksums/Git revisions and metadata provenance. Do not require newly acquired source distributions beyond materials already fetched for the release build. Do not turn a lock checksum or compiled Rust wrapper crate into proof of its bundled C implementation. Compiled Rust units eliminated later by linking may remain conservative compiled inputs; exact machine-code reachability and an exact compiler-unit dependency DAG are not claimed.

Today only Fastly retains actual Cargo JSON; native commands do not. Current post-build `--filter-platform` metadata and cargo-cyclonedx output cannot substitute for these observations. Existing `composition/capture.rs`, `composition/origin.rs` and `recipe/plan.rs` supply reusable distinctions, not an already implemented ordinary all-target provider.

### J: actual private bundles and npm membership

Observe the private `OUT_DIR/tsjs-dist` belonging to the successful Cargo invocation, identified through its build-script output. Record the actual discovered module set, source/npm/generated origins, nonzero Rollup contributions, exact lock installation path/name/version/integrity, and final bundle hashes **after** Vite/esbuild transformations. Attribute npm packages from those module records; the entire installed npm tree is not a substitute. Keep installed/executed bundler and other known build tools in a separate build-material role. Retain generated-module producer identity and the evidence actually observed; an unexplained contributing origin fails admission.

Bind the final module bytes to that build's generated Rust include inputs and inspect the final executable for the reviewed whole-module retention observation. Distinguish a positively observed full retained module from a prepared/compiled JS input. Absence of a whole-bundle substring is **not** proof that no transformed fragment survived: keep its package inputs as build material with fragment retention unestablished, rather than declaring npm absent. CLI depends on core and JS too; inspect every CLI output instead of assuming an empty npm set.

Rollup contributions precede final minification. They are conservative evidence of package inputs to the actual private bundle, not an assertion that every recorded package's functions survive in the final executable. State that limit in the SBOM. Type-only imports and Rollup-eliminated modules do not become embedded npm components. The embedded Prebid shim is covered; the separately produced/operator-selected external Prebid bundle is outside this release binary.

The ordinary provider must record the **actual Cargo-built private bundles**, not run an additional bundle build and claim it produced the signed binary. `build-all.mjs`/`release-composition.mjs` already support opt-in contribution recording, but ordinary builds do not enable it yet. `ordinary/inputs.rs` only hashes a Fastly-specific private layout; `collect-js.mjs` requires checked preparation and is not an ordinary drop-in. Extending this owning evidence is real work included in the estimate.

Ordinary capture does **not** depend on `observe-npm.mjs`. Its controlled esbuild 0.27.3 restriction remains frozen while the current lock uses 0.28.2. This option neither reuses nor adapts that observer. If the later provider design needs it, stop for its separate decision; do not silently change the pin. Native bundler/lifecycle helper closure remains a stated scope limit, without weakening npm module identity, actual-build or generated-origin checks.

### Logical runtime requirements

| Contract | Required observation from the final bytes                                                    | Limit                                                                                 |
| -------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| **RW**   | Wasm imports/host requirements, accepted Wasm target/runtime and binary digest               | Not the installed Fastly runtime's identity or implementation                         |
| **RM**   | Mach-O CPU, loader, dylibs, minimum OS and SDK load facts, binary digest                     | Not SDK input closure or proof of execution on macOS 13                               |
| **RG**   | GNU x64 ELF interpreter, needed libraries, symbol-version/ABI requirements and binary digest | Not the deployed loader/libraries' bytes or package closure                           |
| **RS**   | Static x64 ELF/architecture facts and absence of a loader/needed libraries, binary digest    | Static does not imply complete musl/crt/compiler-runtime material or licence coverage |
| **RA**   | Actual ARM ELF interpreter, needed libraries and symbol-version requirements, binary digest  | Never reuse x64 observations for ARM                                                  |

Keep external requirements separate from embedded-component claims. Axum's operator-provided CA bundle, runtime configuration and stores are runtime data, not embedded build components. Existing `composition/external.rs` supplies RW/RM/GNU-x64 projection; its hard-coded x64 constructor and interpreter-bearing ELF shape require reviewed ordinary ARM/static extensions for RA/RS. Existing smoke/format checks support capture but do not already produce all these retained receipts.

### Cargo/npm declared licences

For each admitted Cargo/npm package, retain the declaration text and its observed source: the exact manifest/package metadata path and digest, package/source identity and build binding. Cargo workspace-inherited declarations must resolve against that build's actual workspace. npm declarations come from that exact installation's package metadata, checked against its lock identity; distinguish observed installed metadata from independently authenticated upstream originals. Reuse already available licence-file references where supportable; no new source-acquisition programme or full file-level licence scan is implied.

Use `licenseDeclared` only for supported expressions faithfully derived from those declarations. For missing, legacy, unsupported or unresolvable declarations, use `NOASSERTION` and `licenseComments` explaining which case occurred, preserving the observed text/reference where available. `licenseConcluded` remains `NOASSERTION`; the generator makes no legal conclusion. Unknown declarations do not justify omitting known compiled package identities. A declaration capture/identity failure is an admission error, not a missing-license excuse.

The frozen renderer recognizes MIT, Apache-2.0 and their two ordered OR combinations. Its npm projections currently provide no licence field, so declaration capture/projection requires review and implementation; it is not already closed by the source port. Do not extend expression support silently. A Rust wrapper's declaration does not establish all bundled native licences; neither Cargo/npm declarations nor explained `NOASSERTION` imply complete native/tool/SDK licence coverage.

## Existing tool and builder identity context

Record configured pins **and actual selected identities** with their source and scope. Read facts in the existing producing job/container; do not add acquisition, image builds or whole-SDK hashing for this option.

| Context     | Identity available / proposed observation                                                                                                         | What must not be inferred                                                                                                       |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Rust        | `rust-toolchain.toml` 1.98.1; selected compiler/Cargo version/target, existing source pins and available executable identities                    | Rust std/compiler-builtins, helpers, libraries and sysroot material closure                                                     |
| Node/npm    | `.tool-versions` Node 24.20.0; selected Node/npm versions and existing distribution verification                                                  | Native npm helper/lifecycle closure                                                                                             |
| GNU builder | Existing UBI 9.6 base reference/digest from `phase1-build.sh`, actual platform and local derived image ID                                         | The base digest covers later unpinned `dnf install`; a local derived ID proves authenticated package acquisition                |
| macOS       | Existing `macos-15`/`macos-15-intel` runner labels; observe selected Xcode version, SDK version/path selection and corresponding Mach-O SDK facts | Runner labels or deployment target 13.0 are Xcode/SDK pins, an immutable VM digest, SDK input closure or old-OS execution proof |
| Static musl | Existing musl/musl-dev/musl-tools 1.2.4-2 selections and `gcc-13` selector; actual selected versions                                              | Complete GCC/musl/crt materials or authenticated platform-image closure                                                         |

There is **no explicit Xcode/SDK pin** in the inspected ordinary release workflow. Recording the selected versions is proposed same-job observation, not evidence already retained by that producer. Do not invent a pin or use controlled-recipe fixtures as ordinary-job facts. Required identity fields that cannot be observed must fail the proposed profile rather than be guessed. No new pin, resource or stronger acquisition requirement is adopted here. Fastly's existing pinned CLI route remains packaging verification, not toolchain closure.

## Scope statement inside every SBOM

Proposed common profile identifier: `issue-cargo-npm-build-v1`. This is a proposed coverage claim, not a new schema/policy file. Encode a bounded, versioned scope statement in SPDX's existing `creationInfo.comment`; preserve per-component origin/role/`licenseComments`. The later plan must specify deterministic rendering and admission of that statement, with no new undeclared SPDX properties.

Draft required text, present in **each** shipped document:

> Coverage profile: issue-cargo-npm-build-v1. Build-derived Cargo/npm SBOM with logical runtime requirements for the identified executable. Cargo membership derives from this release build's observed compilation; npm membership derives from its actual private bundle contributions and module evidence. Compiled inputs and build tools are distinguished from positively observed retained modules. Further linking/minification and fragment retention are not fully attributed. Tool and builder records identify selected versions/pins only. This document does not establish complete native C/assembly, static libraries, Rust standard-library/compiler-runtime, linker/helper, system-library, SDK/header/stub/framework, image-package or corresponding licence closure. External requirements describe logical dependencies, not the operator's installed runtime or configuration. This is not a complete-composition or controlled-build qualification claim.

The runtime requirements and source/target/artifact joins remain structured package/relationship evidence, not just prose. The common scope marker is required even if a target has no positively retained JS module. Tool identities are build context/materials, never evidence that their complete contents are in the binary. Do not set a whole-document completeness or qualified-coverage flag to true. Existing strict `generate_from_completed`, candidate rejection and uninhabited `Qualified*` capabilities remain unchanged; any later issue-scoped admission must be a separately reviewed owning-evidence path, not candidate promotion or a fallback on strict failure.

### Draft verification-guide wording after approval and implementation

The existing guide describes the current interim inventory correctly. Do not replace it with a future claim in this docs-only change. The proposed replacement after real implementation is:

> Each `<binary>.spdx.json` is a build-derived Cargo/npm SBOM under profile `issue-cargo-npm-build-v1`. It records packages observed in that binary's release build, the actual private JS bundle/module evidence, declared Cargo/npm licences with explained `NOASSERTION`, and logical runtime requirements from the final executable. Its root SHA-256 binds it to the binary. It distinguishes build inputs/tools from positively observed retained modules; exact final functions or transformed-fragment attribution are not claimed. Native implementation, Rust sysroot/compiler-runtime, system-library, SDK and tool/image-package closure and their full licence coverage are explicit limits recorded inside every SBOM. Verified provenance/SPDX authenticates this stated scope; it does not establish complete composition, vulnerability absence or installed-runtime identity. Strict native/platform closure is a tracked future enhancement under #1235's follow-up work, not a prerequisite hidden inside this profile.

Existing online/offline signer/source/tag pins, bundle and immutable-asset verification and exact shipped-SPDX/predicate comparison remain necessary and unchanged. Consumers must inspect and enforce the scope/profile they require; the SPDX predicate URI alone does not prove a particular coverage level. No future operator CLI/consumer implementation is authorized by this wording.

## Draft Gate 2 spec amendment for approval

The following is proposed amendment text to the Phase 3 Gate 2 spec/plan and the phased interpretation of the retained R-SBOM/R-COHORT requirements. It is not an edit to the carried #1262 sentences and does not claim approval of a changed contract:

> **Issue-scoped release SBOM milestone.** To align the next milestone with #1235's requirement for release-build Cargo/npm coverage, every one of the seven selected binaries MUST have an SPDX 2.3 build-derived Cargo/npm SBOM admitted under the explicitly identified issue-scoped profile. It MUST derive compiled Cargo membership from the successful actual build stream, with unfiltered identity metadata and reconciled target/host/unit context; derive npm membership from that execution's private bundles, contributions and module evidence; report the artifact's logical runtime requirements; and retain declared Cargo/npm licence evidence with explained `NOASSERTION`. The release artifact root MUST retain its public filename, exact release-tag version and executable SHA-256; Cargo component versions and source commit MUST remain independently identified.
>
> Each SBOM MUST record the common coverage profile, conservative membership limits, selected tool/builder identity context and explicit lack of strict native/platform/tool/sysroot/SDK material and corresponding licence closure. This scoped document MUST NOT be described as complete composition, hermeticity, controlled qualification or exact retained machine-code attribution. Missing or inconsistent in-scope evidence for any member MUST block the entire seven-binary cohort. There MUST be no per-target activation, silent subset, downgrade or strict-failure fallback. Existing no-configuration-in-the-binary, source/signer, immutable-publication, predicate/root equality, parser/size bounds and Fastly ordinary equality requirements remain in force.
>
> Strict native/platform composition closure remains a tracked future enhancement, with its unresolved evidence and acquisition questions retained in the full-cohort assessment. Its qualified types and complete-production generation remain closed. Accepting this milestone does not declare those cells resolved, delete their tests or authorize a new resource. A future stronger profile requires separate reviewed evidence and explicit policy, without renaming the published SBOM companion or weakening historical strict policy.

This amendment requires an explicit user/maintainer decision on the coverage reduction. Approval to discuss it is not approval to implement, publish or change existing strict admission. It settles neither legal licence sufficiency nor future native/platform resources.

## Gate 3 consequence: one cohort and explicit assurance

If this amendment is approved, the revised Gate 3 plan must keep **all seven rows together** under neutral v2 cohort `trusted-server-binaries`, each requiring the **same issue-scoped SBOM profile**. Fastly additionally requires the existing matched ordinary A/B receipt; native reproduction remains measured, not a release gate. The existing `provenance-sbom` / `provenance-sbom-repeatable` labels alone must not conceal the reduced SBOM scope: a reviewed explicit coverage discriminator/policy identity must distinguish this profile from strict complete composition. Its field placement is for the revised v2 plan, not a schema change authorized now.

For that amended profile, “repeatable” still means the validated matched `ordinary-release-fastly-byte-equality-v1` receipt under Phase 2's narrower conditions. It does not mean controlled recipe/native/platform qualification. Do not broaden false-qualification acceptance globally, reuse a candidate build-evidence/build-recipe document, or make an empty/placeholder companion admissible. The later plan must define real issue-profile owning evidence for every required companion slot before publication.

Frozen v1 policy/schema/fixtures and `phase1-fastly-cli` remain unchanged and do not automatically accept the issue-scoped profile. The proposed **53-file / 52-checksum destination**, existing seven filenames, companion names, exact retained ordinary receipt and cycle-free signing order remain the planning contract; this option creates or publishes none of those future companions. A scope amendment does not itself prove those writers or all 53 real files exist. Any need to alter that destination must return as a separate decision, not silently shrink it.

Gate 3 Steps 2–5 stay held **now**, even while this option is reviewed. If the scope is accepted, bring back a revised concrete Gate 2/Gate 3 plan for approval before capture, v2 metadata/schema/admission/assembly or generator work. Consumer code remains Phase 4; publication integration, actual-workflow contract restoration and fork trust remain separately gated. There is no per-target early activation and no rehearsal authorization in this document.

## Engineering estimate on the same basis

Engineering person-days include design of the eventual adapter, implementation, focused verification and review **if later approved**. These are judgment ranges, not measured delivery bounds, staffing promises or elapsed schedules. They exclude approval waiting, live publication/consumer integration and later v2 index work. No strict N0/L0/T0/I0 acquisition/closure foundation is charged to this option; it uses existing fetched Cargo/npm materials and existing hosted execution. If a concrete plan requires new resources or observer adaptation, stop and re-estimate rather than treating this range as permission.

| Shared foundation, counted once                                                         | Person-days |
| --------------------------------------------------------------------------------------- | ----------: |
| Actual Cargo stream, catalog, unit/feature and package/source joins                     |         4–6 |
| Ordinary private-bundle/npm contributions, actual-build ownership and module membership |        6–10 |
| Common final-byte logical runtime projection                                            |         2–3 |
| Cargo/npm declared-licence capture and explained `NOASSERTION`                          |         1–2 |
| Scoped SPDX rendering/admission, limits, version mapping and existing identity context  |         3–5 |
| Shared negative tests, whole-cohort handoff checks and focused review                   |         2–3 |
| **Shared subtotal**                                                                     |   **18–29** |

| Target integration beyond shared work                           | Person-days |
| --------------------------------------------------------------- | ----------: |
| Fastly canonical/reproduction capture and RW                    |         1–2 |
| GNU x64 CLI, including actual container stream/material handoff |         2–3 |
| Apple ARM CLI / RM                                              |         1–2 |
| Apple Intel CLI / RM after shared Apple work                    |       0.5–1 |
| GNU x64 Axum / RG after GNU work                                |       0.5–1 |
| Static musl Axum / ordinary RS support                          |       1.5–3 |
| GNU ARM Axum / ordinary RA and actual container evidence        |         2–3 |
| **Target increments**                                           |  **8.5–15** |
| **Total issue-scoped engineering estimate**                     | **26.5–44** |

The remaining cost is real ordinary capture/admission work: the source port does not enable complete generation, native jobs lack streams, JS capture is opt-in/Fastly-specific, npm licence fields are absent and RA/RS require ordinary representations. This estimate does not include resolving the strict native/platform gaps later. No target-specific activation or new builder is assumed to achieve it.

## CI time impact per job

Baselines below are **observed rehearsal `.16` at `5fa384336`**, as recorded in the [assessment](binary-attestations-phase3-full-cohort-feasibility.md); they are not current-head benchmarks. Added times are **unmeasured planning allowances** for local stream/catalog collection, source/private-output hashing, contribution recording, runtime projection, rendering and handoff. No trace-based closure, SDK tree hashing, new package acquisition, second npm build or image replacement is included. Do not credit possible savings from replacing the currently separately compiled converter/generator without measurement.

| Job                   | Observed baseline | Proposed added allowance | Illustrative total |
| --------------------- | ----------------: | -----------------------: | -----------------: |
| Fastly canonical A    |              5:53 |                  1–4 min |          6:53–9:53 |
| Fastly reproduction B |              4:29 |                  1–4 min |          5:29–8:29 |
| GNU x64 CLI           |              8:14 |                  1–4 min |         9:14–12:14 |
| Apple ARM CLI         |             13:03 |                  1–4 min |        14:03–17:03 |
| Apple Intel CLI       |             31:09 |                  2–6 min |        33:09–37:09 |
| GNU x64 Axum          |              5:24 |                  1–4 min |          6:24–9:24 |
| Static musl Axum      |              4:47 |                  1–4 min |          5:47–8:47 |
| GNU ARM Axum          |              4:48 |                  1–4 min |          5:48–8:48 |
| Fastly comparison     |              1:26 |               0.25–1 min |          1:41–2:26 |

These are eight producing jobs plus comparison, not nine different binaries. Added producer/comparison runner time totals **9.25–35 minutes**. Existing sequential A→B→comparison adds **2.25–9 minutes** under these assumptions; native jobs overlap, so neither subtotal is a release wall-time forecast. Intel's larger allowance is a conservative estimate based on its slower baseline, not a measured platform multiplier.

For the unchanged signing and publication jobs, allow a provisional **0–1 extra minute each** for larger admitted SPDX/evidence payloads and existing verification; no extra signing/publication calls are proposed here. That yields **9.25–37 additional aggregate runner-minutes** including both allowances, excluding service latency variance. Validation/check jobs need focused fixtures but no proposed new build; their time impact is unmeasured and must be reported by the implementation plan. No claim is made that the allowances bound remote API latency.

Existing producer jobs have 90-minute limits; Fastly comparison alone has ten minutes; signing and release have fifteen minutes each. The illustrative totals suggest headroom but prove no future fit. The eventual implementation must measure each job, retained evidence size and existing predicate bounds, report regressions and fail within the approved limits. Exceeding them requires a new reviewed decision, not an automatic timeout/resource increase.

## Future enhancement and acceptance evidence

Track **strict native/platform composition and licence closure** as a separate future enhancement under #1235's remaining phase work, linked to the existing 21-cell assessment and its native/tool/licence cells. This is a planning record, not a newly filed issue. It includes Rust std/compiler-builtins/crt/runtime material, bundled C/assembly/static archives, actual link selections, tool/helper/library/resource closure, SDK originals and authenticated derived image/package acquisition. Preserve the strict tests and unresolved evidence; do not tick those cells as closed after the narrower milestone.

A later implementation plan must demonstrate all seven actual builds, their per-artifact C/J/runtime/declared-licence evidence and mandatory scope text, plus wrong-source/target/unit/lock/module/digest rejection, generated-origin failures, absent/unsupported declarations, missing scope markers, size/parser failures and whole-cohort rejection. Verify the distinction between prepared inputs, positively observed retained modules and unknown fragment retention. Prove that strict complete/qualified consumers still reject issue-profile evidence and that exact published SPDX equals the verified predicate. Such evidence and any live rehearsal require their later approvals; none is claimed here.

## Review log

- Round 1 (2026-10-28, fresh independent source/contract review): no blocking findings. Checked actual Cargo/private JS capture gaps, conservative membership and licence claims, runtime extensions, selected Xcode/SDK observations, all-seven rejection, strict-profile separation and authority/observer boundaries against repository source. The review did not independently retrieve the live issue; the author separately read #1235. No tests, capture, resources or measurements were performed.

- Round 2 (2026-10-28, a second fresh independent estimate/source review): no blocking findings. Recomputed shared/target engineering totals, per-job baseline conversions, aggregate runner time and sequential Fastly impact; checked tool pins, actual Xcode/SDK limitations and all-seven scope against the workflows and assessment. Unmeasured overhead remains an estimate. No tests, acquisition or measurements were performed.

These reviews assess the proposal, not adoption of the scope amendment. The user and maintainers still decide; no implementation or later gate is opened.
