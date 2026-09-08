# PR 1079 Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every review finding on PR 1079 and produce an `rc/202608`-based staging branch containing the corrected implementation.

**Architecture:** Keep the first-claimant state machine, but make suppression token-local and correlation navigation/element-local. The first suppressed delivery closes registration while preserving every already-registered losing token until navigation or element replacement. Compose GPT/Prebid refresh wrappers explicitly, and centralize pre-response creative freshness validation plus safe authenticated-shell expansion.

**Tech Stack:** TypeScript, Vitest/jsdom, Playwright, esbuild, Rust workspace validation, Git.

---

### Task 1: First-impression token semantics

**Files:**

- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/core/first_impression.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Add failing overlap and late-token tests**

Add tests named `suppresses every publisher auction registered before the first TS delivery` and `suppresses a correlated TS-owned delivery after the five-second lease`. Assert two pre-registered callbacks are both suppressed, a later auction proceeds, and a fake-timer callback after 5 seconds remains suppressed.

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "registered before|five-second lease"`

Expected: FAIL because `suppressionConsumed` permits the second delivery and expiry deletes the late token.

- [ ] **Step 3: Implement token-local suppression**

Replace `suppressionConsumed` with a claim-level `publisherRegistrationClosed` flag. Set it on the first suppressed delivery; do not consult it when consuming tokens already registered. Retain unresolved TS-owned suppressing tokens as non-evictable tombstones while generation and exact element identity match, including across timeout and auction failure; prune publisher-owned expired tokens and remove suppressing tombstones only on navigation or element replacement.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run the Step 2 command. Expected: PASS.

- [ ] **Step 5: Commit the state-machine checkpoint**

Run: `git add crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/core/first_impression.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts && git commit -m "fix(js): make first impression suppression auction local"`

### Task 2: Prebid request and delivery correlation

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Add five failing Prebid regressions**

Add tests named `consumes late-handoff suppression when Prebid suppresses the same delivery`, `limits a global request to opts.adUnitCodes`, `forwards only unsuppressed excluded slots`, `rejects pending delivery state from a previous navigation`, and `rejects pending delivery state after physical element replacement`. Assert the next legitimate refresh survives composed wrappers; only the selected global unit is mutated/claimed/correlated; a suppressed slot is absent from the native mixed refresh; and stale records neither suppress nor directly forward the new physical slot.

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "late-handoff|opts.adUnitCodes|unsuppressed excluded|previous navigation|physical element replacement"`

Expected: FAIL on the current wrapper, scoping, forwarding, and stale-correlation behavior.

- [ ] **Step 3: Implement scoped, physical correlation**

When `opts.adUnits` is absent and `opts.adUnitCodes` is an array, filter `pbjs.adUnits` before snapshotting, mutation, claiming, and correlation. Stamp `PendingPublisherBid` and `PendingPublisherCode` with `navGeneration` and the exact resolved `HTMLElement`; accept them only if generation, element identity, connectivity, DOM lookup, and target-slot resolution still match. Retain still-current suppressing correlations as tombstones. When Prebid suppresses a slot, clear the matching `gptSlotHandoffs` one-shot flag. In the no-auction/excluded branch call native GPT with `forwardedSlots`, not the original list.

- [ ] **Step 4: Run the full Prebid test file and verify GREEN**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/prebid/index.test.ts`. Expected: PASS.

- [ ] **Step 5: Commit the Prebid checkpoint**

Run: `git add crates/trusted-server-js/lib/src/integrations/prebid/index.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts && git commit -m "fix(js): scope publisher delivery correlation"`

### Task 3: Creative freshness and nested shell repair

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`
- Test: `crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts`

- [ ] **Step 1: Add failing stale-response and nested-shell tests**

Change `does not resize a stale cache response after navigation` to assert zero port posts, zero successful-response evidence, and zero billing beacons. Add `expands every collapsed ancestor through the authenticated slot root`, with iframe -> 1x1 inner wrapper -> 1x1 outer wrapper -> authenticated root. Add/extend the browser scenario to assert all clipping ancestors have the winning dimensions.

