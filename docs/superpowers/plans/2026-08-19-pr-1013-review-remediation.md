# PR #1013 Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve all technically sound actionable review feedback on PR #1013 while preserving ordinary proxy behavior and the ESI spike's fail-safe contracts.

**Architecture:** Keep the existing publisher, template-cache, and adapter boundaries. Add narrow typed invariants at those boundaries: fallible Fastly body reads, an explicit response-privacy marker, shared publisher-ESI detection, and fallible metadata encoding. Behavioral changes are test-first; mechanical review cleanup follows once runtime contracts are green.

**Tech Stack:** Rust 2024, `error-stack`, Fastly SDK/Viceroy, `edgezero_core` HTTP types, TypeScript, Vitest, Bash/GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-08-19-pr-1013-review-remediation-design.md`

---

## File Map

- `crates/trusted-server-adapter-fastly/src/template_cache.rs`: fallible cache-body reads, insert length metadata, shared purge key, metadata-error propagation.
- `crates/trusted-server-adapter-fastly/src/main.rs`: terminal response effects keyed by an explicit privacy marker.
- `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`: consume the shared publisher-ESI detector and cover comment blocks.
- `crates/trusted-server-core/src/response_privacy.rs`: define and attach the typed terminal-private marker.
- `crates/trusted-server-core/src/platform/template_assembly.rs`: own the shared ESI-directive detector.
- `crates/trusted-server-core/src/platform/template_cache.rs`: shared purge constant, normalized keys, fallible metadata encoding, reservation and schema tests.
- `crates/trusted-server-core/src/platform/mod.rs`: public platform roster/export consistency.
- `crates/trusted-server-core/src/publisher.rs`: unconditional encoding restriction, collision bypass, unused-argument and page-bids cleanup, rustdoc/harness notes.
- `crates/trusted-server-core/src/creative_opportunities.rs` and `src/integrations/gpt_diagnostics.rs`: consolidate adjacent impl blocks and test conventions.
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts` and `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`: one-shot initial scheduler contract.
- `crates/trusted-server-js/lib/test/integrations/gpt/*.test.ts`: executable scheduling contracts.
- `Cargo.toml`, `crates/trusted-server-adapter-fastly/Cargo.toml`, `.github/workflows/test.yml`, `scripts/c2-local-test.sh`, `trusted-server.example.toml`, and `docs/guide/configuration.md`: dependency, CI, harness, and operator-facing cleanup.
- `docs/superpowers/archive/` plus cross-references: archive the two superseded documents.

### Task 1: Make Fastly cache I/O fail safely

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Test: `crates/trusted-server-adapter-fastly/src/template_cache.rs`

- [ ] **Step 1: Add a failing fallible-reader regression test**

Extract the byte-reading decision behind a private helper generic over `std::io::Read`, then test it with a reader that returns bytes followed by `io::Error`. The assertion must expect `ReadFoundError::Invalid(TemplateCacheMiss::Truncated)` (or `Backend` if investigation shows the adapter consistently classifies transport failures that way) and must not panic.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 cache_body_read_error`

Expected: FAIL because the current path uses `Body::into_bytes` and has no fallible helper/classification.

- [ ] **Step 3: Replace the panicking SDK conversion**

Import `std::io::Read as _`, call `read_to_end` on `found.to_stream()?`, map the error through `ReadFoundError`, and retain the post-read `metadata.body_len` check. Do not use `into_bytes`.

- [ ] **Step 4: Add `.known_length(body.len() as u64)` to direct `put`**

Place it beside `surrogate_keys` and `user_metadata`, matching the reservation insert builder.

- [ ] **Step 5: Run the adapter cache suite and verify GREEN**

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 template_cache`

