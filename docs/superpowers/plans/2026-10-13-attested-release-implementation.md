# Attested release binaries and verified consumption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish and verify the four canonical Fastly/`ts` artifacts required by #1235, with complete evidence and a validated manual Fastly production route.

**Architecture:** Controlled producer scripts build and describe bytes; a reusable workflow signs only validated outputs; one caller publishes the complete immutable distribution. A host-only Trusted Server verifier separately establishes build evidence, publication and consumer authorization before retaining verified bytes. Packaging and integrated EdgeZero deployment are separate milestones that consume these boundaries.

**Tech Stack:** Rust 1.95.0/2024, existing error-stack/serde/serde_json/tokio/reqwest/tempfile, workspace sha2, Python unittest and pinned JSON Schema validation, Node 24.12.0/npm, Vite/Rollup, SPDX 2.3, GitHub CLI 2.97.0, actions/attest 4.2.2, Fastly CLI 15.1.0.

---

## Authority, baseline and scope

Planning baseline: `44c5b5620bfb1f82e5b959ab5ba8ec69c7091e52`, inspected 2026-10-13. The governing [contract](../specs/2026-10-09-1235-release-attestation-design.md), [feasibility register](../specs/2026-10-12-1235-release-attestation-feasibility.md) and [index schema](../specs/schemas/release-index-v1.schema.json) remain drafts awaiting maintainer approval. Delivery direction on 2026-10-13 is to develop/review the spec, plans and implementation together on `spec/1235-release-attestation` in PR #1262, with no new Trusted Server PR. The reviewed plan guides implementation here; maintainer contract/code approval remains required before merge, and the plan does not qualify a release or authorize live publication/deployment. Task 1 records this delivery contract and the unchanged qualification prerequisites.

#1214 and #1213 are present at this baseline. Extend their merged private JS output/prebuilt interface and source-version mechanism; do not reimplement the historical audit snapshots. Source deployment currently delegates to EdgeZero and no release workflow/verifier exists.

The initial cohort is exactly:

| Canonical asset               | Required assurance                                         | Blocking qualification                                                             |
| ----------------------------- | ---------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `trusted-server-fastly.wasm`  | `provenance-sbom-repeatable`; default, no reusable sandbox | Final complete-byte A/B equality, real SBOM/runtime and verified manual submission |
| `ts-x86_64-unknown-linux-gnu` | `provenance-sbom`                                          | glibc ceiling 2.34; clean UBI 9, Ubuntu 22.04, Debian 12 startup                   |
| `ts-aarch64-apple-darwin`     | `provenance-sbom`                                          | Explicit loader floor 13.0; actual macOS 15 ARM execution                          |
| `ts-x86_64-apple-darwin`      | `provenance-sbom`                                          | Same loader floor; actual macOS 15 Intel execution                                 |

Every selected canonical Build A, SBOM, signature and publication check is mandatory. CLI Build B is a bounded measurement, not a shipping equality requirement. No Linux ARM64, installer packages, other adapter releases, retained-input archive, operator reconstruction or official offline deployment is added by this plan.

Two linked plans keep independent work out of these closure criteria:

- [CLI packaging attestation integration](2026-10-13-cli-packaging-attestation-integration.md): reconcile #1292, package exact canonical bytes and authenticate member identities. Later releases only after the required spec/schema amendments are approved.
- [Fastly prebuilt EdgeZero integration](2026-10-13-fastly-prebuilt-edgezero-integration.md): upstream prepared payload, integrated online production/staging and durable recovery; AC-8 is separate from initial #1235 closure.

## Work breakdown and ownership

Use successive focused commit/review groups within PR #1262: (1) input/fact/JS guards, (2) qualified recipes and composition, (3) collection/signing/publication, (4) verifier/manual route/docs. Each depends on its predecessor; tag enablement waits for all four. No new Trusted Server implementation PR is created. Build scripts can be proved locally before production YAML exists. No intermediate PR may enable an incomplete release trigger.

| File group                                                                     | Responsibility                                                                                           |
| ------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| `scripts/release/release-cohort.json`, `release-tools.json`                    | Reviewed fixed identities, profiles, approved RC branches and qualified environment pins                 |
| `scripts/release/common.py`, `preflight.py`, `prepare.py`                      | Strict bounded JSON, source/material authentication, explicit environment and preparation                |
| `scripts/release/build-js.sh`, `collect-js.mjs`                                | Independent locked JS A/B and final contribution inventory                                               |
| `scripts/release/build.sh`, `inspect-native.py`, `collect-composition.py`      | Fixed artifact recipes, native qualification and observed build evidence                                 |
| `scripts/release/generate-sbom.py`, `collect-builds.py`, `make-index.py`       | SPDX coverage, exact transfer/comparison gates and distribution assembly                                 |
| `.github/workflows/release-validation.yml`, `release-build.yml`, `release.yml` | Same-commit gates; isolated builds/signing; sole draft/publication owner                                 |
| `crates/trusted-server-cli/src/release/`                                       | Host-only reusable verifier: policy, strict documents, bounded gh, GitHub publication and evidence graph |
| `crates/trusted-server-cli/src/commands/release.rs`                            | Public read-only `ts release verify` adapter to the verifier                                             |
| `scripts/release/inspect-fastly-package.py`                                    | Trusted-source manual-route archive inspection, never a replacement verifier                             |
| `scripts/release/tests/`, CLI/core/JS tests                                    | Negative producer fixtures, complete consumer fixtures and actual artifact qualification                 |

Execute each listed negative fixture as a separate red/green microcycle: write one case, run its focused filter, implement the smallest relevant behavior and rerun it, then move to the next case. A task groups related work for review/commit; its fixture list is not one giant implementation step.

New files below are proposed, not existing APIs. Keep security parsing and operation decisions in focused modules. Use concrete error-stack reports, top-level imports, descriptive assertions and project commit conventions. Reuse existing libraries; add host-only sha2 and a pinned schema validator only with dependency justification. An archive parser belongs to the manual helper initially, so do not add a Rust archive dependency merely for future deployment.

### Task 1: Freeze approved scope and reconcile the packaging owner

**Files:** Modify this plan and the governing spec only for approved corrections; reference PR #1292's `docs/superpowers/specs/2026-10-12-863-cli-release-packaging-design.md` and `docs/superpowers/plans/2026-10-12-cli-release-packaging.md` after their owning branch is available.

