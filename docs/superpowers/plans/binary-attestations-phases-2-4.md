# Binary attestations phases 2–4

**Status:** Assignment approved with dependency-closure corrections on 2026-10-26. Phase 2 porting may begin after the compile checks below. Capability integration and qualification remain separately gated.

**Goal:** Land the remaining reviewed source as three stacked, independently checked draft PRs while preserving the Phase 1 release contract.

## Source and stack

Frozen source: `122b78664b1235c717e9208fd200f9a9b0c335df` from #1262; source-map base: `f9281917a7260b723f5f22175911c6d2147ef650`. A fresh read of #1262 on 2026-10-26 confirmed that its published head is still the frozen source. Introducing and shaping SHAs remain recorded per group in [the slice map](binary-attestations-slice-map.json); they are provenance references, not safe whole-commit cherry-pick boundaries.

Phase 1 baseline: `a501d1ba6bde2db82546d7e16c93facab8c613e6`, docs-only since rehearsal .9 at `149754d357901ec874eab6140e92bce3d8613085`. Do not edit #1435 further.

| Draft PR                                                           | Branch                               | Base                                 | Intended capability                                      |
| ------------------------------------------------------------------ | ------------------------------------ | ------------------------------------ | -------------------------------------------------------- |
| Binary attestations: phase 2 – reproducible Fastly builds          | `feature/binary-attestations-phase2` | `feature/binary-attestations-phase1` | Fastly equality and a real rebuild-check companion       |
| Binary attestations: phase 3 – complete evidence and release index | `feature/binary-attestations-phase3` | Phase 2 branch                       | Complete SPDX, evidence companions and a signed index    |
| Binary attestations: phase 4 – ts release verify                   | `feature/binary-attestations-phase4` | Phase 3 branch                       | Public online/offline verification and publication owner |

Finish and rehearse each phase before the next. Keep PRs draft, without reviewers. When a lower phase merges, merge main into the stack and retarget the next base; never rebase or force-push. The intended capabilities above are acceptance criteria, not claims that the frozen source already implements their missing integrations.

## Assignment corrections

- Phase 2 owns every non-SPDX composition file: `composition.rs` and capture, external, graph, JavaScript, native, origin, platform, their tests and fixtures. The production recipe/composition dependency cycle closes within Phase 2.
- Phase 3 owns `spdx/**`, `collection/**`, `distribution/**`, their tests and fixtures. Shared SPDX schema/license and qualification-results schema land in Phase 2 because strict document validation embeds them; no SPDX generator is enabled by carrying schema data.
- The neutral `js_inventory_sha256` test helper moves from `recipe/tests/candidate_entry.rs` to `recipe/tests/candidate.rs` in Phase 2. The three recipe runners import it there. It needs only retained JS contributions.
- `recipe/tests/candidate_entry.rs`, `candidate_index.rs` and `spdx_candidate.rs` land in Phase 3. `candidate.rs` retains fixture parsing, measurements and recipe support in Phase 2; its export-after-cleanup wrappers, `RuntimeAcceptance` and export assertions land in Phase 3.
- In `recipe/tests/complete.rs`, Phase 2 keeps build/composition/JS rejection tests; Phase 3 adds the SPDX-generation rejection test and the collection-export assertion. `full.rs`, `cli.rs` and `macos_cli.rs` retain build, equality or native measurement, inspection, remapping and runtime/startup checks in Phase 2; Phase 3 restores SPDX/index/export assertions. No test is dropped from the final stack.
- Four `native_spdx_*` tests in `composition/native/tests.rs` land in Phase 3. Capture tests and `fixture_for_projection` remain in Phase 2. `recipe/startup/accepted/tests.rs` contains no evidence-module imports and stays wholly in Phase 2.
- `crates/trusted-server-cli/tests/fixtures/release/release-index.json` moves to Phase 3 because distribution tests embed it. CLI consumer code remains Phase 4.
- Main command/module registrations, error variants, re-exports, manifests, lockfile, workflows and shared tests are staged by dependency. Stage unused evidence/publication variants until their owning phase rather than introducing broad warning suppressions.

### Avoid duplicate build-facts ports

Ten old slice 2.3 paths are already landed in #1435: Fastly and CLI build scripts; core build script, Git resolver, build-info module, library registration and resolver test; Fastly main; CLI build-facts test; core constants. Preserve the current logging and documentation refinements in the last three. CLI `lib.rs` and `run.rs` also already carry the build-info re-export and labelled version output; Phase 4 ports only their remaining command/module hunks.