Expected: all template-cache tests PASS, including the new read-error case.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/template_cache.rs
git commit -m "Make Fastly template cache reads fallible"
```

### Task 2: Preserve injection when encoding negotiation fails

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add a failing end-to-end publisher test**

In the ESI publisher tests, send a navigation with `Accept-Encoding: zstd, gzip;q=0, deflate;q=0, br;q=0, identity;q=0`. This fully refuses every representation TS can assemble, so `negotiate_reader_compression` must fail. Queue an origin response that would be undecodable if the header leaked, then assert the recorded origin request advertises `identity` and the returned HTML contains TSJS injection.

- [ ] **Step 2: Verify RED**

Run: `cargo test-fastly esi_unsupported_reader_encoding_still_injects_tsjs`

Expected: FAIL because ESI mode currently skips `restrict_accept_encoding` when `reader_supports_assembly` is false.

- [ ] **Step 3: Apply the minimal fix**

Call `restrict_accept_encoding(&mut req)` unconditionally before the origin fetch. Keep reader assembly eligibility separate; it controls shared assembly, not whether the origin offer is processable.

- [ ] **Step 4: Verify GREEN and the helper matrix**

Run: `cargo test-fastly publisher_proxy`

Run: `cargo test-fastly esi_unsupported_reader_encoding_still_injects_tsjs`

Expected: all matching tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Preserve injection for unsupported encodings"
```

### Task 3: Scope terminal privacy re-enforcement to TS-owned responses

**Files:**

- Modify: `crates/trusted-server-core/src/response_privacy.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Test: `crates/trusted-server-core/src/response_privacy.rs`
- Test: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Add two failing terminal-policy tests**

Extend the Fastly tests so an assembled response is explicitly marked and remains `private, no-store` after hostile late effects. Add a companion test whose unmarked origin response starts with `Cache-Control: private, max-age=600`, `ETag`, and `Last-Modified`; after terminal effects it must retain all three.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 terminal_response`

Expected: the ordinary-origin preservation test FAILS because current code infers TS ownership from the header value.

- [ ] **Step 3: Add a typed response extension**

Define a documented marker such as `TerminalPrivateResponse` in `response_privacy.rs`. Make `enforce_synthesized_html_cache_privacy` and the cached-template stamping path insert it when TS creates per-reader output. Keep `enforce_private_no_store` as the pure header mutation used during terminal re-enforcement.

- [ ] **Step 4: Consume the marker in Fastly terminal effects**

Replace the `is_private_or_no_store` snapshot with `response.extensions().get::<TerminalPrivateResponse>().is_some()`. Apply late effects first, then re-enforce only for marked responses. Leave the existing Set-Cookie privacy guard last.

- [ ] **Step 5: Verify GREEN across core and Fastly**

Run: `cargo test-fastly response_privacy`

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 terminal_response`

Expected: marked response remains terminal-private; unmarked origin-private response is unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/response_privacy.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Scope terminal privacy to synthesized responses"
```

### Task 4: Refuse publisher ESI and seam collisions without mutating bytes

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_assembly.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`

- [ ] **Step 1: Rewrite the collision test to express the desired behavior**

Change `an_origin_marker_collision_is_normalized_before_store` into a regression asserting the cold response preserves the publisher marker bytes, C2 stores no entry, and a second request reaches origin again. Add a script-string collision fixture so the test proves no HTML-comment-only neutralizer is involved.

- [ ] **Step 2: Add failing ESI-comment tests**

Test `<!--esi anything-->`, uppercase `<!--ESI`, and ordinary `<esi:include>` against the shared detector and adapter assembly gate.

- [ ] **Step 3: Verify RED**

Run: `cargo test-fastly origin_marker_collision`

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 esi_comment`

Expected: collision test observes mutation/storage and comment test is accepted by the current `<esi:`-only scan.

- [ ] **Step 4: Centralize the detector and revoke authorization**

Add a public, documented `contains_publisher_esi_directive(&[u8]) -> bool` in `platform/template_assembly.rs` that case-insensitively detects `<esi:` and `<!--esi`. Export it through `platform/mod.rs` and use it in publisher and adapter.