- [ ] Read the current spec revision; record its commit and the same-branch/same-PR delivery direction. Work through reviewed tasks in PR #1262; obtain maintainer contract/code approval before merge. Do not require a separate spec merge or open another implementation PR.
- [ ] Record one publication owner, `.github/workflows/release.yml`, and one signing identity, same-revision `release-build.yml`. #1292 must not independently publish an overlapping immutable tag while #1235 assets are still pending.
- [ ] Confirm the current four-artifact cohort and three CLI baselines. Initial #1235 delivery may precede packaging on a different new tag. Adding package records or ARM64 requires the linked plan's approved delta, never a covert schema expansion here.
- [ ] Settle one strict producer control channel: `TRUSTED_SERVER__RELEASE_BUILD=1`, injected by trusted recipe after environment sanitation, recorded as an input, rejected if present with another value. This is a fact/input guard, not the prohibited parallel JS fallback mode. Name the checked prebuilt inventory input `TSJS_PREBUILT_INVENTORY`.
- [ ] Confirm permission policy remains runtime-variable; conflicting #1045/#1084 code blocks release enablement. Record the owner and dependency rather than designing a new permission transport.
- [ ] Review milestone exits with maintainers in PR #1262. Commit any approved documentation delta as a focused commit on this branch with `Clarify release implementation prerequisites`.

**Exit:** An engineer can identify approved source, scope, producer controls and the single release authority; no unspecified cross-PR ownership decision remains.

### Task 2: Establish producer fixtures and strict document utilities

**Create:** `scripts/release/common.py`, `scripts/release/tests/test_common.py`, `scripts/release/tests/fixtures/README.md`, `scripts/release/requirements.txt`.

- [x] Write unittest cases for duplicate keys, nonfinite JSON, trailing controls, unsafe names, depth 65, oversized input, negative lengths and ambiguous duplicate identities.
- [x] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_common.py' -v`; expect failure because the module does not exist.
- [x] Implement strict byte-first loading, bounded reads, SHA-256, safe flat asset names, deterministic output and atomic writes. Start duplicate handling with:

```python
def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result
```

Use `json.loads(..., object_pairs_hook=unique_object, parse_constant=reject_constant)` after size/depth checks. Hash original file bytes, not parsed/reserialized objects.

- [x] Qualify and pin the schema-validator distribution and hashes in `requirements.txt`; install through a private venv. Add explicit dependency installation to producer validation, rather than relying on ambient Python packages.
- [x] Re-run the tests; expect all negatives rejected and deterministic positive bytes. Commit `Add strict release document utilities`.

### Task 3: Encode cohort, tools and fail-closed release preflight

**Create:** `scripts/release/release-cohort.json`, `release-tools.json`, `preflight.py`, `tests/test_preflight.py`, `crates/trusted-server-cli/release-policy.json`.

- [x] Write cases for missing/extra artifacts, profiles changed after failure, unknown target/feature, missing pins and every skip value (`""`, `"0"`, `"1"`). Add wrapper, Cargo config, NODE_OPTIONS, native selector and source mutation cases.
- [x] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_preflight.py' -v`; expect missing module/behavior failures.
- [x] Create the tracked consumer/producer policy bytes now, with the spec's concrete fixed identity/profile/action/bounds fields, so recipe/index assembly can bind their source identity before signing. Runtime enforcement is implemented in Tasks 17–20; this source-controlled contract is not a downloadable policy override.
- [x] Map the four asset IDs to fixed package/bin/target/features. Never accept shell commands from JSON. Pin Rust/Node/npm, gh 2.97.0, attest 4.2.2/full SHA and Fastly 15.1.0; populate actual binary/image/SDK digests only from authenticated acquisition and Task 8 qualification. Missing qualified pins fail `--require-qualified`; no dummy digest is accepted.
- [x] Inspect the inherited environment before clearing it. Reject skip presence and unreviewed override channels; create a fresh allowlisted child environment. Remove publisher prefixes and inject only approved commit/control facts. Derive epoch from the approved Git commit's committer timestamp. Check tracked bytes plus unexpected source-only files, modes and links before/after execution.
- [x] Select both generic and target-suffixed AWS-LC selectors, bundled SQLite, Cargo config/home layers, sysroot/tool paths and Vite mode. Set `AWS_LC_SYS_USE_SYSTEM=0`, `LIBSQLITE3_SYS_USE_PKG_CONFIG=0`; reject alternate selectors rather than merely setting weaker generic defaults.
- [x] Re-run fixtures, including a fictional operator-config independence case. Commit `Define fixed release inputs and preflight policy`.

**Interface:** `python3 scripts/release/preflight.py --cohort scripts/release/release-cohort.json --source-commit <approved-commit> --require-qualified`; exit 0 only for a complete selected contract, exit nonzero before preparation/signing otherwise.

### Task 4: Reconcile immutable build facts and CLI source resources

**Modify:** `crates/trusted-server-core/build.rs`, `build_support/git_version.rs`, `tests/git_version_resolve.rs`, `src/lib.rs`; `crates/trusted-server-cli/src/run.rs`, `src/prebid_bundle.rs`.
**Create:** core `src/build_info.rs`; CLI `build.rs`, `tests/release_build_facts.rs`.
**Modify when adding the owning facts surface:** `crates/trusted-server-adapter-fastly/src/main.rs`; retain existing version-header/service-version behavior.

- [x] Add strict full-commit tests: missing/malformed/branch/tag values fail in release mode without local-git fallback; development behavior stays covered. Add two external tags/same source yielding identical facts, owning-target/variant checks and compiled-path fallback rejection.
- [x] Run `cargo test -p trusted-server-core --test git_version_resolve` and `cargo test -p trusted-server-cli --target aarch64-apple-darwin --test release_build_facts`; expect the new behavior to fail.
- [x] Extend the merged #1213 helper using the approved release control. Derive target from Cargo `TARGET`, package version from the owning product, and default/reuse variant at the Fastly owner. Expose typed immutable facts without importing verifier/publisher keys into core. Do not embed tag, run ID, time or final checksum.
- [x] Set clap's version text to package version plus source identity. Official releases compile out `find_js_lib_dir`'s literal developer checkout fallback; preserve explicit/current-checkout resource discovery. Keep the tracked example config as a fictional scaffold, never active settings.
- [x] Run core fact/template tests, CLI tests/clippy and Fastly version-header tests. Outside-checkout help/version/config-init must pass; Prebid source-dependent commands return documented prerequisites. Commit `Derive immutable release build facts`.

### Task 5: Authenticate the existing prebuilt JS interface

**Modify:** `crates/trusted-server-js/build.rs`, `build/bundle_set.rs`, `src/lib.rs` test inclusion.
**Create:** `crates/trusted-server-js/tests/release_prebuilt.rs`.

