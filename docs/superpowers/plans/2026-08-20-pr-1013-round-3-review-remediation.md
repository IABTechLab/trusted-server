# PR #1013 Round-3 Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every actionable round-2 and round-3 PR #1013 review finding and return the branch with complete local verification and a rerun browser-integration check.

**Architecture:** Preserve the existing publisher, Fastly terminal-hook, template-cache, and GPT scheduler boundaries. Complete the privacy invariant through a shared response marker helper, preserve GPT one-shot state on the shared `tsjs` object, explicitly discharge failed Fastly reservations, and make the remaining test/comment/doc changes locally without broad refactoring.

**Tech Stack:** Rust 2024, `error-stack`, Fastly Core Cache/Viceroy, TypeScript, Vitest/jsdom, Cargo target aliases, GitHub Actions/CLI.

---

## File Map

- `crates/trusted-server-core/src/response_privacy.rs`: own generic terminal-private stamping and keep the synthesized-HTML wrapper.
- `crates/trusted-server-core/src/publisher.rs`: apply terminal-private marking to page-bids/invalid-304 responses; add page-bids and ESI coverage; repair cache terminology comments.
- `crates/trusted-server-adapter-fastly/src/main.rs`: prove late filter effects cannot weaken a page-bids response carrying the marker.
- `crates/trusted-server-adapter-fastly/src/template_cache.rs`: cancel invalid reservations, preserve error context, test released obligations, and correct partial-write documentation.
- `crates/trusted-server-core/src/platform/template_cache.rs`: complete CR/LF rejection coverage.
- `crates/trusted-server-core/src/html_processor.rs`: clarify CSP nonce safety net and collision assertion text.
- `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`: use shared document-level latch state.
- `crates/trusted-server-js/lib/src/core/types.ts`: type and document the internal latch.
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`: preserve the latch across fallback-to-bundle scheduler replacement.
- `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`: mirror slot semantics and cover bootstrap state.
- `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`: cover the real bootstrap-to-bundle handoff.
- `docs/guide/configuration.md`: qualify request-side `max-age` bypass wording.

### Task 1: Complete terminal-private page-bids coverage

**Files:**

- Modify: `crates/trusted-server-core/src/response_privacy.rs:92`
- Modify: `crates/trusted-server-core/src/publisher.rs:4413,6014,6028,6365,17438`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:580`

- [ ] **Step 1: Write failing marker tests for page-bids response paths**

In `publisher.rs`, add assertions using:

```rust
assert!(
    response
        .extensions()
        .get::<crate::response_privacy::TerminalPrivateResponse>()
        .is_some(),
    "page-bids response should remain terminal-private after late response effects"
);
```

Cover `page_bids_preflight_denied`, `page_bids_unknown_format`, and the successful JSON response returned by `run_page_bids_response`. Extend the invalid-origin-304 test to require the same marker.

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test-fastly page_bids -- --nocapture
cargo test-fastly eligible_navigation_rejects_unexpected_origin_304 -- --nocapture
```

Expected: the new marker assertions fail because these paths stamp only the header.

- [ ] **Step 3: Add a generic terminal-private helper**

In `response_privacy.rs`, add:

```rust
pub(crate) fn enforce_terminal_private_cache_privacy(response: &mut Response) {
    enforce_private_no_store(response);
    response.extensions_mut().insert(TerminalPrivateResponse);
}
```

Change `enforce_synthesized_html_cache_privacy` to delegate to it. Keep both functions `pub(crate)` so no public API is added.

- [ ] **Step 4: Route all affected response paths through the helper**

Replace direct `Cache-Control: private, no-store` insertion for preflight denial, unknown format, successful page-bids JSON, and the invalid-origin-304 rebuild with `enforce_terminal_private_cache_privacy(&mut response)`. Preserve status, content type, body, and deprecated-alias headers.

- [ ] **Step 5: Add the Fastly late-effects regression test**

Construct a real page-bids denial response through `trusted_server_core::publisher::page_bids_preflight_denied`, apply `RequestFilterEffects` that sets public `Cache-Control` and CDN cache headers, then call `apply_terminal_response_effects`. Assert the terminal result is exactly `private, no-store`, has no validators/CDN cache headers, and retains the marker-driven behavior. Together with the core successful-JSON marker test, this pins the per-user JSON path without exporting test-only constructors.

- [ ] **Step 6: Run focused and adapter tests**

Run:

```bash
cargo test-fastly page_bids -- --nocapture
cargo test-fastly late_filter_effects_cannot_make -- --nocapture
cargo test-fastly eligible_navigation_rejects_unexpected_origin_304 -- --nocapture
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/response_privacy.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Keep page bids responses terminal private"
```

### Task 2: Preserve the GPT scheduler latch across handoff

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js:82`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts:430`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:648`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts:122`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts:150`

- [ ] **Step 1: Mirror the missing bootstrap slot-contract tests**

Add tests equivalent to the bundle suite:

```typescript
it('fallback scheduler preserves head-injected slots when initialSlots is omitted', () => {
  // Seed ts.adSlots, call scheduleInitialAdInit(bids), assert the same slots remain.
})

it('fallback scheduler replaces existing slots when initialSlots is explicitly empty', () => {
  // Seed stale slots, call scheduleInitialAdInit({}, []), assert [].
})
```

- [ ] **Step 2: Write a failing bootstrap-to-bundle handoff test**

In `schedule_initial_ad_init.test.ts`, evaluate the verbatim bootstrap source, call its scheduler once, import the GPT module so it replaces the scheduler, then call the bundle scheduler with different bids/slots. Assert the first payload remains and only one load/double-rAF chain invokes `adInit`.

- [ ] **Step 3: Run the focused Vitest files and verify the handoff test fails**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts test/integrations/gpt/schedule_initial_ad_init.test.ts
```

Expected: bootstrap slot tests pass against current behavior; handoff test fails because importing the bundle creates a fresh closure latch.

- [ ] **Step 4: Type the shared internal state**

Add to `TsjsApi` near `navGeneration`:

```typescript
/** Internal one-shot state shared by bootstrap and bundle scheduler installs. */
initialAdInitScheduled?: boolean;
```

Use the existing internal-field naming convention; do not expose a new callable API.

- [ ] **Step 5: Replace both closure latches with shared state**

In bootstrap JavaScript:

```javascript
if ((ts.navGeneration || 0) !== 0 || ts.initialAdInitScheduled) return
ts.initialAdInitScheduled = true
```

In the TypeScript bundle:

```typescript
if ((ts.navGeneration ?? 0) !== 0 || ts.initialAdInitScheduled) return
ts.initialAdInitScheduled = true
```

Update durable comments to say the state is one-shot per document and survives fallback-to-bundle scheduler replacement.

- [ ] **Step 6: Run JS tests, build, and formatting verification**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts test/integrations/gpt/schedule_initial_ad_init.test.ts
npx vitest run
node build-all.mjs
npm run format
```

Expected: all pass, generated bundles build successfully, and Prettier reports every JS/TS file already formatted.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/integrations/gpt_bootstrap.js crates/trusted-server-js/lib/src/core/types.ts crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts
git commit -m "Preserve initial ad scheduler latch across handoff"
```

### Task 3: Explicitly cancel invalid Fastly reservations

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs:90,242,355`

- [ ] **Step 1: Write failing obligation-release tests**

For separate cold keys, obtain `TemplateCacheLookup::Reserved`, then:

1. call `insert` with mismatched `body_len`;
2. call `insert` with metadata whose `content_type` contains `\n`.

Assert each error contains the original validation reason. Immediately call `lookup_or_reserve` for the same key and require `Reserved`, proving the first transaction was canceled rather than left pending.

- [ ] **Step 2: Write an error-composition unit test**

Extract a private generic result mapper that accepts the validation error and a cancellation result. With an injected `Err("simulated cancellation failure")`, assert the returned `TemplateCacheError` text contains both the original validation reason and simulated cancellation failure.

- [ ] **Step 3: Run Fastly cache tests and verify failure**

Run:

```bash
cargo test-fastly template_cache -- --nocapture
```

Expected: re-reservation tests fail or time out under the current implicit-drop behavior; the mapper test fails to compile until implemented.

- [ ] **Step 4: Implement cancellation with preserved context**

Create the validation error first, call `self.transaction.cancel_insert_or_update()`, and pass both values through the private mapper:

```rust
fn invalid_reservation_result<E: core::fmt::Debug>(
    validation_error: TemplateCacheError,
    cancellation: Result<(), E>,
) -> Result<(), TemplateCacheError> {
    match cancellation {
        Ok(()) => Err(validation_error),
        Err(error) => Err(backend_error(format!(
            "{validation_error}; cancelling invalid cache reservation also failed: {error:?}"
        ))),
    }
}
```

Use it on both pre-insert validation branches. Do not alter the transaction after `insert` consumes it.

- [ ] **Step 5: Correct the direct-write partial-entry comment**

State that `finish()` is deliberately skipped and readers reject partial content through fallible reads and the post-read check against declared `body_len`; do not claim the entry has no known length.

- [ ] **Step 6: Run Fastly tests**

Run:

```bash
cargo test-fastly template_cache -- --nocapture
cargo test-fastly
```

Expected: all pass without blocked transaction lookups.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/template_cache.rs
git commit -m "Cancel invalid template cache reservations"
```

### Task 4: Close focused coverage and documentation gaps

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs:1102`
- Modify: `crates/trusted-server-core/src/html_processor.rs:725,2151`
- Modify: `crates/trusted-server-core/src/publisher.rs:1656,2078,5492,8225,8739,9246`
- Modify: `docs/guide/configuration.md:1430`

- [ ] **Step 1: Add the missing metadata cases**

