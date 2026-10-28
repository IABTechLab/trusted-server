# Gate 3 proposal: seven-artifact release-index amendment

**Status:** Gate 3 design approved on 2026-10-28 with new neutral v2 cohort ID `trusted-server-binaries`. Frozen v1 keeps `phase1-fastly-cli`. The [concrete index plan](binary-attestations-phase3-seven-artifact-index-implementation.md), with the CLI-owned v2 policy, a separate producer cohort source and staged execution gates, awaits revised-plan approval. The repeatability definition is approved; no implementation follows until its sequencing gates open. No schema, policy, consumer, producer or workflow implementation is authorized yet.

**Historical reviewed baseline:** Phase 2 `8a797dbcb0467b867dfbcc571597110b9254d672`, tree-equivalent to `08e1c33b8` after application integration `7d551777daeaee09e24bce9bf4f96edac6e5e0df`. `.13` remains attributed exclusively to `770a425df4b28caaaef68dfd930bceabd9928046`.

**Verification baseline:** [Historical Phase 2 `8a797dbcb` CI passed](https://github.com/IABTechLab/trusted-server/actions/runs/40654745582); signing/publication and manual negative canaries were skipped as configured.

**Current integration baseline:** Phase 2 `5fa3843364f247fdc261c438ab9257323637a42c`; [exact-head CI](https://github.com/IABTechLab/trusted-server/actions/runs/40664514307) and readiness work are tracked in #1449. Rehearsal `.16` covers this exact implementation; [full transcript](https://github.com/IABTechLab/trusted-server/pull/1449#issuecomment-6467368408). Phase 2 `fb4843355` is the later docs-only evidence record.

**Tracking:** Part of #1235.

## Approved design scope

The user approved a **schema-version 2** index contract for exactly the existing seven binaries, preserving public filenames and frozen v1 bytes. Approval includes explicit informational handling of the existing ordinary Fastly comparison profile, with no controlled-qualification inference. Complete evidence, live signing/publication and the public consumer remain separately gated.

The approved [phase assignment](binary-attestations-phases-2-4.md#separate-design-approval-gates) calls for a reviewed v1.1/v2 amendment. The retained v1 schema fixes integer `schema_version: 1` and exactly four entries; it has no minor-version mechanism. Today’s ordinary cohort has seven entries. Neither contract can silently stand in for the other.

| Approach                          | Benefit                                                                                    | Cost / disposition                                                                             |
| --------------------------------- | ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------- |
| **New version 2 — recommended**   | Distinguishes seven-asset acceptance and policy identity; preserves frozen v1 and fixtures | Requires coordinated producer/schema and later consumer changes                                |
| Replace unpublished v1 explicitly | Fewer version branches                                                                     | Reuses an exactly-four identity for a different contract; avoid historical-policy ambiguity    |
| Keep index deferred               | Preserves today’s 30-file distribution                                                     | Does not complete Gate 3; never publish a partial four-asset index for seven selected binaries |

## Exact cohort

All rows retain `variant: default`, `default_features: true`, `features: []`. These are current ordinary cohort descriptors; their destination labels do not prove controlled qualification.

| Asset                                           | Package / binary                                      | Role / kind                          | Target                      | Runtime / minimum / architecture       | Capability          | Assurance profile            |
| ----------------------------------------------- | ----------------------------------------------------- | ------------------------------------ | --------------------------- | -------------------------------------- | ------------------- | ---------------------------- |
| `trusted-server-fastly.wasm`                    | `trusted-server-adapter-fastly` / same                | `server` / `wasm-core`               | `wasm32-wasip1`             | `fastly-compute` / `wasip1` / `wasm32` | `fastly-production` | `provenance-sbom-repeatable` |
| `ts-aarch64-apple-darwin`                       | `trusted-server-cli` / `ts`                           | `operator-cli` / `native-executable` | `aarch64-apple-darwin`      | `macos` / `13.0` / `aarch64`           | `cli-execute`       | `provenance-sbom`            |
| `ts-x86_64-apple-darwin`                        | `trusted-server-cli` / `ts`                           | `operator-cli` / `native-executable` | `x86_64-apple-darwin`       | `macos` / `13.0` / `x86_64`            | `cli-execute`       | `provenance-sbom`            |
| `ts-x86_64-unknown-linux-gnu`                   | `trusted-server-cli` / `ts`                           | `operator-cli` / `native-executable` | `x86_64-unknown-linux-gnu`  | `linux-gnu` / `glibc-2.34` / `x86_64`  | `cli-execute`       | `provenance-sbom`            |
| `trusted-server-axum-x86_64-unknown-linux-gnu`  | `trusted-server-adapter-axum` / `trusted-server-axum` | `server` / `native-executable`       | `x86_64-unknown-linux-gnu`  | `linux-gnu` / `glibc-2.34` / `x86_64`  | `axum-production`   | `provenance-sbom`            |
| `trusted-server-axum-x86_64-unknown-linux-musl` | `trusted-server-adapter-axum` / `trusted-server-axum` | `server` / `native-executable`       | `x86_64-unknown-linux-musl` | `linux-musl` / `static` / `x86_64`     | `axum-production`   | `provenance-sbom`            |
| `trusted-server-axum-aarch64-unknown-linux-gnu` | `trusted-server-adapter-axum` / `trusted-server-axum` | `server` / `native-executable`       | `aarch64-unknown-linux-gnu` | `linux-gnu` / `glibc-2.34` / `aarch64` | `axum-production`   | `provenance-sbom`            |

The approved v2 cohort ID is `trusted-server-binaries`, carried only by the new v2 policy identity. Frozen v1 keeps `phase1-fastly-cli` and unchanged bytes, fixtures and policy. There are no other renames. Loader floors remain distinct from actually tested operating systems. Native reproduction remains a bounded measurement, with no equality gate. No Fastly staging/reuse variant or additional Axum hardening is enabled.

## Amendment and ownership

| Area                       | Required future change                                                                                                                                   | Boundary preserved                                                                                                    |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Governing schema           | Add a separately approved `docs/superpowers/specs/schemas/release-index-v2.schema.json`; require version 2 and each of the seven identities exactly once | Existing v1 raw bytes remain unchanged; no open-ended artifact list                                                   |
| Producer index descriptors | Represent the seven rows and strict source/digest joins in `distribution/**` and its policy checks                                                       | Extending index identities must not inhabit `Qualified*`, extend controlled recipe selection or enable Axum execution |
| Types/compatibility        | Add only required Axum, musl/static and GNU aarch64 descriptor values                                                                                    | Existing binary names, default variant and other closed values remain                                                 |
| Consumer policy identity   | Explicitly review the new `policy_version`, raw-byte hash and seven entries in a separately approved v2 policy                                           | Downloaded metadata never selects authority; existing signer/repository/tool pins are not silently changed            |
| Consumer implementation    | Phase 4 updates `src/release/{policy,bounds,evidence}.rs` and `evidence/joins.rs` in the CLI                                                             | Phase 3 does not double-port the public consumer or change its `readonly-verify` action                               |
| Bounds                     | Explicit seven-executable count for version 2                                                                                                            | Preserve byte/depth/process/deadline limits unless separately reviewed; measure larger-cohort cost                    |

Keep source, recipe-source and cohort-source commits equal to the independently approved commit, as in v1. Reject duplicate JSON keys, duplicate/conflicting identities, unsafe names, unknown versions/profiles/capabilities and mismatched inventories. Evaluate schemas offline using their approved dialect/bytes. Do not narrow SemVer/build-metadata acceptance or relax other bounds as a side effect.

The frozen four-destination `ReleaseTarget`/`FIXED_ARTIFACTS` tables also select controlled recipes. A seven-entry index descriptor is metadata, not execution authority; the implementation plan must preserve that separation. Historical producer-policy acceptance cannot override the consumer’s compiled cohort/schema restrictions. The historical-policy list remains unchanged unless its exact entries are separately approved.

## Distribution layout

The destination is **53 real files**, not today’s 30-file distribution:

- Four release-wide files: `release-index.json`, `release-index.provenance.sigstore.json`, `tsjs-inventory.json`, `SHA256SUMS`.
- Seven per executable: the executable, `<asset>.spdx.json`, `<asset>.provenance.sigstore.json`, `<asset>.sbom.sigstore.json`, `<asset>.build-evidence.json`, `<asset>.build-recipe.json`, `<asset>.rebuild-check.json`.

Thus `4 + 7 × 7 = 53`; `SHA256SUMS` covers exactly the other 52 files. The index digests exact companion/inventory bytes and excludes its own digest/bundle; their identities remain external. The future signing order must avoid cycles: binary evidence/bundles, then index, then index bundle, then checksums/publication.

SPDX remains 2.3 with predicate `https://spdx.dev/Document/v2.3`. Existing binary filenames, genuine bundle names and reusable signer identity remain unchanged. No placeholder/empty bundles, invented native measurement documents or candidate companions may fill the destination slots. Live writers and publication ownership require Gate 4 approval.

## Ordinary Fastly receipt compatibility

Keep the permanent filename `trusted-server-fastly.wasm.rebuild-check.json`. The currently shipped receipt is version `1`, profile `ordinary-release-fastly-byte-equality-v1`, with `release_qualification:false`. Phase 3 must index its **exact existing bytes**; the frozen candidate renderer must not replace or reserialize it.

Producer `distribution/joins.rs::check_comparison` and later consumer `release/evidence/joins.rs::parse_comparison` need explicit profile dispatch. Validate ordinary source/target/default variant, final binary size/digest, recipe/lock/JS identities, current-run A/B jobs and retained transfer bindings. Unknown/malformed profiles or mismatched joins fail.

Frozen consumer `release/evidence.rs` rejects any false qualification value as a candidate. The only proposed exception is informational acceptance of this exact ordinary Fastly rebuild-check profile after strict validation and authenticated membership. Other candidate companions remain rejected; do not globally allow false qualification. Candidate sentinel tags/domains, missing workflow identities and awaiting-signature empty bundles never become production evidence.

The ordinary profile conveys its narrower equality claim. It cannot satisfy a controlled-qualification requirement; policy must reject it wherever that stronger requirement applies. Cohort labels alone do not grant authority. Later controlled evidence uses a distinct approved profile in the same filename; incompatible field changes require a separately reviewed receipt-version change. The index’s version 2 does not change the receipt’s version 1.

## Admission, failures and verification

Schema approval is not permission to publish a seven-entry signed index. Publication still needs Gate 2 admissible evidence for all selected artifacts, real companion/bundle writers and the separately approved signing/publication integration. A missing member blocks that selected cohort; neither a partial index nor a downgraded profile is a recovery path. Actual native measurement outcomes may differ without creating a native equality gate.

Test exact seven-entry membership, all identity tuples and bounds; every missing/duplicate/extra asset; source/recipe/cohort mismatches; ordinary versus controlled profile dispatch; false-qualification candidate rejection; stale/mismatched transfers; and raw companion-byte preservation. Keep v1 fixtures unchanged and test explicit version-2 policy acceptance, not automatic historical acceptance.

Future live rehearsal follows Gate 4 and sandbox-trust approval: independently verify every genuine asset, index/bundle and checksum; preserve existing binary provenance/SPDX commands; compare the indexed ordinary receipt byte-for-byte; test a missing/changed companion before publication. Production policy’s official repository/signer trust is not expanded to a fork through downloaded index contents. Gate 6 owns that rehearsal trust mechanism.

## Approval choices

- Approve version 2 with frozen v1 retained, the seven exact rows and the 53-file destination layout.
- The user selected the new neutral v2 cohort ID `trusted-server-binaries`; frozen v1 retains its legacy ID.
- Approve the narrow informational ordinary-profile handling, with controlled requirements and candidate restrictions preserved.
- Keep public consumer code in Phase 4 and live publication/sandbox trust behind Gates 4/6. No new schema/policy file is created before this design and its concrete implementation plan are approved.

## Review log

- **Round 1 (2026-10-28, fresh independent AI design review):** No blocking findings. Checked both Gate 2/3 approval proposals against the approved phase assignment, frozen source and current ordinary release contract. Confirmed ownership/completeness boundaries, the seven identities and 53-file destination, narrow ordinary-profile handling, and separate consumer/publication/trust gates. Coverage closure, observer adaptation and concrete implementation plans remain prerequisites. Read-only source review; no tests or runtime qualification were performed.

- **Round 2 (2026-10-28, a second fresh independent AI design review):** No blocking findings or required corrections. Rechecked the proposals against the approved phase assignment, frozen source and current ordinary release contract. Confirmed owning-evidence admission and its unresolved coverage prerequisites, version/license compatibility, exact seven-artifact membership, v2 separation from frozen v1, the 53-file inventory and narrow informational ordinary-profile handling. Concrete implementation plans and separate publication, consumer and trust approvals remain required. Read-only source review; no tests or runtime qualification were performed.