- [x] Write process/fixture tests for prebuilt mode plus any skip presence, absent inventory, wrong source/digest/size, duplicate IDs, unsafe filenames, missing/empty/extra modules and source changed during copy.
- [x] Run `cargo test -p trusted-server-js --test release_prebuilt`; expect current prebuilt behavior to fail these guards.
- [x] Extend the existing bundle-set validator; require `TSJS_PREBUILT_INVENTORY` in release mode and validate exact input and private copied bytes. Track both paths/control values with Cargo rerun directives. Reject skip before selecting prebuilt/source path; no TSJS_RELEASE_BUILD parallel mode and no read from old `dist/`.
- [x] Preserve ordinary development's supported fresh source build. In release Rust jobs require prebuilt inputs and prohibit npm invocation; a fake failing npm proves it is never run.
- [x] Run JS crate tests, Fastly/CLI target checks and prebuilt negatives. Commit `Bind release JS inputs to a checked inventory`.

### Task 6: Collect JS contributions without changing bundle bytes

**Modify:** `crates/trusted-server-js/lib/build-module-options.mjs`, `build-all.mjs`.
**Create:** `lib/test/release-composition.test.mjs`, `scripts/release/collect-js.mjs`.

- [ ] Write temporary IIFE fixtures for type-only/external Prebid import, eliminated module, transitive npm contributor and plugin/virtual helper. Build with/without recording and assert identical final JS bytes.
- [ ] Run `cd crates/trusted-server-js/lib && npx vitest run test/release-composition.test.mjs`; expect missing recording behavior.
- [ ] Add an opt-in Rollup contribution hook at `moduleBuildOptions()`. Join source/package origins to the lockfile, retain installed/lifecycle/build-tool materials separately, and hash final files after Vite/esbuild. Suppress ambient `.env` loading through the reviewed release invocation; explicit production mode alone is not sufficient.
- [ ] Emit sorted version-1 inventory with source, module names/lengths/digests and origins; ambiguous contributing ownership fails. State conservative post-Rollup reachability limits.
- [ ] Run byte-preservation and ownership negatives plus the full JS test/format checks. Commit `Record release JS composition evidence`.

### Task 7: Prepare and compare two independent JS builds

**Create:** `scripts/release/build-js.sh`, `prepare.py`, `tests/test_prepare.py`, `tests/test_js_release.py`.

- [ ] Write tests for archive/revision mismatch, stale extracted tree, prohibited install download, two bundles differing by one byte, test failure and changed source. Assert no generated output exchange between A/B.
- [ ] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_*release.py' -v`; expect missing orchestration.
- [ ] Preparation verifies registry/git/npm materials and required native esbuild package archives, then freshly materializes each installation. Run pinned `npm ci --offline --include=dev --no-audit --no-fund` with verified freshly prepared caches in two independent workspaces under the selected denial boundary; review/record lifecycle scripts and fail any lifecycle tool fetch. Task 7 initially proves this with local fixtures; Task 8 must qualify the actual hosted JS boundary before Task 15 may use it officially. No restored node_modules/generated JS. Bundle under qualified network denial; compilation-time downloads fail.
- [ ] Compare final core and all tracked integration files byte-for-byte. Publish only A plus inventory after equality. Run release JS tests regardless of a local test flag; verify source-only trees remained intact.
- [ ] Prove a missing prepared tool cannot fetch a substitute; rerun negatives and full JS suite. Commit `Build and compare shared release JS inputs`.

### Task 8: Qualify real platform environments before fixing recipes

**Create:** `scripts/release/qualification/qualify.py`, `socket_probe.py`, `results.schema.json`, `scripts/release/tests/test_qualification.py`; update `release-tools.json` with measured pins.

- [ ] Write schema/preflight fixtures rejecting wrong architecture, missing SDK/tool digest, failed startup, permitted confined sockets and invented host compatibility.
- [ ] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_qualification.py' -v`; expect unimplemented qualification checks.
- [ ] Execute throwaway full recipes on the actual proposed GitHub-hosted environments: digest-pinned GNU builder <=2.34, matching macOS 15 ARM/Intel, explicit loader floor 13.0. Record host, rustup paths, compiler/linker/SDK/native identities. Docker absence on a developer Mac is not replaced by an inferred Linux result.
- [ ] On each recipe run an unconfined socket success control and a confined denied socket probe, then full compilation under the same qualified boundary. Linux container uses process network isolation; macOS profile must permit required filesystem/process/tool activity while denying sockets. Lifecycle preparation is separate and authenticated.
- [ ] Record actual startup/link evidence and complete Fastly runtime request, not only host health. Missing Intel execution or GNU baseline stops qualification. A different platform/cohort requires reviewed spec changes, not a silent downgrade.
- [ ] Pin authenticated observed digests/bootstrap data and add result validation; commit `Qualify controlled release build environments`.

**Gate:** This task produces measured pins, not placeholder configuration. It can run only with the separately authorized disposable hosted resources. It does not publish official tags or deploy a real publisher service.

### Task 9: Implement fixed fresh Rust recipes and native inspection

**Create:** `scripts/release/build.sh`, `inspect-native.py`, `tests/test_recipe.py`, `tests/test_native.py`, `scripts/release/validate-fastly.py`, `tests/test_fastly_runtime.py`.

- [ ] Test unknown asset/variant, reused target output, source/material mutation, unselected native library, GLIBC symbol ceiling violations, runner-only paths and Mach-O deployment-target mismatch. Add role-specific variation fixtures: missing selected B variation, normalized values misreported as varied, unsupported user variation claimed as passed and incorrect observed concurrency/umask.
- [ ] Run both new unittest modules; expect missing recipe/inspection behavior.
- [ ] Implement a closed interface: `build.sh --asset <fixed-id> --role A|B --source-commit <approved> --js-dir <absolute> --js-inventory <absolute> --materials <fresh-root> --output <fresh-root>`. Select package/target/features from reviewed source; JSON never supplies executable commands.
- [ ] Implement checked A/B ambient profiles in the recipe and record them in `release-tools.json`: different external checkout roots, A umask 022/B umask 027, UTC versus a qualified alternate timezone, distinct qualified locale settings, different observed start times and A jobs 4/B jobs 2 within the qualified insensitive envelope. Vary execution user only where Task 8 proves support; otherwise record it unsupported without claiming it changed. Capture both ambient inputs and effective compiler values after sanitation/remapping; fixed internal locale/path/epoch values are normalized, not varied. No unreviewed caller overrides can choose profiles. Tests assert genuinely observed supported differences and reject dishonest/omitted records.
- [ ] Apply Task 8's qualified OS/container denial profile in the actual production `build.sh`, not just the qualification prototype. Every production invocation must record a successful unconfined socket control and denied confined socket probe, then execute Cargo, compiler, proc-macro, build-script and native-tool children inside that same denied boundary. Preparation alone may fetch materials. A build-script/lifecycle download-attempt fixture must fail, and a missing or ineffective confinement proof rejects the output; `--frozen` is not the boundary.
- [ ] Build explicit packages with `--release --frozen --message-format=json-render-diagnostics` after preparation. Keep `debug = 1`. Capture compiler/build-script output separately from diagnostic stderr; copy only final output after any reviewed transformations. Do not invoke Fastly source build/annotation. Never strip after evidence/signing.
- [ ] Inspect ELF/Mach-O libraries and paths, run clean Linux startup fixtures and real ARM/Intel macOS startup outside checkout. With differing fictional operator config removed by preflight, require identical sanitized effective source/tool/build inputs for every artifact and reject embedded operator sentinels/config fallback. Require complete executable equality for Fastly; preserve native nonmatching outcomes as measurements rather than adding a native repeatability shipping gate. Separately show runtime configuration can change without rebuilding.
- [ ] Before accepting each final Fastly A/B output, validate actual core-Wasm format with a pinned maintained validator and run a positive publisher-origin response/injection fixture through Viceroy using those exact final bytes. The fixture must not invoke Cargo/Fastly build or modify the wasm. Assert final hash unchanged, retain runtime outcome, and test non-wasm/component/wrong-ABI/health-only success rejection. These are required producer gates, separate from Task 8 throwaway qualification and later live deployment.
- [ ] Run recipe/native negatives and one actual artifact per selected environment. Commit `Build and inspect canonical release executables`.

