# PR #1013 Round-3 Review Remediation Design

## Goal

Resolve every actionable round-2 and round-3 review finding on PR #1013 while preserving the existing template-cache architecture and avoiding unrelated refactoring.

## Scope

This remediation covers the page-bids terminal-privacy gap, the bootstrap scheduler test drift, the bootstrap-to-bundle one-shot handoff, explicit Fastly reservation cancellation, the remaining focused test and documentation gaps, and rerunning the browser integration job.

The reviewer-recorded inert seam residue and `identity;q=0` behavior remain unchanged because they are explicitly accepted. Enforcing the terminal-private marker in adapters that do not apply late `RequestFilterEffects` remains out of scope. Workflow timeout or caching changes also remain out of scope unless a rerun reproduces the Playwright installation stall and the user separately chooses to address CI infrastructure.

## Approach

Use narrow, test-driven changes grouped by invariant. Prefer shared helpers for security behavior, but do not consolidate unrelated response construction or redesign the cache.

### Terminal-private page-bids responses

Introduce a generically named core helper that applies the existing `private, no-store` policy and inserts `TerminalPrivateResponse`. Keep the synthesized-HTML helper as the HTML-specific entry point, implemented in terms of the generic helper, so existing HTML call sites remain expressive.

Route every page-bids response shape that Trusted Server marks private through the generic helper: preflight denial, unknown-format failure, successful JSON, and the synthesized invalid-304 response. This preserves the typed marker until Fastly's terminal hook applies late response mutations and then restores the privacy invariant.

Tests will prove that page-bids response constructors attach the marker and that a Fastly `RequestFilterEffects` mutation attempting to set public CDN caching is removed before send. Coverage will include the successful per-user JSON path rather than relying only on a synthetic marker test.

### GPT scheduler handoff and parity

Store the initial-ad-init one-shot latch on the shared `tsjs` object rather than inside each installed scheduler closure. Both the bootstrap fallback and main bundle scheduler will consult and set the same internal boolean. Installing the bundle may replace the fallback function, but it cannot reset a scheduling decision already made for the document.

The typed `TsjsApi` surface will document this field as internal lifecycle state. Tests will exercise the bootstrap-to-bundle handoff and prove the second scheduler cannot overwrite bids, slots, or schedule another `adInit`. The bootstrap suite will also mirror the bundle suite's omitted-versus-explicitly-empty `initialSlots` contract tests.

### Fastly reservation release

On body-length mismatch or metadata-encoding failure, `FastlyTemplateReservation::insert` will call `cancel_insert_or_update` before returning the validation error. If cancellation itself fails, return a `TemplateCacheError` that reports the cancellation failure with the original validation context attached or included, following the adapter's existing concrete error style. Successful validation retains the current insertion path.

The direct-write comment will be corrected to state the actual safety mechanism: an unfinished write is never finished, and fallible reads plus declared-versus-observed body-length validation prevent partial data from being accepted.

### Focused coverage and documentation cleanup

The remediation will also:

- add CR/LF rejection cases for metadata policy-header names and `content_type`, completing coverage of all encoded string fields;
- cover both comment-form and element-form publisher ESI in the end-to-end cache-bypass test;
- document that the `[nonce]` structural handler is the load-bearing CSP nonce safety net when a meta policy uses entity-encoded quotes;
- update the stale seam-collision assertion message to describe terminal emission and repeated-marker rejection rather than removed normalization;
- add a local C1/C3 glossary near the publisher cache terminology and use it to re-anchor the remaining references;
- qualify the configuration guide's request `max-age` inventory so `max-age=0` reload reuse agrees with the detailed paragraph;
- leave the explicitly accepted inert double-collision residue unchanged.

## Error Handling

Runtime error handling remains within the existing `TemplateCacheError` and `error-stack` boundaries. Terminal-private marking is infallible. Page-bids behavior and response bodies do not change; only their out-of-band privacy marker becomes complete. Cache validation failures still fail the store operation, but now deliberately discharge the Fastly transaction obligation before returning.

## Verification

Each behavioral change starts with a regression test and the narrowest relevant suite runs immediately after implementation. Final verification includes:

- focused core publisher, response-privacy, HTML-processor, template-cache, and Fastly adapter tests;
- `cd crates/trusted-server-js/lib && npx vitest run`, the JS build, and JS formatting;
- `cargo fmt --all -- --check`;
- `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, and `cargo test-spin`;
- `cargo clippy-fastly`, `cargo clippy-axum`, `cargo clippy-cloudflare`, `cargo clippy-cloudflare-wasm`, `cargo clippy-spin-native`, and `cargo clippy-spin-wasm`;
- documentation formatting;
- the parity and native CLI suites when their local prerequisites are available.

After the implementation is pushed, rerun the failed browser integration job. A green rerun resolves the CI finding. A repeated timeout during Playwright installation will be recorded as runner-side infrastructure evidence and will not trigger an unapproved workflow redesign.