Remove seam emission from the shared-mode `lol_html` configuration. Once the authorized transform is fully buffered, it therefore contains publisher bytes and ordinary TS injections but no TS-owned seam. Add one helper that inserts a supplied payload immediately before the case-insensitive closing `</body>` tag, or appends it for fragments/malformed documents.

At that out-of-band finalization point, scan the completed transform for publisher `AD_ASSEMBLY_SEAM` bytes and shared ESI directives. If neither exists, insert exactly one inert `AD_ASSEMBLY_SEAM`, validate it, store it, and assemble as today. If either exists, revoke the reservation, insert the current reader's `seam_script_for(&params)` directly at the known structural insertion point, and return that private inline result without calling shared assembly. This preserves every publisher marker byte and avoids trying to distinguish identical markers after injection. Delete `normalize_fresh_template_seam`; its missing-body behavior moves into the insertion helper.

Add focused helper tests for case-insensitive `</body>`, a document with no body-close tag, and the literal text `"</body>"` inside a script followed by the real closing tag. The payload must be placed at the structural closing tag chosen by the existing HTML-processing convention, never inside script data.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test-fastly marker_collision`

Run: `cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1 esi_assembly`

Expected: all collision/directive inputs bypass or are refused and normal templates still assemble.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/platform/template_assembly.rs crates/trusted-server-core/src/platform/mod.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-adapter-fastly/src/esi_assembly.rs
git commit -m "Refuse publisher ESI and seam collisions"
```

### Task 5: Harden cache keys, metadata, and reservations

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Test: both template-cache modules

- [ ] **Step 1: Add failing key, metadata, and reservation tests**

Use the literal `"RSC"` in the case-insensitive key test. Add metadata cases containing `\r` and `\n` in every encoded field class and assert encoding returns a concrete error. Add a reservation test that calls `insert`, drops the wrapper, and asserts cancellation count remains zero.

In the Fastly adapter tests, pass unsafe metadata through both `FastlyTemplateReservation::insert` and `FastlyTemplateCache::put`; assert both return `TemplateCacheError` before beginning a cache write.

- [ ] **Step 2: Verify RED**

Run: `cargo test-fastly vary_header_names_are_matched_case_insensitively`

Run: `cargo test-fastly metadata_rejects_line_breaks`

Run: `cargo test-fastly fulfilled_reservation_does_not_cancel_on_drop`

Expected: the uppercase key splits, `encode` is infallible, and the new reservation expectation is not yet represented.

- [ ] **Step 3: Implement the key and metadata boundaries**

Lowercase each Vary name at `to_cache_key` serialization. Introduce a concrete `TemplateMetadataEncodeError` with `derive_more::Display` and `core::error::Error`; change `encode` to `Result<Vec<u8>, TemplateMetadataEncodeError>`. Reject CR/LF before writing any line. Map the error to `TemplateCacheError` in both Fastly insert paths so publisher storage fails open.

- [ ] **Step 4: Consolidate shared constants and docs**

Export `TEMPLATE_CACHE_PURGE_ALL_SURROGATE_KEY` from core and use it in `TemplateCacheKey::surrogate_keys` and Fastly `purge_all`. Derive the rendered-length assertion from `TEMPLATE_SCHEMA_VERSION`. Document that the default `lookup_or_reserve` exists for unsupported/null caches and should be overridden by cache implementations. Add `PlatformTemplateCache` to the platform trait roster and consolidate adjacent exports.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test-fastly template_cache`

Expected: all core and Fastly template-cache tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/platform/mod.rs crates/trusted-server-adapter-fastly/src/template_cache.rs
git commit -m "Harden template cache boundaries"
```

### Task 6: Make initial GPT scheduling one-shot

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`

- [ ] **Step 1: Add four failing scheduler contracts**

For the bundle and fallback, call `scheduleInitialAdInit` twice during generation zero and assert only the first payload is applied and `adInit` runs once. Add bundle tests showing omitted `initialSlots` preserves a pre-existing array and explicit `[]` replaces it.

- [ ] **Step 2: Verify RED**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt/schedule_initial_ad_init.test.ts test/integrations/gpt/gpt_bootstrap.test.ts`