### Task 10: Record selected Cargo units and native materials

**Create:** `scripts/release/collect-composition.py`, `tests/test_composition.py`, `tests/fixtures/composition/`.

- [ ] Add fixtures for host/target dual-role packages with different unit features, ambiguous source IDs, dev-only graph entries, unknown native origin, system substitution and prepared-but-discarded CLI JS.
- [ ] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_composition.py' -v`; expect absent collector.
- [ ] Join explicit-target compiler/build-script messages with metadata/tree and selected material identities; preserve each observed unit's features/context. Package union features cannot stand in for effective per-unit features. Retain raw observations for audit; do not claim an exact compiler dependency DAG.
- [ ] Capture AWS-LC/ring/SQLite native source and output archive hashes/link directives, Rust sysroot and external platform libraries. Validate generic/target-specific selection against the recipe. Distinguish embedded components, external requirements and build tools.
- [ ] Inspect final files conservatively for complete JS module strings. Record absence explicitly; Fastly and CLI do not need the same retained set. Do not force unused JS into CLI. Commit `Collect artifact-specific build composition` after negatives pass.

### Task 11: Generate complete artifact-specific SPDX

**Create:** `scripts/release/generate-sbom.py`, `tests/test_sbom.py`, `tests/fixtures/spdx/`.

- [ ] Write cases for incorrect root digest, missing native coverage, ambiguous package identity, unknown license, duplicate SPDX IDs, dangling relationships, uncompiled dev dependencies, type-only/virtual JS and oversize predicates.
- [ ] Run `python3 -m unittest discover -s scripts/release/tests -p 'test_sbom.py' -v`; expect missing generator.
- [ ] Emit SPDX 2.3 JSON from Task 10 normalized evidence. Root checksum is final executable SHA-256; contributors/materials/tools/external requirements have distinct relationships. Unknown licenses use NOASSERTION; missing contributing origin/coverage fails. Validator uses a pinned authoritative SPDX schema.
- [ ] Generate full real Fastly and all three CLI SBOMs; schema-valid toy fixtures are not sufficient. Validate size <=16 MiB and document equality semantics: reject duplicate keys, ignore object order, preserve array order.
- [ ] Re-run fixtures plus actual coverage inspection. Commit `Generate validated per-artifact SPDX evidence`.

### Task 12: Bind transfers and enforce selected comparisons

**Create:** `scripts/release/collect-builds.py`, `tests/test_collection.py`.

- [ ] Test stale/wrong-run/source transfers, reused A/B artifact IDs, transfer hash warning, traversal/duplicate ZIP members, missing A, changed inventory, missing supported B variation or dishonest normalized/unsupported claims, absent final-byte runtime proof, missing/failed production socket controls or an unconfined build invocation, and a one-byte final wasm difference.
- [ ] Run collection tests; expect missing validator.
- [ ] Accept only expected upload-step outputs: IDs/archive digests mapped by trusted workflow dependencies to job/attempt/role/recipe. Re-download exact current-run artifacts and validate REST repository/run/source plus transfer bytes. Artifact API does not independently identify the producing job/attempt. Verify bounded declared ZIP members before extraction.
- [ ] Match expected role-specific ambient profiles from the approved recipe against observed A/B records before signing. For mandatory Fastly A/B, all qualified supported dimensions must differ as selected; normalized values remain explicitly identified and omitted supported dimensions fail. Successful native comparisons use their selected observed profile and result binding, but a bounded native B failed/unavailable before environment setup records unavailable fields honestly and cannot be rejected for lacking observations it never produced. Add that exact nonblocking fixture; all native A qualification/composition remains mandatory. Verify final-byte core-format/publisher-request results for both Fastly outputs. Fresh-job identity alone cannot satisfy the variation or runtime contract.
- [ ] Compare complete final Fastly files with no normalized/excluded regions. For native B produce exactly matched/mismatched/failed/unavailable within its deadline; native A failures always block. Record ambient variations and normalized internal values honestly.
- [ ] Preserve A as canonical only after all mandatory requirements pass. Add explicit tests that optional B failure cannot hide any A/composition/JS failure. Commit `Validate release transfers and comparison outcomes`.

### Task 13: Assemble and validate the exact evidence graph

**Create:** `scripts/release/make-index.py`, `validate-distribution.py`, `tests/test_index.py`, `tests/fixtures/index/`.
**Modify only with approval:** the governing index schema.

- [ ] Port positive and adversarial schema fixtures: exact four artifacts, unknown capabilities, duplicate names/keys, source/cohort/recipe mismatch, unsafe names/newlines, altered lengths/digests and role-specific limits.
- [ ] Run index tests; expect missing assembly/semantic checks.
- [ ] Assemble unsigned companions from successful observed outputs, then artifact provenance/SBOM bundles, then the signed index. Index names/hashes all companions and shared JS inventory; exclude own digest/bundle. Recipe/rebuild records are authenticated informational data, not executable programs or independent trust.
- [ ] Require SPDX parsed predicate/document equality plus separate file hashes. Producer verifies every exact expected provenance subject; no additional unapproved code subject. SHA256SUMS covers every other published asset and not itself.
- [ ] Cross-check schema and semantic joins on all real artifacts before signing/rehearsal. Commit `Assemble the authenticated release distribution`.

### Task 14: Expose trusted same-commit validation

**Create:** `.github/workflows/release-validation.yml`, `scripts/release/tests/test_workflow_contract.py`.
**Modify:** `.github/workflows/test.yml`, `format.yml` only to extract/reuse existing required jobs without losing existing triggers/gates.