The six residual slice 2.3 paths are CLI resource discovery, four integration-test/helper paths and the historical Git-version design note. These remain Phase 2. The source-identical release-tools and CLI policy documents are retained, not copied again. Do not overwrite the current seven-artifact cohort or domain allowlist with frozen four-artifact versions.

## Dependency closure evidence

Scratch directories were assembled outside the checkout, without worktrees or branch changes to the frozen source. Inputs were the Phase 1 baseline above, main at `7aa5c993b` (its two additional changes update JS package versions/lock), and the assigned source files. The producer crate is registered in the current workspace manifest; dependencies are resolved with the current workspace versions. Rust 1.98.1 is explicit because this checkout has an older local override.

| Scratch tree                                                    | Host check | Strict clippy |
| --------------------------------------------------------------- | ---------- | ------------- |
| Phase 2 alone                                                   | Pass       | Pass          |
| Phase 2 + Phase 3 evidence modules and restored test assertions | Pass       | Pass          |

Commands for each tree:

```sh
cargo +1.98.1 check -p trusted-server-release --all-targets --target aarch64-apple-darwin --locked --offline
cargo +1.98.1 clippy -p trusted-server-release --all-targets --all-features --target aarch64-apple-darwin --locked --offline -- -D warnings
```

Initial Phase 2 check had no unresolved imports after splitting the evidence tests. Strict clippy exposed unused destination-only re-exports, error variants and one macOS evidence-measurement parameter; these were staged or removed from the Phase 2 hunk. Phase 3 initially failed on its embedded CLI release-index fixture; assigning that prerequisite to Phase 3 fixed it. Final checks and clippy exited zero without warnings for both trees.

This proves host compilation and lint closure, including macOS test compilation. It does not execute tests, cover Linux-only cfg branches, prove seven-artifact behavior, qualify tools or prove a release capability. No process-spawning or ignored tests were run locally. Linux compilation and process tests belong in CI. The original frozen four-artifact fixtures/schema remain historical candidate data; seven-artifact index construction waits for the separate schema approval.

## Counts and estimates

The slice map has 434 unique paths, exactly matching its base-to-source Git diff. This ledger assigns every path once: 22 carried documentation/evidence paths, 10 already-landed build-facts paths, and 402 remaining assigned paths. Two Phase 2 policy/tool paths are already source-identical and require no port. Cross-phase test hunks are tracked above and do not double-count paths.

| Phase | Assigned paths | New files | New source text lines | Existing paths | Existing source additions / removals, upper bound |
| ----- | -------------: | --------: | --------------------: | -------------: | ------------------------------------------------: |
| 2     |            274 |       241 |                79,907 |             33 |                                     +1,443 / −559 |
| 3     |             74 |        72 |                20,438 |              2 |                                      +1,189 / −14 |
| 4     |             54 |        45 |                14,985 |              9 |                                         +675 / −2 |

| Phase | Rust new lines / existing additions | Workflow, script and non-Rust test new lines / existing additions |
| ----- | ----------------------------------: | ----------------------------------------------------------------: |
| 2     |                        72,530 / 284 |                                                       4,089 / 534 |
| 3     |                          18,457 / 0 |                                                     1,415 / 1,189 |
| 4     |                        14,811 / 330 |                                                             0 / 0 |

These are frozen-source estimates, not promised final diff sizes: new-file totals include mixed files before their assertion split; Rust includes Rust tests; tooling excludes Rust tests; binary fixtures have no text-line count. Existing changes use original base-to-source hunks excluding source-identical paths. Current-main adaptations, later integration designs and restored assertion hunks can change per-phase totals. Report actual counts in each PR. Counts exclude this new plan and the tracking-issue draft.

## Separate design-approval gates

Assignment approval authorizes the mapped port and dependency/test staging, not the following missing capabilities:

1. **Real Fastly release integration.** Frozen `main.rs` has no build/equality/export command; bootstrap remains fail-closed; qualified Rust/JS tool enums remain uninhabited. Ordinary-CI equality exists in ignored tests, not a production release adapter. Design approval is required before implementing the release gate and real rebuild-check writer. Keep native reproduction measured, not gated.
2. **Complete SPDX.** `spdx::generate_from_completed` is a closed stub. Candidate projections cannot be presented as a complete SBOM. Owning complete composition inputs require an approved design.
3. **Seven-artifact index amendment.** The carried v1 schema fixes four artifacts. Draft a reviewed v1.1/v2 amendment early in Phase 3 and obtain approval before schema changes or seven-artifact index construction. No placeholder signature bundles.
4. **Signed index and live publisher.** The frozen signed-companion types are closed; publication has an abstract API and fake tests but no live adapter. Approval precedes implementation. Preserve Phase 1 identities, names, predicates, release-ID mutation and recovery behavior.
5. **Public offline verification.** Frozen offline verification is internal, build-only and expects caller-enforced network denial. A public offline interface/trust boundary needs approval; manual commands or the private helper do not substitute for the requested capability. Offline build evidence does not prove current publication.
6. **Fork verifier trust.** Frozen consumer policy pins official repository IDs and signer URLs; online transport constructs official URLs. A trusted rehearsal policy/transport mechanism needs approval. Never infer authority from downloaded index contents or weaken production policy to accept the fork.

No new trigger, production/qualified selection or maintainer authority is authorized. All `Qualified*` enums stay uninhabited. Ordinary-CI/rehearsal evidence retains `release_qualification:false`. Existing Phase 1 workflows/helpers remain until compatible replacements are rehearsed. No IAB tags, releases or settings changes.

The local unpublished GNU masking commit `b91c7450a4f617d5ec879f612b295a532c7d7efa` is excluded and untouched. If separately approved later, its boundary/recipe coverage belongs in Phase 2 as its own reviewed commit. Its three new unmapped paths are `boundary/gnu_masks.rs`, `recipe/gnu/helper_masks.rs`, and `recipe/gnu/helper_masks/tests.rs` under the producer crate. No historical WIP test claim is reused as current verification.

## Execution checkpoints

- [x] Re-read source/map, independently investigate capture dependencies and carried build facts.
- [x] Assemble Phase 2 and Phase 3 scratch trees; run host check and strict clippy; correct prerequisite ownership.
- [ ] Commit this assignment and [tracking-issue draft](binary-attestations-phases-2-4-tracking-issue.md) on Phase 2.
- [ ] Port source by module, preserving Phase 1 hunks and recording each current-main adaptation; update per-path landing metadata in the slice map.
- [ ] Adapt closed producer tables to the seven Phase 1 assets and current pins without opening qualifications or changing the index schema.
- [ ] Run explicitly selected pure tests with a nonzero-selection guard; compile platform-specific branches in CI; keep process/ignored tests CI-only.
- [ ] Obtain independent and security reviews; pass touched CI gates before pushing and rehearse only an exact green reviewed head.
- [ ] Request the separate Fastly integration design approval before implementing its missing gate/writer. Do not claim the intended Phase 2 capability is shipped by porting closed code.

## Full path ledger

The landing phase below is exclusive file ownership. Shared files receive later integration hunks as listed above. Phase 1 rows are retained, not re-ported. Landing PRs for phases 2–4 are not created yet; their exact branch/base and intended titles are recorded above.