Expected: duplicate-call tests report two queued/running `adInit` calls; slot-contract tests expose any accidental overwrite ambiguity.

- [ ] **Step 3: Add the one-shot latch in both mirrors**

Set a private property on `ts` before applying the first payload or registering callbacks. Return early on later calls and on nonzero navigation generations. Extend the TypeScript interface locally if necessary without adding a public API field. Preserve the truthiness check so `undefined` means preserve and `[]` means replace.

- [ ] **Step 4: Replace historical API narration with durable invariants**

Keep the generation, hydration, hidden-document, and slot-handoff rationale; remove wording about the prior missing assignment/bug.

- [ ] **Step 5: Verify GREEN and build output**

Run: `cd crates/trusted-server-js/lib && npx vitest run test/integrations/gpt/schedule_initial_ad_init.test.ts test/integrations/gpt/gpt_bootstrap.test.ts`

Run: `cd crates/trusted-server-js/lib && node build-all.mjs`

Expected: tests PASS and bundles rebuild successfully.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-core/src/integrations/gpt_bootstrap.js crates/trusted-server-js/lib/test/integrations/gpt/schedule_initial_ad_init.test.ts crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts
git commit -m "Make initial GPT scheduling one-shot"
```

### Task 7: Remove spike residue and apply Rust consistency fixes

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`

- [ ] **Step 1: Preserve page-bids format behavior with tests**

Replace enum-parser unit tests with endpoint-level or helper tests proving absent/`json` succeeds and `fragment`, typo, and empty values return the existing unknown-format response.

- [ ] **Step 2: Verify tests before refactor**

Run: `cargo test-fastly page_bids_format`

Expected: existing behavior tests PASS; this is a characterization refactor, so no RED is required until the enum is removed.

- [ ] **Step 3: Apply mechanical cleanup**

Remove `PageBidsFormat` and use a direct `matches!(format, None | Some("json"))` guard. Remove the unused `RuntimeServices` parameter from `store_template_if_authorized` and its call sites. Remove the dead non-identity cached-response encoding branch. Change rustdoc links to the test-only `c2_bypass_reason` into code formatting. Add a comment at `build_seam_script` pointing to the harness literals. Fold adjacent impl blocks, remove test-only `#[must_use]`, and add `"should ..."` messages to the noted assertions.

- [ ] **Step 4: Verify core behavior and documentation**

Run: `cargo test-fastly page_bids`

Run: `cargo test-fastly gpt_diagnostics`

Run: `cargo doc -p trusted-server-core --no-deps --all-features --target wasm32-wasip1`

Expected: tests and rustdoc PASS with no broken intra-doc links.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/integrations/gpt_diagnostics.rs
git commit -m "Remove ESI spike residue"
```

### Task 8: Align manifests, examples, CI, and historical docs

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/trusted-server-adapter-fastly/Cargo.toml`
- Modify: `.github/workflows/test.yml`
- Modify: `scripts/c2-local-test.sh`
- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Move the #1009 validation spike and cacheable-root design into the flat `docs/superpowers/archive/` directory.
- Modify: every reference returned by `rg` for those filenames

- [ ] **Step 1: Move the ESI dependency to workspace dependencies**

Add the full pinned `esi` Git revision under `[workspace.dependencies]`; change the Fastly manifest to `esi = { workspace = true }`. Run `cargo metadata --no-deps --format-version 1` and expect success without lockfile churn.

- [ ] **Step 2: Correct operator documentation and examples**

Label the configuration section experimental and spike-scoped under #1009. Use the complete four-name Next.js Vary list in both files and state uncovered origin `Vary` names refuse storage. Change the bypass text to “positive or malformed request `max-age`”.

- [ ] **Step 3: Harden the harness timing check**