- [ ] Add static workflow fixtures for wrong SHA/event/ref, unauthorized RC branch, skipped/path-filtered gates, arbitrary commands, excessive permissions and any floating external action ref in the complete release/validation invocation.
- [ ] Run workflow contract tests; expect absent callable validation.
- [ ] SHA-pin every external action in release.yml, release-build.yml and all called release validation jobs, including checkout/setup/cache/upload/download and attest. Preserve readable version comments, audit dependencies and reject floating @v\*/main refs. Local workflow calls are allowed only at the caller's checked source revision. Existing test/format action refs must not be inherited unreviewed into this path.
- [ ] Expose reviewed workflow_call or explicit jobs running every current AGENTS gate at the tag SHA. Maintain PR/main tests; RC is permitted only by exact reviewed cohort branch entries. Validate strict SemVer/lightweight tag independently; annotated tags fail.
- [ ] Wait up to 90 minutes for this invocation's required terminal validation results; fail cancelled/skipped/timeout results with rerun guidance. Do not trust arbitrary named successful checks or unrelated main commits.
- [ ] Include JS build/tests/format, all Rust aliases/parity/CLI/build-digest, docs/Markdown and `scripts/lint-wasm-clock.sh`. Commit `Validate release source at the exact tag commit`.

### Task 15: Orchestrate independent builds and isolated signing

**Create:** `.github/workflows/release-build.yml`; extend workflow contract fixtures.

- [ ] Test never-started/queued or scheduler-cancelled optional native B jobs as well as pre-step failure: the trusted collector must emit a bounded failed/unavailable measurement with absent observations represented honestly and no fabricated upload identity. Confirm the workflow cannot wait indefinitely or hide a mandatory A failure.
- [ ] Test topology: shared JS A/B, separate fresh Fastly A/B, three mandatory native A jobs, bounded optional native B jobs, validated current-run collector and no signing credentials in executable/lifecycle jobs. Require job configurations to apply Task 9 role profiles and publish observed variation/runtime records; tests reject default-only A/B jobs or collectors that bypass these gates.
- [ ] Run workflow tests; expect absent graph.
- [ ] Call local reusable workflow at source revision. Build jobs are read-only. Collector uses Task 12 outputs; signing grants only contents-read/id-token-write/attestations-write, plus actions-read when needed. No artifact-metadata write for file attestations. Caller grants enough permissions for called signing jobs but its own build jobs explicitly narrow them.
- [ ] Invoke pinned attest 4.2.2 twice per canonical asset: exact executable/SBOM/evidence/recipe/rebuild provenance subjects, then executable subject + SPDX predicate. Save named bundles, validate all proofs/joins and sign index after collection. Do not construct caller predicates or sign source supplied commands.
- [ ] Rehearse actual certificates/toolkit fields in a disposable repository with test-only policy fixtures. Verify repository IDs, exact SAN, issuer, build/caller/source/event/run fields for both predicate types. Commit `Sign validated release outputs through the reusable builder`.

### Task 16: Publish through one immutable-release owner

**Create:** `.github/workflows/release.yml`, `scripts/release/publish.py`, `tests/test_publication.py`.

- [ ] Add fake API cases for matching/conflicting draft, missing/altered asset, concurrent rerun, lost response, already published tag, mutable/unknown final state and permission differences on approved RC commits.
- [ ] Run publication/workflow tests; expect absent publication owner.
- [ ] Keep tag-scoped concurrency without cancellation. Draft, upload, re-download and validate the exact asset set before publication. Match source/bytes for draft resume; never overwrite published assets/tags. Publication has contents-write only, no OIDC/provider/settings token.
- [ ] Require maintainer governance approval and immutable/tag-rule setup before enabling tags; afterward read immutable=true and verify GitHub release statement/membership with the normal token. A confirmed mutable release fails publication, is removed only when the workflow release-write role authorizes that still-mutable cleanup, and produces an incident with the cleanup outcome. Never delete an immutable release. Unknown state produces an incident and read-only reconciliation; it must not trigger blind deletion. A lost publish response also triggers read-only reconciliation before another mutation.
- [ ] Exercise all failure paths in an authorized disposable public repository and record measured evidence. Production tag/permissions require separate release authorization. Commit `Publish complete immutable attested releases`.

### Task 17: Implement strict consumer documents and embedded policy

**Modify:** CLI `release-policy.json` from Task 3.
**Create:** CLI `src/release/mod.rs`, `error.rs`, `policy.rs`, `documents.rs`, `src/release/tests/mod.rs`, `src/release/tests/documents.rs`, `tests/fixtures/release/`.
**Modify:** CLI `src/lib.rs`, `Cargo.toml` (host-only sha2/schema validator with justification).

- [ ] Before the first filtered run, wire the host-only `mod release` in CLI `src/lib.rs`, the production child modules in `src/release/mod.rs`, `#[cfg(test)] mod tests` there, and `mod documents` in `src/release/tests/mod.rs`. Add the test cases before checking selection; Rust does not discover this directory automatically. Keep these internals private.
- [ ] Test closed schema/kind/profile/action combinations, duplicate/deep/oversize JSON, self-selected policy, historical policy acceptance/retirement, altered approved-source policy bytes, allowlisted digest paired with the wrong source, unsupported tool and CLI-as-server substitution.
- [ ] Run `cargo test -p trusted-server-cli --target aarch64-apple-darwin release::tests::documents`; expect the intended missing implementation or assertion failure. When it compiles, use the nonzero-selection guard below before accepting red or green; a successful run selecting zero tests is a failure of this plan, not a pass.
- [ ] Implement byte-first duplicate-rejecting parsers and the same schema/semantic fixtures as producer. Embed reviewed policy bytes; compute their SHA-256 externally at runtime. Policy contains no own digest. First bind producer-policy bytes to the independently approved source commit through the fixed official repository path `crates/trusted-server-cli/release-policy.json` (trusted source snapshot or bounded official source API acquisition from that exact commit); never obtain this authority from the release index or an arbitrary supplied path. Match its exact-byte digest/version, then accept only current policy bytes or the reviewed historical allowlist. For offline checks, independently transfer the approved source-policy snapshot alongside other trust inputs. Downloaded metadata never changes consumer policy.
- [ ] Keep the already registered tests active and require nonzero selected counts on every focused rerun; process tests use public CLI dispatch rather than making internals public. Keep concrete release errors as Report<ReleaseError>; convert to the existing String dispatch boundary only at the command adapter. Do not introduce a new generic attestation crate or publisher types.
- [ ] Fix repository/owner IDs, supported combinations, exact gh/action contract and all R-BOUNDS limits. Re-run tests/clippy. Commit `Parse release evidence under embedded consumer policy`.

### Task 18: Implement the bounded isolated gh runner

**Create:** CLI `src/release/gh.rs`, `src/release/tests/gh.rs`.

