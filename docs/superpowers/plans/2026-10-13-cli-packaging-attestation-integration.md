# CLI package attestation integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make installable CLI packages verifiably contain the canonical attested executables, without creating a second release authority or weakening #1235 evidence.

**Architecture:** Consume final canonical Build A executables and their qualified evidence; packaging never recompiles or alters those bytes. A separately signed package inventory binds outer package digests to exact executable members. One publication job uploads canonical and selected package evidence before publishing a new immutable tag.

**Tech Stack:** Existing #1292 shell helpers/nfpm packaging design, the host-only Rust release tool's strict metadata and bounded package/archive modules, SPDX canonical executable evidence, pinned GitHub attest/gh contracts from the primary plan.

---

The [Rust migration addendum](../specs/2026-10-14-release-tooling-rust-migration.md) applies to every new packaging producer helper. Extend private `crates/trusted-server-release/src/package.rs` and `distribution.rs` modules; retain thin POSIX/nfpm adapters, no introduced Python or generic packaging framework. Native independently approved-source bootstrap and all exact executable/acquisition checks remain required. Unit tests stay private alongside modules; fixture data uses the crate's `tests/fixtures/` and public process tests use `tests/commands.rs`.

## Inputs and prerequisites

Read the [primary release plan](2026-10-13-attested-release-implementation.md), [#1235 contract](../specs/2026-10-09-1235-release-attestation-design.md), PR #1292's packaging spec/plan and its final reviewed revision. At planning time #1292 is documentation only; do not assume its workflow/helper paths exist. Fetch or stack the owning changes before editing them. This plan supplements that plan rather than copying its independent publish job.

The governing draft #1235 index v1 covers three raw CLI binaries plus Fastly, no package-inventory reference and no Linux ARM64. The package integration therefore requires a reviewed additive contract and new schema version. Task P1 MUST approve it before P2–P7 implementation. Neither package provenance alone nor SHA256SUMS satisfies the executable's SPDX/provenance graph.

**Selected initial integration:** preserve the existing three qualified CLI targets. Produce three Linux x86_64 installer formats and two macOS archives (five packages). This deliberately does not complete #863's Linux ARM64 requirement. Task P8 independently extends canonical qualification/SBOM/policy to ARM64 before publishing its three packages. If maintainers require all eight installer assets in the first packaging release, P8 becomes a prerequisite before that release; no half-qualified ARM asset is slipped into the three-target cohort.

Initial #1235 release can happen first; packaging appears on a later new tag once its approved contract is enabled. Never backfill packages into an immutable release. No implementation step may remove #1262's raw assets to make room for packages.

### Task P1: Approve the cross-PR contract and schema migration

**Modify after owning branch is available:** `docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md`, `docs/superpowers/plans/2026-10-12-cli-release-packaging.md`, #1235 contract.
**Create:** `docs/superpowers/specs/schemas/release-index-v2.schema.json`, `cli-packages-v1.schema.json`; keep index v1 immutable for historical verification.

- [ ] Write a concrete spec delta: one release.yml owner; reusable release-build.yml signer; canonical three CLI profiles plus Fastly unchanged; five initial wrappers; ARM enablement separately selected; package code/member evidence is additive.
- [ ] Agree on #1262's GNU/macOS baselines and real Intel startup. Do not treat Ubuntu 22.04/glibc 2.35 or Rosetta alone as satisfying GNU 2.34/Intel qualification. Keep package ecosystem smoke tests as additional evidence.
- [ ] Resolve version policy explicitly: packaging milestone chooses #1292's tag equals owning CLI Cargo version, with documented maintainer bump/changelog procedure. This is a reviewed narrower packaging prerequisite, not an attempt to embed external tag into binaries. Initial un-packaged #1235 tags need not retroactively adopt it.
- [ ] Approve index v2's optional `package_inventory` reference and its attestation references with exact name/length/digests. Enable it only in the reviewed `phase1-fastly-cli-packaged` cohort and updated consumer policy. Approve `ts release verify <tag> --package <package-asset> --expected-source-commit <commit>` as the distinct package-selection interface; `--package` and `--asset` are mutually exclusive. Older v1 consumers reject unsupported v2, with an independently trusted verifier upgrade path documented.
- [ ] Define package inventory records: schema/source/cohort/recipe/policy identity, each package name/format/architecture/size/SHA-256, exact executable member path/mode/size/SHA-256 and canonical raw asset name. Refer to raw asset digests, not release-index digest: index → inventory → package/member MUST NOT form a self-hash cycle. Bundle references are in the index, not inside the inventory that the bundles authenticate.
- [ ] Replace #1292's signed/annotated `git tag -s` release instruction with `git tag v<version> <approved-commit>` followed by the explicit tag push. Validate lightweight object type; a tag signature does not replace the approved source/signing policy.
- [ ] Get the delta approved/merged and update plan baseline. Commit `Align CLI packaging with canonical attestation`.

**Exit:** No implementer chooses their own signer, platform floor, ARM assurance, archive relationship or versioning rule.

### Task P2: Consume canonical executables without transformations

**Modify/create according to #1292 ownership:** `scripts/release/package-linux.sh`, `package-macos.sh`, `packaging/nfpm.yaml`, release tool private `package::tests` and `tests/commands.rs`.

- [ ] Write tests rejecting wrong source/target/role, source recompilation, strip/sign hooks, unverified raw input, altered member and CLI B output substituted for A.
- [ ] Derive `HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')`; run `cargo test -p trusted-server-release --target "$HOST_TARGET" package::tests` with the primary plan's nonzero-selection guard; expect missing integration behavior. These local source-built commands do not qualify native bootstrap.
- [ ] Accept only Task 12/13 primary-plan validated A output and identity table. All final stripping/signature transformations occur in the canonical recipe before its digest/SBOM, never in packaging. Remove #1292's independent strip step and compiled-cache restoration from the canonical build path.
- [ ] Add packaging configuration for the qualified x86_64 Linux executable and both Apple executables. Preserve member bytes; set documented installation modes. nfpm/tool acquisition has reviewed version/digest verification, not a floating installer.
- [ ] Test canonical file unchanged before/after all package formats. Commit `Package the canonical CLI bytes without modification`.

### Task P3: Inspect whole package structure and bind members

**Extend:** release tool `src/package.rs`, `src/distribution.rs` and private registered tests; add adversarial data under `crates/trusted-server-release/tests/fixtures/packages/` and process cases to `tests/commands.rs`. Proposed fixed commands are `trusted-server-release inspect-cli-package` and `make-package-inventory`, only after P1 approval.

- [ ] Write tests for nested traversal, links/hardlinks, duplicate members, oversized compressed/decompressed content, extra executable members, wrong architecture/mode, substituted maintainer script, unsupported compression and altered ts bytes.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" package::tests` and `distribution::tests` separately with the nonzero-selection guard; expect absent inspector.
- [ ] Implement bounded format-specific inspection: Debian ar/control/data archives, RPM payload/header, Arch tar and macOS tar. Use maintained parsers/pinned utilities where possible. Each adapter validates allowed installation paths, executable ts, license/readme and explicitly approved metadata/scripts; reject unknown code-bearing entries. A raw tar scan is insufficient for .deb/.rpm.
- [ ] Compare extracted executable bytes against canonical retained SHA-256/size, check 0755 installation mode, then hash the final outer package. Generate deterministic inventory only after every selected wrapper passes. Do not compare an archive digest directly to executable digest.
- [ ] Validate closed package schema and source/target/profile joins with the canonical index data. Reject inventory attempts to approve an unknown raw target. Commit `Bind installer packages to canonical CLI members`.

### Task P4: Smoke-test installation and provenance-aware extraction

**Modify:** #1292 smoke helpers/workflow jobs; release tool private package-smoke tests and public process fixtures.

- [ ] Test that failed install/startup/version or member mismatch blocks the selected package cohort; metadata-only architecture checks do not count as execution qualification.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" package_smoke` with the nonzero-selection guard after registering these cases; expect incomplete checks.
- [ ] Install Linux wrappers in reviewed clean Ubuntu/Fedora/Arch environments, run version/help/config-init outside a checkout, and compare installed /usr/bin/ts to canonical digest. Preserve the GNU compatibility qualification independently of package-manager tests.
- [ ] Extract/run macOS archives on actual qualified ARM/Intel hosts. Rosetta may be extra coverage, never the sole Intel gate. Restore executable permissions without altering bytes; verify loader requirements.
- [ ] Record exact container/tool/OS identities and outcomes. Smoke images MUST be pinned and qualified, not `latest`. Commit `Qualify installed CLI package behavior and identity`.

### Task P5: Sign package evidence inside the existing trusted boundary

**Modify:** `.github/workflows/release-build.yml`, release tool `src/distribution.rs`, workflow contract tests.

- [ ] Test package metadata cannot replace executable SPDX/provenance, wrong signer/action/run, modified outer bytes, inventory-member mismatch and unapproved package subject.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" distribution::tests` and `workflow_contract` separately with the nonzero-selection guard; expect package evidence unsupported.
- [ ] Reuse exact same-revision signer and pinned attest 4.2.2; do not keep #1292's direct v2 provenance action in publish. Package provenance names each final package plus authenticated package inventory as exact subjects. Raw executable proofs still name only their approved executable/companions.
- [ ] Add a signed package-inventory proof and index v2 references for inventory/bundles. Root index is assembled after package proofs exist and covers them; no signature/checksum cycle. For the CLI's component composition, consumers use the executable's SPDX proof. Do not advertise an enclosing installer SBOM unless a separate container-level scope is actually generated.
- [ ] Verify actual pinned certificates/provenance and all wrapper/member edges before publication. Commit `Attest CLI installer and member evidence`.

### Task P6: Extend consumer policy and bootstrap before v2 publication

**Modify:** CLI `src/release/policy.rs`, `documents.rs`, `evidence.rs`, `release-policy.json`, release verification/CLI tests, `docs/guide/release-verification.md`, packaging install guide.

- [ ] Add tests for v1 historical acceptance, v2 package selection, unknown package/version, altered inventory/member, verified outer package with unverified executable and the inverse.
- [ ] Run CLI release unit/process tests; expect unsupported v2.
- [ ] Implement the reviewed v2/member policy without letting a downloaded index select arbitrary package kinds or actions. Verify package signature/official membership, authenticate inventory, verify raw executable/SBOM graph and prove exact installed/extracted member equality before recommending installation/execution.
- [ ] Implement the exact package-selection syntax approved in P1; do not overload `--asset` silently to turn arbitrary archives into canonical executable subjects. Use the same internal verification library, with a distinct verified-package result and retained-member identity.
- [ ] Qualify independently trusted source/previous-verifier bootstrap for first v2 use. A CLI that only supports v1 cannot automatically authorize its own unverified upgrade. Commit `Verify packaged CLI members under reviewed v2 policy`.

### Task P7: Integrate one complete immutable publication

**Modify:** `.github/workflows/release.yml`, package release helpers, `CONTRIBUTING.md`, `CHANGELOG.md` and tests.

- [ ] Test incomplete wrappers, mixed source versions, package-only draft, missing raw/SBOM assets, an existing immutable v1 tag and uncertain publish response.
- [ ] Run `cargo test -p trusted-server-release --target "$HOST_TARGET" publication::tests` with the nonzero-selection guard; expect package cohort unsupported.
- [ ] Publish exact canonical plus selected wrapper assets/evidence/checksums through the single owner. No independent #1292 publish job races or finalizes the tag first. Require all selected wrapper gates, complete downloads/joins and post-publication immutable checks.
- [ ] Rehearse the whole v2 release and trusted package install in an authorized disposable repo, then run all AGENTS gates and producer/consumer tests. Only a separately authorized new production tag follows.
- [ ] Commit `Publish canonical and packaged CLI evidence together`.

### Task P8: Qualify Linux ARM64 before expanding packaging scope

**Modify through a separate reviewed extension:** cohort, tools, versioned schema, consumer policy, native recipe/composition fixtures and package matrix.

- [ ] Obtain approval for an aarch64-unknown-linux-gnu canonical executable, provenance-sbom profile and qualified compatibility floors. Until then it is unsupported, not a weaker undocumented extra asset.
- [ ] Build actual ARM64 A with fresh JS/materials, validate native/external components, generate full SPDX and attest it under the same policy. Native B remains a bounded optional measurement unless explicitly promoted.
- [ ] Run startup on actual ARM64 baseline hosts and each claimed package ecosystem. If an Arch ARM64 runtime cannot be qualified, either retain an explicit metadata-only distribution limitation approved in the packaging spec or defer that package; do not advertise fully smoke-tested execution.
- [ ] Approve the `phase1-fastly-cli-arm64-packaged` cohort with five canonical executables and eight wrappers, and update the versioned schema/producer/consumer expected sets before runs; repeat P2–P7 for ARM packages. A failed selected ARM A/SBOM/runtime gate blocks the expanded cohort, with no after-failure fallback.
- [ ] Commit `Qualify and attest the ARM64 CLI distribution`; close #863 only when its approved platform/package acceptance is satisfied.

## Completion and review

P1–P7 produce verifiable packages for the initially qualified targets on a new release. P8 completes the original all-four-platform packaging intent after qualification. Packaging does not weaken or retroactively expand existing #1235 releases. Run each new focused test first, then the primary plan's full AGENTS command list and package-manager/actual-host qualification. Review the complete evidence graph, source/tag/version consistency and first-use bootstrap before publication.