Set `BID_DELAY=3` on both workflow harness invocations. In the script, branch around the ratio assertion after a nonnumeric probe so the probe contributes one failure and does not compare fabricated zeros. Keep `first_body_byte < complete / 3` unchanged for valid timings.

- [ ] **Step 4: Archive historical docs and repair links**

Move both files into the flat `docs/superpowers/archive/` convention. Run:

```bash
rg -n "1009-esi-validation-spike|esi-cacheable-root-validation-design" docs crates
```

Update every result, including relative links inside the archived files, until all links resolve to the archive paths.

- [ ] **Step 5: Verify formatting and references**

Run: `cd docs && npm run format`

Run: `cd crates/trusted-server-js/lib && npm run format`

Run a stale-link search for either archived filename under the old `plans/` or `specs/` directory.

Expected: formatters PASS and the stale-path search returns no matches.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/trusted-server-adapter-fastly/Cargo.toml .github/workflows/test.yml scripts/c2-local-test.sh trusted-server.example.toml docs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Align ESI spike documentation and tooling"
```

### Task 9: Resolve the fingerprint suggestion against runtime lifetime

**Files:**

- Inspect: `crates/trusted-server-adapter-fastly/src/app.rs`
- Inspect: `crates/trusted-server-core/src/publisher.rs`
- No planned code modification: expand scope only after proving a durable cross-request owner exists

- [ ] **Step 1: Trace settings lifetime on every adapter**

Document whether `Settings` survives multiple publisher requests in Fastly, Axum, Cloudflare, and Spin. Confirm the Fastly `AppState` lifecycle against the entry point and SDK runtime behavior rather than assuming a process model.

- [ ] **Step 2: Choose one evidence-backed resolution**

The expected resolution, based on `AppState`'s documented per-request Wasm lifecycle, is no code change and a concise review-thread response explaining why `OnceLock` merely relocates the same once-per-request computation. If investigation instead proves a durable cross-request owner exists, stop before editing, amend the design and plan with the exact owner/API/tests, and obtain user approval for that expanded implementation.

- [ ] **Step 3: Re-run fingerprint characterization tests**

Run: `cargo test-fastly template_fingerprint`

Expected: fingerprints remain deterministic and change for template-shaping settings.

- [ ] **Step 4: Record the evidence-backed response in the handoff checklist**

Do not create an empty commit. Link the `AppState` lifecycle evidence and explain that a durable cache would require a separate architecture decision if none exists today.

### Task 10: Full verification and review handoff

**Files:**

- Inspect: all changed files
- No code changes unless a verification failure exposes a regression; any fix starts a new RED/GREEN cycle.

- [ ] **Step 1: Run format and rustdoc**

```bash
cargo fmt --all -- --check
cargo doc -p trusted-server-core --no-deps --all-features --target wasm32-wasip1
cd docs && npm run format
cd ../crates/trusted-server-js/lib && npm run format
```

- [ ] **Step 2: Run all target-matched clippy gates**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

- [ ] **Step 3: Run Rust and parity tests**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
```

- [ ] **Step 4: Run JavaScript verification**

```bash
cd crates/trusted-server-js/lib
npx vitest run
node build-all.mjs
```

- [ ] **Step 5: Run the C2 local harness when prerequisites exist**

```bash
BID_DELAY=3 ./scripts/c2-local-test.sh esi
BID_DELAY=3 ./scripts/c2-local-test.sh inline
```

Expected: both report zero failures. If Viceroy or the production Wasm artifact is unavailable, record the exact prerequisite failure and rely on the matching CI jobs after push.

- [ ] **Step 6: Review the final diff**

Run: `git diff --check origin/main...HEAD`

Run: `git status --short`

Confirm no generated secrets, local configuration, unrelated refactors, or stale review references are present.

- [ ] **Step 7: Prepare thread-by-thread responses**

For each GitHub inline comment, record the commit/file that resolves it or the evidence-backed technical response. Do not post or resolve threads without the user's explicit request; provide the response list for review.