**Modify:** CLI `src/release/mod.rs`, `src/release/tests/mod.rs`.

- [ ] Register the new gh production modules and `mod gh` test module before the first focused run. Apply the nonzero-selection guard; missing module registration or zero matches must never report a passing security check.

- [ ] Write fake-child tests for hangs/cancellation, simultaneous stdout/stderr saturation, malformed/truncated JSON, exit failure, multiple attestations, token precedence, host/config overrides and log redaction.
- [ ] Run `cargo test -p trusted-server-cli --target aarch64-apple-darwin release::tests::gh`; expect absent runner.
- [ ] Resolve gh once through trusted PATH/explicit trusted installer configuration, require 2.97.0, and retain that executable target. Pass fixed argument vectors with exact certificate identity; never arbitrary forwarded verifier flags. Accept explicit GH_TOKEN then GITHUB_TOKEN, pass only GH_TOKEN, isolate home/config/state, disable prompts, reject loader injection and keep provider credentials out.
- [ ] Concurrently drain bounded pipes: 120 seconds per child, 15 minutes per attempt, 128 MiB stdout/1 MiB stderr. Kill/reap on either limit/cancellation. Use real OS fake-child fixtures, not mocks that omit pipe pressure. Failed exit never yields trusted parsed claims.
- [ ] Re-run runner negatives and an approved signed public fixture under the pinned gh binary. Commit `Bound and isolate release verification subprocesses`.

### Task 19: Verify signatures, evidence joins and official publication

**Create:** CLI `src/release/evidence.rs`, `github.rs`, `artifact.rs`, `src/release/tests/verification.rs`.

**Modify:** CLI `src/release/mod.rs`, `src/release/tests/mod.rs`.

- [ ] Register the new evidence/github/artifact production modules and `mod verification` test module before the first focused run. Apply the nonzero-selection guard; missing module registration or zero matches must never report a passing security check.

- [ ] Add a valid graph and independently altered edges for index, all provenance subjects, SPDX subject/document/root, JS prepared/retained records, policy/source/run/caller identities, signed names, REST lengths/immutability and tag object type.
- [ ] Run verification tests; expect no implemented evidence graph.
- [ ] Verify index provenance before trusting fields. Obtain only approved official GitHub host/repository data with explicit read-only credential and bounded responses. Validate lightweight source target, source approval, independently acquired fixed-path producer policy bytes at that commit, exact signed release names/digests and actual/index/REST sizes. Primitive gh success is insufficient.
- [ ] Check verified certificate numeric IDs, exact SAN/issuer/build config/digest/trigger/run URI in both proofs, and provenance build type/caller/external parameters/resolved source/invocation. SPDX lacks SLSA caller fields; use its certificate plus subject/document constraints.
- [ ] Separate BuildEvidenceVerified, PublicationVerified and consumer ActionAuthorized states; constructors remain private. A retained verified-release-asset owns private bytes/handle and source/policy/role evidence. Read-only CLI verification returns a VerificationReport, not deployment authorization. The linked integration plan adds the private action-scoped VerifiedArtifact only after concrete context approval; an asserted path/hash cannot construct it. Stream bounded asset responses into retained files, enforcing each role limit while downloading; do not buffer a 512 MiB executable through the gh JSON pipe or check limits only after writing it. Rehash before later use and reject offline official-deploy construction.
- [ ] Exercise first-use verification using an independently trusted previous verifier or source-built verifier from independently approved source, never the downloaded executable verifying itself. Commit `Verify complete official release evidence before authorization`.

### Task 20: Expose read-only CLI verification and bootstrap instructions

**Create:** CLI `src/commands/release.rs`, `tests/release_cli.rs`, `src/release/offline.rs`, `src/release/tests/offline.rs`, `docs/guide/release-verification.md`.
**Modify:** CLI `src/commands/mod.rs`, `src/run.rs`, `src/release/mod.rs`, `src/release/tests/mod.rs`; `docs/guide/cli.md`, `.vitepress/config.mts`.

- [ ] Register `mod offline` in both the private production and test roots before its first focused run. Register the public command module/dispatch before process tests; run `release::tests::offline` with the nonzero-selection guard in addition to the CLI process target.
- [ ] Test the required parse path `ts release verify v1.2.0 --asset ts-aarch64-apple-darwin --expected-source-commit <approved-commit>`, invalid source/asset/unsupported offline-deploy flags and no provider/config/build hooks on all failure paths.
- [ ] Run `cargo test -p trusted-server-cli --target aarch64-apple-darwin --test release_cli`; expect unknown command.
- [ ] Add the command adapter to the reusable host verifier. The initial public surface stays the spec's verify/tag/asset/expected-source contract; any cache/export/output flag must be documented and reviewed, not smuggled into an implementation example.
- [ ] Document trusted gh installation and independent source/previous-verifier bootstrap before first executing downloaded ts. A source-built verifier provides approved-source trust, not official binary provenance for itself. Describe exact policy checks after primitive gh signatures; do not instruct self-verification or checksum-only installation.
- [ ] Add an internal build-evidence-only offline verification path, reusing parser/policy/subject/SPDX joins and bounded gh runner with independently approved roots/source/digest. Test it with networking denied and token absent; its result cannot construct publication/deployment authorization. No initial public offline flag is added. Document the pinned primitive commands and the manual companion/document/policy checks needed beyond their exit status; offline success is build evidence only, no official release membership or offline deploy. Run CLI helper, docs lint/format/build and malformed/timeout/zero-provider-write fixtures. Commit `Expose official release verification and trusted bootstrap`.

### Task 21: Validate and document manual Fastly production consumption

**Create:** `scripts/release/inspect-fastly-package.py`, `tests/test_fastly_package.py`.
**Modify:** `docs/guide/release-verification.md`, `docs/guide/cli.md`, `CHANGELOG.md`; correct obsolete config-embedding comments in source/test/setup files identified below.

- [ ] Test accepted four-member layout, generic manifest-only archive, swept-in files, reserved/empty package name, traversal, links, duplicates, wrong module/manifest, malformed/non-core wasm and max-size bounds.
- [ ] Run Fastly package tests; expect absent inspector.
- [ ] Use a fresh private directory, approved-source inspector, an allowlisted deployment-only manifest and verified wasm. `compute pack --wasm-binary` has two output archives; select named archive only after exact layout/content checks. Pack/validate accepting a file does not prove valid Wasm or application execution. Verify valid core format and positive publisher-request fixture separately.
- [ ] Inspector takes the approved expected executable digest and manifest identity from the successful verification/context procedure, rehashes both the retained archive and its actual module member immediately before submission, and never trusts a digest supplied by the archive. Document trusted-source acquisition for the helper; it is not a separately authenticated release asset.
- [ ] Document observed service/base-version bindings and explicit production command with --service-id, --version and --no-default-domain. Explain manifest precedence, automatic clone/activation, possible same-hash skip and inability to supply integrated staging/preactivation guarantees. Serialize manual operator changes. No compute publish/source build is permitted.
- [ ] Run a separately authorized disposable Fastly service submission; compare submitted member and actual returned version, including skip/uncertain cases. Record live proof before declaring AC-9 passed. No customer context or provider credentials enter producer jobs.
- [ ] Correct false config-embedding text in `.github/actions/setup-integration-test-env/action.yml`, `.github/workflows/test.yml`, integration runtime/framework/parity helpers, browser infra helper and Spin app/middleware/route comments; behavior changes remain separate. Commit `Document and validate verified-byte Fastly deployment`.

