# Binary attestations: phases 2–4

Historical separate-issue draft. On 2026-10-28, the existing parent issue #1235 was selected to track this work; publication of another tracking issue is not required.

## Summary

Track the remaining work from #1262 as three stacked draft PRs, each checked and rehearsed before the next phase begins. Part of #1235; Phase 1 is #1435. Phase 1 remains draft for its maintainer review.

The source is frozen at `122b78664b1235c717e9208fd200f9a9b0c335df`, with base `f9281917a7260b723f5f22175911c6d2147ef650`. The full [assignment and dependency evidence](binary-attestations-phases-2-4.md) records all 434 paths, carried work, estimates, staged test hunks and approval gates. Once posted, replace this relative link with the committed document link and put this issue's number in each phase PR.

## Stack and acceptance criteria

### Phase 2 — Reproducible Fastly builds

Branch: `feature/binary-attestations-phase2`; base: `feature/binary-attestations-phase1`.

- [ ] Port strict documents, policy/preflight/source, process supervision, sandbox boundaries, JS preparation, recipes and binary inspection from the recorded source.
- [ ] Include all non-SPDX composition capture modules and their recipe dependencies.
- [ ] Preserve already-landed immutable build facts; port only residual source-resource and integration-test changes.
- [ ] Obtain separate approval for the missing real equality-gate/rebuild-check integration before implementing it.
- [ ] Prove fresh canonical/reproduction Fastly builds are byte-identical and publish a real `<fastly asset>.rebuild-check.json` companion. Native reproduction stays measured, not gated.
- [ ] Rehearse the new capability and unchanged Phase 1 verification before declaring the phase ready.

### Phase 3 — Complete evidence and release index

Branch: `feature/binary-attestations-phase3`; base: Phase 2 branch.

- [ ] Port SPDX projections, collection, workflow-jobs/transfer validation and distribution/index code.
- [ ] Restore the recipe and native-composition SPDX/index/export assertions staged out of Phase 2; do not drop tests.
- [ ] Obtain approval for the complete-SPDX integration and owning composition inputs; the source generator is a closed stub.
- [ ] Draft and obtain approval for a seven-artifact index schema amendment before changing the schema or constructing the seven-artifact index.
- [ ] Obtain approval for the signed-index integration; publish only genuine signature bundles.
- [ ] Publish complete SPDX, `release-index.json`, `tsjs-inventory.json`, per-asset `.build-evidence.json`, `.build-recipe.json`, `.rebuild-check.json` and index provenance as additive real assets.
- [ ] Rehearse complete evidence and unchanged Phase 1 commands before declaring the phase ready.

### Phase 4 — ts release verify

Branch: `feature/binary-attestations-phase4`; base: Phase 3 branch.

- [ ] Port consumer verification and publication-owner code with existing tests.
- [ ] Obtain separate approval for a public offline verification interface/trust boundary; the private source helper and manual commands are not substitutes.
- [ ] Obtain approval for live publication wiring and a trusted fork-rehearsal transport/policy mechanism. Keep official production repository/signer identity strict.
- [ ] Ship public online and offline `ts release verify`, clearly distinguishing offline build verification from online publication checks.
- [ ] Replace the interim publisher only after its replacement is rehearsed, retaining release-ID mutation, refusal and recovery guarantees.
- [ ] Rehearse the public CLI and unchanged Phase 1 verification before declaring the phase ready.

## Fixed contract and qualification

Preserve the reusable `release-build.yml` signer identity, seven binary names, SPDX 2.3 predicate and companion names, signature-bundle names, `SHA256SUMS` rules, immutable build facts, runtime bind/port settings, publication/signing variable gate and stable/prerelease Latest behavior. New assets are additive.

All uninhabited `Qualified*` enums remain uninhabited; production and qualified selectors remain closed. Ordinary-CI and rehearsal evidence remains `release_qualification:false`. New triggers, maintainer settings/authority, qualification resources, and missing integration designs require separate approval.

The unpublished GNU masking WIP `b91c7450a4f617d5ec879f612b295a532c7d7efa` stays excluded and untouched. If wanted later, request approval and land its boundary/recipe coverage as a separate reviewed Phase 2 commit.

## Verification and delivery

- [ ] Record source SHAs, adaptations, actual new-file/addition counts and Rust versus workflow/script/test size in every PR.
- [ ] Keep process-spawning and ignored tests CI-only. Run selected pure tests locally with a nonzero-selection guard.
- [ ] Pass touched repository CI gates and target-matched check/clippy, formatting, documentation/domain lint, Node tests, shellcheck and actionlint as applicable.
- [ ] Complete independent and security review of boundary, transfer, signing, publication and verifier code.
- [ ] Rehearse only the exact reviewed head after upstream CI is green. Use fresh sequential lightweight tags in the fork only, never reused, deleted or retargeted; post full transcripts and pass/fail results.
- [ ] Keep PRs draft without reviewers. After lower merges, merge main into the stack and retarget the next base; no rebase or force-push.

No IAB tags, releases or settings changes. Fork settings/variables/deletions need approval. Phase 1 helpers remain until their compatible replacements are rehearsed.

## Current planning evidence

Phase 2 alone and Phase 2 plus Phase 3 evidence code pass host all-target check and strict clippy on Rust 1.98.1. This compiles macOS test code; Linux-only code and process tests still need CI. It proves dependency closure, not qualification, behavioral correctness or completion of the missing integrations.

Exclusive ledger: Phase 2 274 paths, Phase 3 74, Phase 4 54; 32 paths already carried in #1435. Frozen-source estimates: Phase 2 241 new files/79,907 text lines; Phase 3 72/20,438; Phase 4 45/14,985. Split assertions and current-main adaptations change actual per-PR sizes, which must be reported separately.