Extend `metadata_encoding_rejects_line_break_injection` with a policy-header name containing `\r` and a `content_type` containing `\n`. Keep all four string-field cases in the same table-driven assertion.

- [ ] **Step 2: Restore both ESI forms end to end**

Parameterize `publisher_esi_comment_is_never_stored_or_executed` over:

```rust
[
    "<!--ESI publisher-->",
    "<ESI:remove>publisher</ESI:remove>",
]
```

For each form, use a distinct cold cache/stub or distinct URL so each iteration independently proves bypass, byte preservation, no cache entry, and zero assembler calls.

- [ ] **Step 3: Repair CSP and seam comments**

Above the `[nonce]` handler, explain that meta `content` matching is supplemental because `lol_html` does not decode entity-encoded quotes; the structural `[nonce]` handler is the load-bearing refusal for any nonce an element can consume. Update the HTML processor assertion and publisher collision fixture rustdoc to describe terminal seam emission, repeated-marker rejection, and cache bypass.

- [ ] **Step 4: Add the C1/C3 glossary and re-anchor references**

Near the first surviving publisher cache reference, add a concise glossary:

```rust
// C1 is Fastly's raw origin/read-through cache. C3 is the forbidden cache of a
// final per-user assembled response. The template cache sits between them.
```

Rewrite the remaining references so each is intelligible locally and does not mix the old C2 taxonomy with “template cache.”

- [ ] **Step 5: Correct request max-age documentation**

Change the fail-closed inventory to “positive or malformed request `max-age`, `min-fresh`” so it agrees with the `max-age=0` reload paragraph.

- [ ] **Step 6: Run focused tests and formatting**

Run:

```bash
cargo test-fastly metadata_encoding_rejects_line_break_injection -- --nocapture
cargo test-fastly publisher_esi -- --nocapture
cargo test-fastly html_processor -- --nocapture
cargo fmt --all -- --check
cd docs
npx prettier --write guide/configuration.md
npm run format
```

Expected: all tests pass and formatters report no changes needed after formatting.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/publisher.rs docs/guide/configuration.md
git commit -m "Close template cache review gaps"
```

### Task 5: Run the complete local CI gate

**Files:** none unless a verification failure reveals an in-scope defect.

- [ ] **Step 1: Verify the worktree diff and formatting**

Run:

```bash
git status --short
git diff --check main...HEAD
cargo fmt --all -- --check
cd crates/trusted-server-js/lib && npm run format
cd docs && npm run format
```

Expected: only planned changes exist; every formatter passes.

- [ ] **Step 2: Run all target-matched tests**

Run:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
```

Expected: all pass. If only the CLI helper lacks a documented local prerequisite, record that explicitly; parity is required locally or must be confirmed green in CI.

- [ ] **Step 3: Run all target-matched clippy gates**

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: all pass with warnings denied.

- [ ] **Step 4: Run final JS verification**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run
node build-all.mjs
```

Expected: all tests and builds pass.

- [ ] **Step 5: Inspect the final diff**

Run:

```bash
git status --short --branch
git diff --stat origin/1009-esi-cacheable-root-spec...HEAD
git log --oneline origin/1009-esi-cacheable-root-spec..HEAD
```

Expected: the diff contains only the approved remediation and its spec/plan commits.

### Task 6: Update the PR and rerun browser integration

**Files:** none.

- [ ] **Step 1: Push the reviewed commits**

Run:

```bash
git push origin 1009-esi-cacheable-root-spec
```

Expected: the PR head advances to the final local commit.

- [ ] **Step 2: Locate the PR checks and browser workflow run**

Run:

```bash
gh pr view --json number,url,headRefOid,statusCheckRollup
gh pr checks
```

Identify the new-head `browser integration tests` check and its workflow run ID. Do not rerun an obsolete-head run.

- [ ] **Step 3: Rerun only the failed browser job if needed**

Before any rerun, mechanically verify the selected workflow run belongs to the current PR head:

```bash
PR_HEAD_SHA=$(gh pr view --json headRefOid --jq .headRefOid)
RUN_HEAD_SHA=$(gh run view <run-id> --json headSha --jq .headSha)
test "$RUN_HEAD_SHA" = "$PR_HEAD_SHA"
gh run rerun --job <browser-job-id>
```

Obtain `<browser-job-id>` from the selected run's jobs and only run the final command if the SHA comparison succeeds. This reruns the browser job alone rather than every failed job in the workflow. If the new push does not automatically schedule the browser job, locate the new-head workflow run rather than rerunning the old canceled run. Monitor until terminal state.

Expected: green browser integration tests. If Playwright installation again consumes the job timeout before any browser launches, capture the run URL and exact failure phase as infrastructure evidence; do not change workflow caching or `timeout-minutes` without separate approval.

- [ ] **Step 4: Report final verification and review mapping**

Summarize each resolved finding, the local gate results, the browser-check result, commit hashes, and any infrastructure-only limitation. Do not claim completion until all required local gates and the relevant remote check have terminal evidence.