### Task 22: Full validation, qualification evidence and release handoff

**Modify:** the implementation PR's evidence record and docs; no automatic production mutation.

- [ ] Run the complete command block below and collect exit statuses/log summaries. On local certificate/localhost sandbox failures rerun the same tests with needed access; do not weaken TLS or alter assertions.
- [ ] Rehearse hosted source authorization, shared JS, Fastly A/B, all three native A/SBOMs, optional B terminal failures, actual exact-action signatures, immutable draft/publication and full consumer verification together. Toy/synthetic component proofs cannot replace this complete composition.
- [ ] Recheck permission/config separation, governance/rules/bypass authority, bootstrap docs, actual platform evidence and live manual submission. Missing evidence blocks tag enablement; native B equality, packaging or upstream integration do not add hidden blockers.
- [ ] Review against AC-1–AC-7, AC-9–AC-10; keep AC-8 separate. Use @superpowers:requesting-code-review and @superpowers:verification-before-completion. Maintainer approval precedes any production tag.
- [ ] Commit validated documentation/evidence with `Record initial release qualification`; update existing PR #1262 with implementation and validation evidence only when the required checks pass; never create a replacement implementation PR.

## Focused Rust test selection guard

Before accepting any filtered private-module run, require an actual selected test. Once the target compiles, repeat this guard for `documents`, `gh`, `verification` and `offline`; registration belongs to the preceding task step, not to a later implementation step. An intended compile failure while adding missing behavior can establish the first red state, but a zero-test successful command never can.

```sh
set -e
RELEASE_HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 }')"
RELEASE_TEST_FILTER=release::tests::documents
RELEASE_TEST_LIST="$(mktemp)"
trap 'rm -f "$RELEASE_TEST_LIST"' EXIT
cargo test -p trusted-server-cli --target "$RELEASE_HOST_TARGET" \
  "$RELEASE_TEST_FILTER" -- --list > "$RELEASE_TEST_LIST"
python3 - "$RELEASE_TEST_LIST" <<'PY'
import sys
from pathlib import Path
matches = [line for line in Path(sys.argv[1]).read_text().splitlines()
           if line.endswith(": test")]
if not matches:
    raise SystemExit("Focused release test filter selected zero tests")
print(f"Selected {len(matches)} release tests")
PY
cargo test -p trusted-server-cli --target "$RELEASE_HOST_TARGET" "$RELEASE_TEST_FILTER"
```

## Full implementation verification commands

Run from the repository root with pinned tools and installed npm dependencies. Linux CI uses its host triple; macOS can use the CLI helper. Record each command independently so a later command cannot mask an earlier failure.

```sh
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
cargo test -p trusted-server-core --test template_build_digest
cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings
scripts/lint-wasm-clock.sh
python3 -m unittest discover -s scripts/release/tests -v
```

Run the remaining commands with the repository-pinned Node/npm selected explicitly (subdirectory shell startup can otherwise pick a different Node):

```sh
(cd crates/trusted-server-js/lib && node build-all.mjs && npx vitest run && npm run format)
(cd docs && npm run lint && npm run format && npm run build)
docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"
RELEASE_HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 }')"
cargo run -p trusted-server-cli --target "$RELEASE_HOST_TARGET" -- dev lint domains --staged
git diff --check
git diff --cached --check
```

Stage the reviewed implementation files before the staged-domain check; each grouped command stops at its first failure. New producer/schema dependencies must be installed at their exact reviewed versions first. No bare workspace cargo test/build or global all-features clippy.

## Traceability and milestone exits

| Requirements                          | Tasks / principal negative proof                                                                   | Acceptance |
| ------------------------------------- | -------------------------------------------------------------------------------------------------- | ---------- |
| R-COHORT, R-PLATFORM, R-VARIANT       | 1,3,8,9: exact cohort/profile, incompatible native runtime, unsupported reuse                      | AC-2, AC-6 |
| R-CONFIG, R-INPUTS, R-ENV             | 3–5,7,9: operator inputs, unexpected source files, skips/overrides, source integrity               | AC-2       |
| R-JS                                  | 5–7,10: locked A/B, inventory joins, input versus retained bytes                                   | AC-3, AC-4 |
| R-BUILD, R-REPEAT, R-NATIVE           | 8–12: actual confinement, complete wasm equality, valid transfers, honest native outcome           | AC-3       |
| R-SBOM                                | 10,11,13,19: real Rust/native/JS coverage and SPDX root/predicate equality                         | AC-4       |
| R-FACTS                               | 4: strict full source, owning target/variant, external tag and unchanged service identity          | AC-5       |
| R-SOURCE, R-SIGN, R-PUBLISH           | 14–16: approved lightweight main/RC, exact-commit gates, isolated signer, immutable reconciliation | AC-6       |
| R-INDEX, R-EVIDENCE, R-POLICY         | 2,13,17,19: schema/semantic joins, subjects and independent policy identity                        | AC-4, AC-7 |
| R-BOOTSTRAP, R-BOUNDS, R-PUBLICATION  | 18–20: trustworthy first use, limits/pipe pressure, exact REST/signed membership                   | AC-7       |
| R-OFFLINE                             | 19,20: approved build proof only; offline official deploy rejected                                 | AC-7       |
| R-DEPLOY, R-PACK, R-CONTEXT, R-MANUAL | 19,21: verified bytes before mutations, explicit manual limits and live submission                 | AC-9       |
| R-EDGEZERO, R-RECOVERY                | Linked upstream plan, not first-release closure                                                    | AC-8       |
| R-FOLLOWUP                            | 1,20–22: explicit unsupported adapters/reconstruction/offline; January supersession                | AC-10      |

A completed task means its described tests pass and reviewed code exists, not merely that its checkbox was checked. Recipe pins/qualification are concrete outputs of Task 8, not assumed values. An unavailable required host blocks the selected contract; it does not authorize weakening it. For optional CLI B only, bounded unavailability is an honest nonblocking result.

## Planning-only verification and handoff

