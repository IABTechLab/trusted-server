# Binary attestations Phase 3: retained evidence and index foundations

**Issue:** #1235. **Date:** 2026-10-28. **Status:** Step 1 source port approved; later implementation remains gated. Draft #1463 is stacked on Phase 2. This phase-specific spec consolidates existing approvals; it introduces no new release design.

## Problem and approved slice

Phase 2 supplies ordinary Fastly equality and an authenticated rebuild-check, not complete composition or a release index. The approved Step 1 imports exactly 63 assigned frozen-source paths, plus their required shared support/assertion hunks, for SPDX, collection and distribution foundations. It restores the assertions deferred from Phase 2. Source: `122b78664b1235c717e9208fd200f9a9b0c335df`; exclusive and shared ownership follows the [slice map](../plans/binary-attestations-slice-map.json) and [phase assignment](../plans/binary-attestations-phases-2-4.md).

Every imported path is compared to frozen bytes. Necessary integration, current-main or test-split adaptations are recorded per path with their reason. Already-carried Phase 1/2 work is retained. GNU masking WIP is excluded and untouched.

## Boundaries and compatibility

`spdx::generate_from_completed` remains a closed error path. All `Qualified*` types stay uninhabited; no production/qualified selector, live index writer, signer or publisher is enabled. Candidate diagnostics retain their gaps and cannot become complete generation or publication authority. The four-entry v1 policy, schema and fixtures remain frozen.

The approved [Gate 2 design](../plans/binary-attestations-phase3-complete-spdx.md) requires retained, artifact-bound owning evidence. Complete production generation stays closed until its approved selected-cohort conditions are actually met. The [full-cohort assessment](../plans/binary-attestations-phase3-full-cohort-feasibility.md) presents costly unresolved cells and an unadopted identity alternative; its recommendation does not amend scope or authorize capture code.

The approved [Gate 3 design](../plans/binary-attestations-phase3-seven-artifact-index.md) names future v2 cohort `trusted-server-binaries`; frozen v1 keeps `phase1-fastly-cli`. This slice creates no v2 policy, cohort, schema, descriptor, admission or assembly files. Steps 2–5 wait for the final Gate 2 rollout/assurance decision under the [revised plan](../plans/binary-attestations-phase3-seven-artifact-index-implementation.md). Consumer code remains Phase 4; publication and fork trust remain Gates 4/6.

Seven release binaries, SPDX 2.3/predicate/companion names, reusable signer identity and checksum rules remain unchanged. No workflow, release script or signing changes are included. No rehearsal tag is needed for this source-only slice; a dependency that would require changing release machinery must be reported before proceeding.

## Integration and affected adapters

Register the imported private producer modules and only their required error variants, dependencies and owning-accessor/re-export support. Preserve ordinary Phase 2 commands and current toolchain/dependency adaptations. Restore the approved shared recipe/native-composition assertion hunks, preserving the Phase 2 location of `js_inventory_sha256` rather than duplicating it.

Fastly, Axum, Cloudflare, Spin and the operator CLI gain no runtime behavior, configuration, consent/privacy or deployment changes. No operator configuration or secret becomes a compile-time input. Candidate metadata is internal producer/test evidence; it is not a new release asset.

The map also assigns workflow/script files to Phase 3. They remain outside this Step 1 authorization. The frozen workflow-contract module/test unit, its command and three `workflow_contract_command_*` process tests are explicitly deferred to Gate 4 with the nine excluded workflow/script/config paths. At Gate 4 the unit must validate the actual release workflows; no frozen workflow fixture substitutes for them. Nothing in this source-only port may import or compile against the deferred unit. No dependency is silently deleted or replaced with a passing stub.

## Tests and failure handling

Add a pure regression guard for exact raw v1 policy/schema hashes and preserve imported v1 fixture bytes. Run host all-target check and strict all-target/all-feature producer clippy after coherent chunks. Audit pure test bodies before exact-name local execution, require nonzero selection and a passing/non-ignored result. Process-spawning tests remain hosted-only.

Every restored assertion must be accounted for: executed locally when pure, executed in hosted CI where supported, or retained with an explicit named ignore reason where its required fixture/provider is not available. The frozen controlled observer's esbuild 0.27.3 restriction is not adapted to the current 0.28.2 lock. Ignored collector/candidate tests are never reported as passing.

Parsing, ownership, digest, candidate and incomplete-evidence failures retain their frozen fail-closed semantics. No test-only constructor becomes a production entrypoint. Source/fixture hash guards and independent scope/quality reviews must detect accidental activation or frozen-contract changes.

## Delivery

Use separate coherent signed commits, explicit paths and a per-path deviation/test-execution ledger. Run touched formatting/domain/documentation gates and require exact-head hosted CI. Keep #1463 draft with no reviewers; do not modify #1435/#1449 or create tags/releases/settings.

## Review log

- Round 1 (2026-10-28, fresh independent AI source review): found the workflow-contract disposition was not yet explicit in the spec/assignment. Corrected it to the user-approved 63-path core scope and Gate 4 deferral of the nine workflow/script/config paths, both workflow-contract files, command and three process tests. Confirmed the frozen core has no dependency on that unit. No other blocking findings in the inspected generator/qualification, candidate, ownership/failure, adapter/configuration/privacy or v1 compatibility boundaries.

- Round 2 (2026-10-28, a second fresh independent AI source review): no blocking findings. Rechecked all 63 paths for deferred dependencies, the exact 11-path/command/three-test Gate 4 boundary, frozen v1 hashes, the closed generator, seven uninhabited qualification enums and candidate-only signing/index semantics. No tests, runtime qualification or implementation were performed by the review.