- [ ] **Step 2: Run focused GPT tests and verify RED**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt/ad_init.test.ts -t "stale cache response|every collapsed ancestor"`

Expected: FAIL because stale cache data is posted and only the immediate parent is resized.

- [ ] **Step 3: Validate before creative side effects**

Create one helper that checks current generation, winning bid/renderer ownership, authenticated source iframe identity, connectivity, and containment. Invoke it immediately before every APS or ADM `postMessage`; return before successful-response diagnostics, `markUsed`, or billing on failure.

- [ ] **Step 4: Expand the authenticated shell safely**

Require finite positive dimensions no larger than 10,000. Require the source iframe to retain its 1x1 attributes and collapsed computed dimensions. Preflight every ancestor through the authenticated root, rejecting detached/foreign roots, `body`/`html`, fixed/sticky positioning, and anchor/vignette/interstitial markers. Then resize the iframe and each ancestor whose width or height remains collapsed; never mutate outside the authenticated root.

- [ ] **Step 5: Run GPT unit and browser tests**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt/ad_init.test.ts`

Run: `cd crates/trusted-server-integration-tests/browser && npx playwright test tests/shared/aps-renderer.spec.ts`

Expected: PASS.

- [ ] **Step 6: Commit the renderer checkpoint**

Run: `git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts crates/trusted-server-integration-tests/browser/tests/shared/aps-renderer.spec.ts && git commit -m "fix(js): reject stale creatives and expand nested shells"`

### Task 4: Full verification

- [ ] **Step 0: Commit the reviewed design and plan**

Run: `git add docs/superpowers/specs/2026-08-27-pr-1079-review-remediation-design.md docs/superpowers/plans/2026-08-27-pr-1079-review-remediation.md && git commit -m "docs: plan PR 1079 review remediation"`.

- [ ] **Step 1: Run JS gates**

Run from `crates/trusted-server-js/lib`: `npm run format && npm run lint && npx vitest run && node build-all.mjs`. Run the relevant Playwright suite with the command established in Task 3. Expected: every command exits 0.

- [ ] **Step 2: Run repository Rust gates**

Run: `cargo fmt --all -- --check`, `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`, `./scripts/test-cli.sh`, `cargo clippy-fastly`, `cargo clippy-axum`, `cargo clippy-cloudflare`, `cargo clippy-cloudflare-wasm`, `cargo clippy-spin-native`, and `cargo clippy-spin-wasm`. Expected: every command exits 0.

- [ ] **Step 3: Commit formatting or test-only adjustments**

If verification changed tracked files, review them and commit only scoped changes as `chore: finalize PR 1079 remediation verification`.

### Task 5: Build the staging branch

- [ ] **Step 1: Confirm a clean repair branch**

Run: `git status --short --branch` and record `git rev-parse HEAD`. Expected: branch `fix/gpt-first-impression-aps-shell-review`, no uncommitted changes.

- [ ] **Step 2: Refresh the remote RC ref**

Run: `git fetch origin refs/heads/rc/202608:refs/remotes/origin/rc/202608 refs/heads/fix/gpt-first-impression-aps-shell:refs/remotes/origin/fix/gpt-first-impression-aps-shell`.

- [ ] **Step 3: Create and merge the staging branch**

Run: `git switch -c staging/202608-pr1079-review origin/rc/202608` then `git merge --no-ff fix/gpt-first-impression-aps-shell-review -m "Merge PR 1079 review remediation for staging"`. Expected: merge succeeds without unresolved conflicts.

- [ ] **Step 4: Re-run critical post-merge gates**

Run: `cd crates/trusted-server-js/lib && npm run format && npm run lint && npx vitest run && node build-all.mjs`.

Run: `cd crates/trusted-server-integration-tests/browser && npx playwright test tests/shared/aps-renderer.spec.ts`.

Run from the repository root: `cargo fmt --all -- --check && cargo check-fastly && cargo check-axum && cargo check-cloudflare`.

Expected: every command exits 0 and `git status --short --branch` is clean on `staging/202608-pr1079-review`.

- [ ] **Step 5: Report deployable refs**

Record the repair-branch hash, staging merge hash, exact test results, and any non-blocking environment limitations. Do not push unless separately requested.