For these Markdown plans, validate formatting, links, shell syntax, requirement coverage, task dependencies and independent review. Do not claim that runtime tests verify implementation that has not been written. Continue in the main checkout `/Users/prk-jr/Desktop/opensource/rust/trusted-server`, branch `spec/1235-release-attestation`, and existing PR #1262. The temporary planning worktree/branch were used only to draft the plans; their commits have been incorporated into this branch.

## Review record

On 2026-10-13, independent whole-package review approved these three plans after three passes; separate producer and consumer/upstream reviews informed the corrections. Findings resolved: required Fastly B variation enforcement, final-byte application qualification, mutable-publication recovery, lightweight tagging, native comparison/profile boundaries, all-action SHA pins, approved-source policy-byte binding, lifecycle network denial, and noncircular staging qualification with complete durable outcomes.

This is planning-completeness approval, not maintainer spec approval or proof of implementation/hosted qualification. Document verification covers formatting/domain rules, relative references, shell/Python example syntax and all 29 requirement mappings.

A subsequent fresh spec-to-plan review on 2026-10-13 found three execution-coverage gaps despite the earlier review: applying actual production network confinement, registering private Rust tests before filtered runs, and independently qualifying integrated production activation. All were corrected; the two fresh reviewers then reported clean primary and milestone alignment. The optional native scheduler-failure fixture was also strengthened. These reviews do not replace the future implementation and qualification gates.

## Implementation execution record

Implementation starts on 2026-10-13 at `d157766fa4b9c13b9b4e666a8961009f1133857b`, the current governing-spec revision, in the main checkout on `spec/1235-release-attestation` and PR #1262. The schema revision inspected is `24a8a7180a32a30e0438d9f5b77697c058c6e9c3`. These are investigation revisions, not an approved release source commit. The user has authorized implementation on this branch; maintainer contract/code approval and release qualification remain outstanding.

Task 1 investigation confirms the four-artifact cohort, its existing compatibility baselines, `TRUSTED_SERVER__RELEASE_BUILD=1` and `TSJS_PREBUILT_INVENTORY`. No release workflow exists at this revision. The selected publication owner remains `release.yml`, with same-revision `release-build.yml` owning signing. PR #1292 is a documentation draft at `bbba62aca580b94192628495826ae9551efad24b`; its package-only distribution, additional target, compatibility changes and annotated tags require reconciliation through the linked packaging plan. It must not independently publish an overlapping immutable tag.

Permission-policy reconciliation remains an enablement dependency owned by the #1045/#1084 work (`jwrosewell`), not a blocker for local implementation. The current checkout loads app configuration at runtime and has no embedded permission-policy module. #1045 at `a028c00da096cc722dd95307ab8ee0e3514a448e` embeds `config/permissions/sample.yaml`; #1084 at `3c43876fc8b0d316d812a8c74aba47cc87d17279` explicitly defers runtime policy. Those proposals must satisfy R-CONFIG before release enablement. This record neither approves their embedded policy nor selects a new transport.

Task completion requires implementation evidence and both independent reviews. Task 1's owner reconciliation and maintainer milestone review remain pending; subsequent local implementation can proceed. Hosted qualification, governance setup, signing/publication rehearsal and live Fastly submission remain distinct evidence gates, and no production release trigger is enabled by this record.

Task 2 is implemented in `1cfca1357`, with review corrections in `c29940908` and `fbb88d2e`. Independent spec and quality reviews pass after resolving tuple-contained key validation, serialization exception consistency and iterator identity selectors. The controller's fresh utility run passes 35 tests. A fresh private Python 3.12 environment installs all six pinned dependencies with `--require-hashes --only-binary=:all:`; `pip check`, the normative release-index schema check and Draft 2020-12 positive/negative semantics pass. The reproducible installation/validation procedure is in `scripts/release/tests/fixtures/README.md`; Task 14 must wire this explicit installation into hosted producer validation before using the validator. These checks establish utility behavior and local validator compatibility, not artifact or hosted qualification.

Task 3 is implemented in `c9076d0f1`, with review corrections in `31ee0739` and `b89552d1`. Independent spec and quality reviews pass after correcting the SPDX 2.3 predicate identity and including executable JS/Rust validation sources in the source checks. The controller's fresh Python run passes 72 tests, the complete native CLI test command passes, and `cargo clippy-cli` passes. Source snapshots accept Git-equivalent executable modes under the selected umask variations while retaining actual permissions for before/after checks. Narrow domain-lint support for the three mandatory standards hosts is tested against the production policy and hostile lookalikes. Real-checkout inspection rejects a pre-existing ignored CLI source `.DS_Store`, which is preserved; actual build preparation requires a clean source snapshot. Tool, image, SDK and recipe digests remain explicitly pending, and `--require-qualified` cannot succeed until Task 8 supplies authenticated acquisition and measured evidence. Vite environment suppression, OS confinement, native graph coverage and consumer enforcement remain their later tasks.

Task 4 is implemented in `bfb697404`. Independent spec and quality reviews pass. The controller freshly ran all 13 source-resolution tests and all six owning-build-script/facts/resource tests; the quality reviewer independently repeated both suites. Implementation verification also passes the full native CLI test command, core template tests, CLI/core/Fastly lint, Fastly source-version tests with default and reusable-sandbox features, formatting and staged domain lint. Canonical CLI/Fastly facts derive from their owning Cargo package, target and feature settings; the public data constructor cannot reconfigure those constants and carries no verification authority. Tests compile the actual release cfg and resource-discovery function, reject the existing developer checkout fallback, prove its literal is absent, and preserve ancestor resources. Help/version/config-init portability uses the Cargo-built CLI; complete official release executable startup and cross-tag final-byte equality remain later recipe qualification evidence. No release workflow, publication or provider action is introduced by this task.

Task 5 is implemented in `e1606fa3d`. Independent spec and quality reviews pass. The controller freshly ran all 28 native actual-script/helper tests; the spec reviewer independently ran those and all 10 library tests. Implementation verification also passes WASM library tests, the full 50-file/1,211-test JS suite with type checking, Fastly/CLI target checks and lint, formatting, staged domain lint, and release-mode Cargo interface fixtures. The release fixtures use checked mock bundle bytes and a failing npm, proving prebuilt consumption and no npm execution; they do not prove official source approval or JS provenance. Checked inventories are bounded strict JSON, exact sorted identities and byte hashes; native held descriptors, identity checks and private-byte verification reject links and copy-time mutation. The exact raw inventory hash is exposed through Cargo build-script evidence for Task 10. Producers must honor the documented 32 MiB per-module input budget. Contribution/tool/material arrays remain opaque at this boundary; Task 6 supplies semantic composition evidence and Task 7 supplies independent JS builds. Source parent aliases are accepted only when the held resolved directory identity remains unchanged; Cargo-owned private output parents are an explicit trust boundary. Hosted qualification and final retained-module evidence remain pending.