| Frozen-source path                                                                                             | Landing phase | State                                                     |
| -------------------------------------------------------------------------------------------------------------- | ------------- | --------------------------------------------------------- |
| `docs/superpowers/plans/2026-10-13-attested-release-implementation.md`                                         | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/plans/2026-10-13-cli-packaging-attestation-integration.md`                                   | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/plans/2026-10-13-fastly-prebuilt-edgezero-integration.md`                                    | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/specs/2026-01-15-attestation-design.md`                                                      | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md`                                         | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/specs/2026-10-12-1235-release-attestation-feasibility.md`                                    | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/specs/2026-10-14-release-tooling-rust-migration.md`                                          | 1 — carried   | Carried in #1435                                          |
| `docs/superpowers/specs/schemas/release-index-v1.schema.json`                                                  | 1 — carried   | Carried in #1435                                          |
| `scripts/release/js-composition.md`                                                                            | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-15-consumer-real-fixtures.md`                                           | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-15-js-two-builds.md`                                                    | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-16-confined-cargo-candidate.md`                                         | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-16-fastly-canonical-reproduction-equality.md`                           | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-16-fastly-runtime-gate.md`                                              | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-16-linux-cli-candidate.md`                                              | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-16-producer-normalized-manifests.md`                                    | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-19-apple-sdk-inputs.md`                                                 | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-20-apple-cli-candidates.md`                                             | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-20-linux-cli-gnu-follow-up.md`                                          | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-21-candidate-release-index.md`                                          | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-21-four-asset-candidate-join.md`                                        | 1 — carried   | Carried in #1435                                          |
| `scripts/release/qualification/2026-10-21-native-reproduction-schedule.md`                                     | 1 — carried   | Carried in #1435                                          |
| `Cargo.lock`                                                                                                   | 2             | Pending                                                   |
| `Cargo.toml`                                                                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/Cargo.toml`                                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/documents.rs`                                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/documents/tests.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/error.rs`                                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/git_objects.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/main.rs`                                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/policy.rs`                                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/policy/tests.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/policy/tools.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preflight.rs`                                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preflight/child.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preflight/tests.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source.rs`                                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/closure.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/config.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/git.rs`                                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/materialize.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests/closure.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests/layout.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests/objects.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests/process.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tests/worktree.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/tree.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/source/worktree.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/tool_tree.rs`                                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/tool_tree/tests.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/commands.rs`                                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/sha-mbles-1.bin`                                                 | 2             | Pending                                                   |
| `scripts/release/bootstrap.sh`                                                                                 | 2             | Pending                                                   |
| `scripts/release/release-cohort.json`                                                                          | 2             | Pending                                                   |
| `scripts/release/release-tools.json`                                                                           | 2             | Already identical; retain, no port                        |
| `crates/trusted-server-release/src/boundary.rs`                                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/cleanup_and_system_root_tests.rs`                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/command.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/command/selector_tests.rs`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/daemon.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/docker.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/docker/host_path_tests.rs`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/entropy.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/host_inputs.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/macos.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/probe.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/proof.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/roots.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/seccomp.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/selector_placement_tests.rs`                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/sibling_tests.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/survivors.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests/container.rs`                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests/linux.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests/macos.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests/modes.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/boundary/tests/unit.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/lifecycle_guard.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process.rs`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/environment.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/environment/selector_tests.rs`                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/pipes.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/redact.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/supervise.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests/fixture.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests/outcomes.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests/redaction.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests/supervision.rs`                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/process/tests/variables.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/boundary.rs`                                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/Cargo.lock`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/Cargo.toml`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-direct/Cargo.toml`                     | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-direct/build.rs`                       | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-direct/src/main.rs`                    | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-grandchild/Cargo.toml`                 | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-grandchild/build.rs`                   | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/build-grandchild/src/main.rs`                | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/control/Cargo.toml`                          | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/control/build.rs`                            | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/control/src/control.rs`                      | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/control/src/main.rs`                         | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/macro-attempt/Cargo.toml`                    | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/macro-attempt/src/lib.rs`                    | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/cargo-denial/crates/macro-attempt/src/main.rs`                   | 2             | Pending                                                   |
| `scripts/release/qualification/SECCOMP-LICENSE`                                                                | 2             | Pending                                                   |
| `scripts/release/qualification/seccomp-default.json`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-adapter-fastly/build.rs`                                                                | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-adapter-fastly/src/main.rs`                                                             | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-cli/build.rs`                                                                           | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-cli/src/prebid_bundle.rs`                                                               | 2             | Pending                                                   |
| `crates/trusted-server-cli/tests/release_build_facts.rs`                                                       | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/build.rs`                                                                          | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/build_support/git_version.rs`                                                      | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/src/build_info.rs`                                                                 | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/src/constants.rs`                                                                  | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/src/lib.rs`                                                                        | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-core/tests/git_version_resolve.rs`                                                      | 1 — carried   | Build facts landed in #1435; preserve current refinements |
| `crates/trusted-server-integration-tests/browser/helpers/infra.ts`                                             | 2             | Pending                                                   |
| `crates/trusted-server-integration-tests/tests/common/runtime.rs`                                              | 2             | Pending                                                   |
| `crates/trusted-server-integration-tests/tests/frameworks/mod.rs`                                              | 2             | Pending                                                   |
| `crates/trusted-server-integration-tests/tests/parity.rs`                                                      | 2             | Pending                                                   |
| `docs/superpowers/specs/2026-09-25-git-version-header-design.md`                                               | 2             | Pending                                                   |
| `crates/trusted-server-js/Cargo.toml`                                                                          | 2             | Pending                                                   |
| `crates/trusted-server-js/README.md`                                                                           | 2             | Pending                                                   |
| `crates/trusted-server-js/build.rs`                                                                            | 2             | Pending                                                   |
| `crates/trusted-server-js/build/bundle_set.rs`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-js/build/inventory.rs`                                                                  | 2             | Pending                                                   |
| `crates/trusted-server-js/build/prebuilt.rs`                                                                   | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/.gitignore`                                                                      | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/.prettierignore`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/build-all.mjs`                                                                   | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/build-module-options.mjs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/eslint.config.js`                                                                | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/package.json`                                                                    | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/release-composition.mjs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/test/observe-npm.test.mjs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/test/release-composition.test.mjs`                                               | 2             | Pending                                                   |
| `crates/trusted-server-js/lib/vitest.config.ts`                                                                | 2             | Pending                                                   |
| `crates/trusted-server-js/tests/release_prebuilt.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/javascript.rs`                                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/javascript/contributions.rs`                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/javascript/contributions/tests.rs`                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/javascript/tests.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/javascript/tests/adopt.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials.rs`                                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/archive.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/differential.rs`                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/git_tree.rs`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/index.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/normalize.rs`                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/source.rs`                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests/git.rs`                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests/normalize.rs`                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests/registry.rs`                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests/source.rs`                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/tests/support.rs`                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/cargo_vendor/writer.rs`                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/extract.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/extract/compatibility.rs`                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/extract/tests.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/extract/tests/cargo_package.rs`                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/extract/tests/npm_compatibility.rs`                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/git.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/git/input.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/git/tests.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/locked.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/manifest.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/npm_bundle.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/materials/tests.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preparation.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preparation/tests.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/preparation/tests/full.rs`                                                  | 2             | Pending                                                   |
| `scripts/release/build-js.sh`                                                                                  | 2             | Pending                                                   |
| `scripts/release/collect-js.mjs`                                                                               | 2             | Pending                                                   |
| `scripts/release/observe-npm.mjs`                                                                              | 2             | Pending                                                   |
| `scripts/release/tests/js-fixture.Dockerfile`                                                                  | 2             | Pending                                                   |
| `scripts/release/tests/prepare-js-fixture.mjs`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-adapter-cloudflare/build.sh`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect.rs`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/elf.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/elf/loader.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/macho.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/macho/signature.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests/builders.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests/builders/macho_data.rs`                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests/elf.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests/macho.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/tests/wasm.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/inspect/wasm.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/package.rs`                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/package/tests.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe.rs`                                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/ambient.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/apple.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/apple/sdk.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/apple/toolchain.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/apple/toolchain/tests.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/artifact.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/artifact/tests.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/cargo_home.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/cargo_home/tests.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/cargo_output.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/cargo_output/tests.rs`                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/compare.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/completed.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/execute.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/fastly_tools.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/final_paths.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/fresh.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/gnu.rs`                                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/gnu/image.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/gnu/tests.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/identity.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/identity/tests.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/native.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/paths.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/paths/fake.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/plan.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/remap.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/config.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/exchange.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/export.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/export/tests.rs`                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/logs.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/runtime/viceroy.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/sources.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/startup.rs`                                                          | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/startup/accepted.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/startup/accepted/tests.rs`                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/startup/lifecycle.rs`                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/startup/native.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests.rs`                                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/ambient.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/bind.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/candidate.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/candidate_entry.rs`                                            | 3             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/candidate_index.rs`                                            | 3             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/cli.rs`                                                        | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/cli_candidate.rs`                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/compare.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/complete.rs`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/complete_darwin.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/darwin.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/darwin_toolchain.rs`                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/fixture.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/full.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/full_macos.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/macos_candidate.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/macos_cli.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/native.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/plan.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/real_fixture.rs`                                               | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/remap.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/runtime.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/sources.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/spdx_candidate.rs`                                             | 3             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tests/tools.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/recipe/tools.rs`                                                            | 2             | Pending                                                   |
| `scripts/release/build.sh`                                                                                     | 2             | Pending                                                   |
| `scripts/release/tests/cargo-fixture.Dockerfile`                                                               | 2             | Pending                                                   |
| `scripts/release/tests/cli-builder.Dockerfile`                                                                 | 2             | Pending                                                   |
| `scripts/release/tests/prepare-cargo-fixture.mjs`                                                              | 2             | Pending                                                   |
| `scripts/release/tests/prepare-cli-fixture.mjs`                                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/schemas/SPDX-LICENSE`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/schemas/spdx-2.3.schema.json`                                                   | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition.rs`                                                             | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/capture.rs`                                                     | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/external.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/external/tests.rs`                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/graph.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/graph/tests.rs`                                                 | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/javascript.rs`                                                  | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/javascript/tests.rs`                                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/native.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/native/tests.rs`                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/origin.rs`                                                      | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/origin/tests.rs`                                                | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/platform.rs`                                                    | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/platform/tests.rs`                                              | 2             | Pending                                                   |
| `crates/trusted-server-release/src/composition/tests.rs`                                                       | 2             | Pending                                                   |
| `crates/trusted-server-release/src/spdx.rs`                                                                    | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/candidate.rs`                                                          | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/candidate/tests.rs`                                                    | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/cargo.rs`                                                              | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/cargo/tests.rs`                                                        | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/javascript.rs`                                                         | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/javascript/tests.rs`                                                   | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/native.rs`                                                             | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/native/tests.rs`                                                       | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/platform.rs`                                                           | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/platform/tests.rs`                                                     | 3             | Pending                                                   |
| `crates/trusted-server-release/src/spdx/tests.rs`                                                              | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/README.md`                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-cross-host-metadata.json`                 | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-cross-messages.jsonl`                     | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-cross-target-filtered-metadata.json`      | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-cross-tree.txt`                           | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-messages.jsonl`                           | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/composition/cargo-1.95-metadata.json`                            | 2             | Pending                                                   |
| `crates/trusted-server-release/src/collection.rs`                                                              | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/archive.rs`                                                      | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/producer.rs`                                                     | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/producer/strict_record.rs`                                       | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/producer/strict_record/tests.rs`                                 | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/records.rs`                                                      | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/records/receipt.rs`                                              | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/tests.rs`                                                        | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/transfer.rs`                                                     | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/transfer/ordinary.rs`                                            | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/transfer/tests.rs`                                               | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs.rs`                                                | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests.rs`                                          | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests/support.rs`                                  | 3             | Pending                                                   |
| `crates/trusted-server-release/src/collection/workflow_jobs/tests/transfer.rs`                                 | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution.rs`                                                            | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/entry.rs`                                                      | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/joins.rs`                                                      | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/ordinary.rs`                                                   | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/summary.rs`                                                    | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/tests.rs`                                                      | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/tests/golden.rs`                                               | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/tests/summary.rs`                                              | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/tests/transfer.rs`                                             | 3             | Pending                                                   |
| `crates/trusted-server-release/src/distribution/transfer.rs`                                                   | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/README.md`                                            | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/build-record.json`                                    | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/collection/composition.json`                                     | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/candidate-release-index.json`                    | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-evidence.json`  | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.build-recipe.json`    | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.rebuild-check.json`   | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/trusted-server-fastly.wasm.spdx.json`            | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-evidence.json`     | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.build-recipe.json`       | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.rebuild-check.json`      | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-aarch64-apple-darwin.spdx.json`               | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-evidence.json`      | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.build-recipe.json`        | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.rebuild-check.json`       | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-apple-darwin.spdx.json`                | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-evidence.json` | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.build-recipe.json`   | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.rebuild-check.json`  | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/ts-x86_64-unknown-linux-gnu.spdx.json`           | 3             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/index/candidate/tsjs-inventory.json`                             | 3             | Pending                                                   |
| `crates/trusted-server-release/src/publication.rs`                                                             | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/api.rs`                                                         | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/incident.rs`                                                    | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/model.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/owner.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/role.rs`                                                        | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests/drafts.rs`                                                | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests/fake.rs`                                                  | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests/final_state.rs`                                           | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests/inputs.rs`                                                | 4             | Pending                                                   |
| `crates/trusted-server-release/src/publication/tests/reconciliation.rs`                                        | 4             | Pending                                                   |
| `.github/actions/setup-integration-test-env/action.yml`                                                        | 3             | Pending                                                   |
| `.github/workflows/release-validation.mjs`                                                                     | 3             | Pending                                                   |
| `.github/workflows/release-validation.test.mjs`                                                                | 3             | Pending                                                   |
| `.github/workflows/release-validation.yml`                                                                     | 3             | Pending                                                   |
| `.github/workflows/test.yml`                                                                                   | 3             | Pending                                                   |
| `crates/trusted-server-release/src/workflow_contract.rs`                                                       | 3             | Pending                                                   |
| `crates/trusted-server-release/src/workflow_contract/tests.rs`                                                 | 3             | Pending                                                   |
| `scripts/release/qualification/approvals.json`                                                                 | 3             | Pending                                                   |
| `scripts/release/qualification/hosted-workflow.proposal.yml`                                                   | 3             | Pending                                                   |
| `scripts/release/qualification/results.schema.json`                                                            | 2             | Pending                                                   |
| `scripts/release/tests/candidate-changes.sh`                                                                   | 3             | Pending                                                   |
| `scripts/release/tests/candidate-changes.test.mjs`                                                             | 3             | Pending                                                   |
| `crates/trusted-server-cli/Cargo.toml`                                                                         | 4             | Pending                                                   |
| `crates/trusted-server-cli/README.md`                                                                          | 4             | Pending                                                   |
| `crates/trusted-server-cli/release-policy.json`                                                                | 2             | Already identical; retain, no port                        |
| `crates/trusted-server-cli/src/commands/dev/lint/domains.rs`                                                   | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/commands/mod.rs`                                                                | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/commands/release.rs`                                                            | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/lib.rs`                                                                         | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/artifact.rs`                                                            | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/bounds.rs`                                                              | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/documents.rs`                                                           | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/environment.rs`                                                         | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/error.rs`                                                               | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/evidence.rs`                                                            | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/evidence/candidate_fixture_tests.rs`                                    | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/evidence/joins.rs`                                                      | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/evidence/upstream_format_tests.rs`                                      | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/gh.rs`                                                                  | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/gh/input.rs`                                                            | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/github.rs`                                                              | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/mod.rs`                                                                 | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/offline.rs`                                                             | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/online.rs`                                                              | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/policy.rs`                                                              | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/process.rs`                                                             | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/source_policy.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/documents/mod.rs`                                                 | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/documents/producer.rs`                                            | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/gh/argv.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/gh/environment.rs`                                                | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/gh/lifecycle.rs`                                                  | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/gh/mod.rs`                                                        | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/mod.rs`                                                           | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/offline.rs`                                                       | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/online.rs`                                                        | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/verification/fixture.rs`                                          | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/verification/mod.rs`                                              | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/release/tests/verification/publication.rs`                                      | 4             | Pending                                                   |
| `crates/trusted-server-cli/src/run.rs`                                                                         | 4             | Pending                                                   |
| `crates/trusted-server-cli/tests/fixtures/release/release-index.json`                                          | 3             | Pending                                                   |
| `crates/trusted-server-cli/tests/fixtures/release/upstream-formats.json`                                       | 4             | Pending                                                   |
| `crates/trusted-server-cli/tests/release_cli.rs`                                                               | 4             | Pending                                                   |
| `.github/workflows/format.yml`                                                                                 | 2             | Pending                                                   |
| `CHANGELOG.md`                                                                                                 | 2             | Pending                                                   |
| `crates/trusted-server-adapter-spin/src/app.rs`                                                                | 2             | Pending                                                   |
| `crates/trusted-server-adapter-spin/src/middleware.rs`                                                         | 2             | Pending                                                   |
| `crates/trusted-server-adapter-spin/tests/routes.rs`                                                           | 2             | Pending                                                   |
| `crates/trusted-server-release/tests/fixtures/README.md`                                                       | 3             | Pending                                                   |
| `docs/.vitepress/config.mts`                                                                                   | 4             | Pending                                                   |
| `docs/guide/cli.md`                                                                                            | 4             | Pending                                                   |
| `docs/guide/edgezero.md`                                                                                       | 2             | Pending                                                   |
| `docs/guide/error-reference.md`                                                                                | 2             | Pending                                                   |
| `docs/guide/getting-started.md`                                                                                | 2             | Pending                                                   |
| `docs/guide/release-verification.md`                                                                           | 4             | Pending                                                   |
| `scripts/docs-proposal/hunks.mjs`                                                                              | 2             | Pending                                                   |
| `scripts/docs-proposal/hunks.test.mjs`                                                                         | 2             | Pending                                                   |
| `scripts/release/qualification/README.md`                                                                      | 2             | Pending                                                   |
| `scripts/release/tests/fixtures/README.md`                                                                     | 2             | Pending                                                   |
