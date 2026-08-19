# Template-cache terminology Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace active shared-template-cache `C2`/`c2` terminology with `TemplateCache`/`template_cache`, migrate the public diagnostic header and opaque key namespace, and rename the local harness without changing cache behavior.

**Architecture:** Keep the existing publisher, platform-cache, Fastly adapter, and ESI assembly boundaries. Change the key hash-domain bytes and rendered prefix together, rename the response-state API and diagnostics together, and update all active callers and documentation; do not change schema version, template bytes, eligibility, freshness, privacy, or assembly behavior. The migration design itself remains the source of the permitted old-spelling compatibility references, and the archived documents remain historical records.

**Tech Stack:** Rust 2024 (`trusted-server-core`, Fastly adapter, wasm32-wasip1 tests), Bash, GitHub Actions YAML, TOML, Markdown, and the repository’s Cargo/npm verification aliases.

---

## File map and terminology boundary

Implementation and tests:

- Modify `crates/trusted-server-core/src/platform/template_cache.rs`: replace the `ts-c2` canonical hash-domain and rendered key prefix with `ts-template-cache`; rewrite taxonomy comments; update/add deterministic namespace tests. Keep the exact schema-history marker `<!--ts-c2-v3-seam-7f4c9e2d-bids-->` unchanged.
- Modify `crates/trusted-server-core/src/publisher.rs`: rename `HEADER_X_TS_C2_CACHE`, `C2ResponseState`, `C2BypassReason`, `C2CachePolicy`, `set_c2_response_state`, `request_bypasses_c2`, `c2_bypass_reason`, `c2_cache_ttl`, local variables, test modules, assertions, comments, and `c2_template_cache` log messages; emit only `x-ts-template-cache` and test that the old header is absent.
- Modify `crates/trusted-server-core/src/html_processor.rs`: rename the active `reserved-c2-seam` test fixture to a template-cache seam name and update its assertions/comments. This fixture is not the schema-history marker exception.
- Modify `crates/trusted-server-core/src/creative_opportunities.rs`, `crates/trusted-server-core/src/platform/types.rs`, and `crates/trusted-server-core/src/response_privacy.rs`: rewrite active cache comments and validation/test messages to “template cache” terminology.
- Modify `crates/trusted-server-adapter-fastly/src/template_cache.rs` and `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`: rewrite active comments and the legacy-read log to `template_cache`; do not alter Fastly cache operations or ESI behavior.

Harness, CI, and operator material:

- Rename `scripts/c2-local-test.sh` to `scripts/template-cache-local-test.sh`; update its function name, header parser, log regexes, comments, and usage text.
- Modify `.github/workflows/test.yml` so the step name and both invocations call `scripts/template-cache-local-test.sh`.
- Modify `trusted-server.example.toml` and `docs/guide/configuration.md` so operator-facing prose, diagnostics, and harness commands say “template cache” and use `X-TS-Template-Cache`.
- Modify these non-archived active plans/specifications where the occurrences describe this cache: `docs/superpowers/plans/2026-08-08-1009-measurement-and-stage-0.md`, `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md`, `docs/superpowers/plans/2026-08-12-1009-esi-merge-hardening.md`, `docs/superpowers/plans/2026-08-14-1009-esi-parser-assembly.md`, `docs/superpowers/plans/2026-08-19-pr-1013-review-remediation.md`, `docs/superpowers/specs/2026-08-11-1009-streaming-assembly-architecture.md`, `docs/superpowers/specs/2026-08-12-1009-esi-merge-hardening-design.md`, `docs/superpowers/specs/2026-08-14-1009-esi-parser-assembly-design.md`, and `docs/superpowers/specs/2026-08-19-pr-1013-review-remediation-design.md`. Preserve unrelated `c2` substrings such as checksums, cookie/EC identifiers, creative IDs, and third-party fixture content.
- Do not edit `docs/superpowers/archive/**`. Do not rewrite the exact schema-history marker. Leave `docs/superpowers/specs/2026-08-19-template-cache-terminology-design.md` as the migration record of the old/new names and compatibility effects; its before/after references are an explicit exception.

The implementation worker should use `@superpowers:test-driven-development` for the focused contract changes, `@superpowers:subagent-driven-development` or `@superpowers:executing-plans` for this task sequence, and `@superpowers:verification-before-completion` before claiming completion.

### Task 1: Establish failing key-namespace and public-header contracts

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs` (test module near `rendered_key_is_fixed_size_and_contains_no_request_material`)
- Modify: `crates/trusted-server-core/src/publisher.rs` (the existing cold/warm end-to-end response-state test)

- [ ] **Step 1: Run the current focused baseline.**

Run:

```bash
cargo test-fastly template_cache::tests::rendered_key_is_fixed_size_and_contains_no_request_material
cargo test-fastly publisher::c2_end_to_end_tests::a_second_request_is_served_from_the_cache_without_touching_the_origin
```

Expected: both commands PASS against the old `ts-c2-v4-...` key and `x-ts-c2-cache` header, establishing that the existing behavior is green before changing expected contracts.

- [ ] **Step 2: Add the deterministic new namespace expectation first.**

In `platform/template_cache.rs`, change the fixed-length expectation to use `ts-template-cache-v{TEMPLATE_SCHEMA_VERSION}-`, assert that the rendered key starts with that prefix, and add an exact fixture assertion for `key()`:

```rust
assert_eq!(
    key().to_cache_key(),
    "ts-template-cache-v4-54431eb4ea82644d6378717a8c3f18302fafbf739e684598da79e392b16900a6"
);
```

This exact value proves both the visible prefix and the canonical hash-domain bytes changed; it must not be replaced by a length-only assertion.

- [ ] **Step 3: Add the public-header expectation first.**

In the cold/warm publisher test, read `x-ts-template-cache` using `HeaderName::from_static` and assert the old `x-ts-c2-cache` header is absent on both the cold and warm responses. Keep the existing `miss-stored` and `hit` values and origin/body assertions unchanged. Use the new raw header name in this test before renaming the production constant so the test compiles and fails at the observable contract.

- [ ] **Step 4: Run the focused contracts and verify they fail for the intended old values.**

Run:

```bash
cargo test-fastly template_cache::tests::rendered_key_is_fixed_size_and_contains_no_request_material
cargo test-fastly publisher::c2_end_to_end_tests::a_second_request_is_served_from_the_cache_without_touching_the_origin
```

Expected: the key test FAILS with the old `ts-c2-v4-...` output (including the old digest), and the publisher test FAILS because the response still emits `x-ts-c2-cache` instead of `x-ts-template-cache`. No policy, body, or cache-state assertion should fail for another reason.

- [ ] **Step 5: Commit the red contract tests.**

Do not commit source implementation changes yet. Commit only the two focused test expectation changes:

```bash
git add crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Specify template cache namespace and header"
```

### Task 2: Migrate the opaque template-cache key namespace

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Test: `crates/trusted-server-core/src/platform/template_cache.rs`

- [ ] **Step 1: Replace both namespace components and terminology in the implementation.**

Change only the namespace inputs/labels and active prose: hash `b"ts-template-cache"` instead of `b"ts-c2"`, render `ts-template-cache-v{schema_version}-{digest}`, and describe the shared transformed template without the C1/C2/C3 numbered taxonomy. Keep `TEMPLATE_SCHEMA_VERSION` at `4`, keep the exact historical v3 marker, keep surrogate keys, and keep all key fields/order and hash algorithm unchanged.

- [ ] **Step 2: Run the key-focused suite.**

Run:

```bash
cargo test-fastly template_cache
```

Expected: PASS, including the exact deterministic namespace assertion, fixed-length/key-format assertion, all-field-distinctness tests, delimiter-collision test, Vary case/order tests, metadata tests, and reservation tests. The old namespace must not be accepted as an alias or read path.

- [ ] **Step 3: Commit the namespace boundary.**

```bash
git add crates/trusted-server-core/src/platform/template_cache.rs
git commit -m "Move template cache keys to named namespace"
```

### Task 3: Rename publisher state, policy APIs, logs, and public diagnostics

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Rename the response-state API and all active internal identifiers.**

Use `TemplateCacheResponseState`, `HEADER_X_TS_TEMPLATE_CACHE`, `set_template_cache_response_state`, `TemplateCacheBypassReason`, `TemplateCachePolicy`, `request_bypasses_template_cache`, `template_cache_bypass_reason`, and `template_cache_ttl`. Rename local `c2_*` variables and the `c2_store_authorization_tests`, `c2_end_to_end_tests`, and `c2_gate_tests` modules to `template_cache_*`. Replace every active `c2_template_cache` log prefix with `template_cache`; preserve the same bounded values (`hit`, `miss-stored`, `miss-store-error`, `miss-reserved`, `bypass-request`, `bypass-response`, `unsupported`, `invalid`, `backend-error`). Rewrite comments/assertion messages to “template cache” without changing logic.

- [ ] **Step 2: Emit only the new public header.**

Make the renamed setter insert `HeaderValue::from_static(state.as_str())` under `x-ts-template-cache`. Do not emit `x-ts-c2-cache` as an alias. Update all publisher tests that use the constant or literal to the renamed constant/new literal, while retaining the explicit old-header-absent assertion from Task 1.

- [ ] **Step 3: Run the focused publisher suites.**

Run:

```bash
cargo test-fastly template_cache_store_authorization_tests
cargo test-fastly template_cache_end_to_end_tests
cargo test-fastly template_cache_gate_tests
```

Expected: PASS. Cold, warm, reserved, bypass, unsupported, invalid, and backend-error states retain their existing values; miss/hit assembly, origin counts, privacy headers, diagnostics bypass, policy gates, and body identity remain unchanged. The new header is present for each relevant state and the old header is absent.

- [ ] **Step 4: Commit the publisher boundary.**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Name template cache diagnostics"
```

### Task 4: Finish active Rust terminology and seam fixtures

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Modify: `crates/trusted-server-core/src/response_privacy.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`

- [ ] **Step 1: Rename the active seam fixture and supporting prose.**

Change `reserved-c2-seam` to `reserved-template-cache-seam` in the HTML processor test and its source/collision assertions. Rewrite comments and rustdoc that call the shared transformed template “C2”; use “template cache” or “shared transformed template.” Do not touch the exact `ts-c2-v3` schema-history marker in `platform/template_cache.rs`.

- [ ] **Step 2: Rename Fastly adapter log/comment terminology.**

Change the legacy read warning to `template_cache legacy read failed` and update Fastly/ESI rustdoc. Do not change error classification, cache key construction (which already consumes the core key), transaction behavior, or assembly output.

- [ ] **Step 3: Run the focused supporting suites.**

Run:

```bash
cargo test-fastly html_processor
cargo test-fastly template_cache
cargo test-fastly publisher
```

Expected: PASS; the renamed fixture still proves the transform-owned terminal seam, the key suite still proves the new namespace, and publisher behavior remains unchanged apart from terminology/header names.

- [ ] **Step 4: Commit the remaining Rust terminology.**

```bash
git add crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/platform/types.rs crates/trusted-server-core/src/response_privacy.rs crates/trusted-server-adapter-fastly/src/template_cache.rs crates/trusted-server-adapter-fastly/src/esi_assembly.rs
git commit -m "Describe shared templates consistently"
```

### Task 5: Rename the local harness and its CI caller

**Files:**

- Rename: `scripts/c2-local-test.sh` → `scripts/template-cache-local-test.sh`
- Modify: `scripts/template-cache-local-test.sh`
- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Rename the script without creating a compatibility shim.**

Use `git mv scripts/c2-local-test.sh scripts/template-cache-local-test.sh`. Update usage comments, `c2_state` to `template_cache_state`, the `x-ts-c2-cache` parser to `x-ts-template-cache`, all `c2_template_cache` log patterns to `template_cache`, and prose describing the inert marker. Keep `esi` and `inline` argument behavior and all timing/body/origin-count assertions intact.

- [ ] **Step 2: Update CI callers.**

Rename the workflow step to “Run template cache ESI local harness” and update both CI commands to `BID_DELAY=3 ./scripts/template-cache-local-test.sh esi` and `BID_DELAY=3 ./scripts/template-cache-local-test.sh inline`.

- [ ] **Step 3: Run shell and harness verification.**

Run:

```bash
bash -n scripts/template-cache-local-test.sh
BID_DELAY=3 ./scripts/template-cache-local-test.sh esi
BID_DELAY=3 ./scripts/template-cache-local-test.sh inline
```

Expected: syntax check PASS; both harness modes PASS, with cold `miss-stored`, warm `hit`, new `X-TS-Template-Cache` parsing, expected origin counts, seam/assembly integrity, and no old-header/log matches. If local Viceroy prerequisites are unavailable, record that environmental block explicitly and run the same commands in CI before completion; do not add a legacy script shim.

- [ ] **Step 4: Commit the harness/CI boundary.**

```bash
git status --short scripts .github/workflows/test.yml
git add -A -- scripts .github/workflows/test.yml
git commit -m "Rename template cache local harness"
```

### Task 6: Update active operator documentation, examples, plans, and specs

**Files:**

- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Modify: the nine active plans/specs listed in the file map

- [ ] **Step 1: Update the operator example and guide.**

Replace numbered-cache prose with “template cache,” update all diagnostic examples to `X-TS-Template-Cache`, and update harness commands to `scripts/template-cache-local-test.sh`. Preserve the configuration keys, safety caveats, bounded state values, rollback instructions, `ts-template` surrogate key, and all behavior descriptions. Explain raw origin caching/final assembly by those names where an old C1/C2 taxonomy was used.

- [ ] **Step 2: Update active plans/specs mechanically but semantically.**

Rename cache-related `C2`, `c2_*`, `x-ts-c2-cache`, and `scripts/c2-local-test.sh` references to the named terminology, including completed checklist text and historical findings. Rewrite sentences that distinguish cache layers in terms of raw origin bytes, shared template cache, and final assembled response. Leave unrelated IDs, hashes, cookie/EC identifiers, and third-party content unchanged.

- [ ] **Step 3: Verify documentation formatting and active references.**

Run:

```bash
cd docs && npm run format
cd ..
rg -n -i --glob '!docs/superpowers/archive/**' --glob '!docs/superpowers/specs/2026-08-19-template-cache-terminology-design.md' --glob '!docs/superpowers/plans/2026-08-19-template-cache-terminology.md' 'X-TS-C2-Cache|x-ts-c2-cache|ts-c2|c2_template_cache|C2Response|C2Bypass|C2Cache|c2_bypass|c2_cache|c2-local-test|reserved-c2-seam|\bC2\b|\bc2\b' crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/platform/types.rs crates/trusted-server-core/src/response_privacy.rs crates/trusted-server-adapter-fastly/src scripts trusted-server.example.toml docs/guide docs/superpowers/plans docs/superpowers/specs
```

Expected: the only result is the exact retained
`<!--ts-c2-v3-seam-7f4c9e2d-bids-->` schema-history marker in
`platform/template_cache.rs`. Inspect that single result rather than weakening the
search. The excluded migration design and implementation plan may retain their explicit
old/new compatibility references; archived documents and unrelated substrings are not
migration failures.

- [ ] **Step 4: Commit the documentation boundary.**

```bash
git add trusted-server.example.toml docs/guide/configuration.md docs/superpowers/plans/2026-08-08-1009-measurement-and-stage-0.md docs/superpowers/plans/2026-08-08-1009-measurement-findings.md docs/superpowers/plans/2026-08-12-1009-esi-merge-hardening.md docs/superpowers/plans/2026-08-14-1009-esi-parser-assembly.md docs/superpowers/plans/2026-08-19-pr-1013-review-remediation.md docs/superpowers/specs/2026-08-11-1009-streaming-assembly-architecture.md docs/superpowers/specs/2026-08-12-1009-esi-merge-hardening-design.md docs/superpowers/specs/2026-08-14-1009-esi-parser-assembly-design.md docs/superpowers/specs/2026-08-19-pr-1013-review-remediation-design.md
git commit -m "Use template cache terminology in documentation"
```

### Task 7: Run full verification and review the migration diff

**Files:**

- Test/verify: all files changed by Tasks 1–6

- [ ] **Step 1: Run Rust formatting and target-matched tests.**

Run:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all commands PASS. The Fastly suite is the authoritative shared-template-cache test target; Axum and Cloudflare confirm the terminology/API changes do not break adapters that use the unavailable-cache fallback.

- [ ] **Step 2: Run target-matched Clippy and JS tests/formatting.**

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs
cd ../../..
```

Expected: all commands PASS with no new warnings, and JavaScript tests/formatting remain green; no JS behavior should have changed.

- [ ] **Step 3: Re-run the renamed harness and documentation search.**

Run:

```bash
BID_DELAY=3 ./scripts/template-cache-local-test.sh esi
BID_DELAY=3 ./scripts/template-cache-local-test.sh inline
rg -n -i --glob '!docs/superpowers/archive/**' --glob '!docs/superpowers/specs/2026-08-19-template-cache-terminology-design.md' --glob '!docs/superpowers/plans/2026-08-19-template-cache-terminology.md' 'X-TS-C2-Cache|x-ts-c2-cache|ts-c2|c2_template_cache|C2Response|C2Bypass|C2Cache|c2_bypass|c2_cache|c2-local-test|reserved-c2-seam|\bC2\b|\bc2\b' crates/trusted-server-core/src/platform/template_cache.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/platform/types.rs crates/trusted-server-core/src/response_privacy.rs crates/trusted-server-adapter-fastly/src scripts trusted-server.example.toml docs/guide docs/superpowers/plans docs/superpowers/specs
git diff --check
git status --short
```

Expected: both harness modes PASS; the active-cache search returns only the exact
retained v3 schema-history marker; `git diff --check` PASS; and `git status --short` is
empty (no stale `scripts/c2-local-test.sh`, generated artifacts, or unrelated edits).
Confirm the only other retained old spellings live in the excluded migration design,
implementation plan, archived records, and explicitly unrelated substrings.

- [ ] **Step 4: Review the final diff before handoff.**

Use `git diff HEAD~6..HEAD --stat` and `git diff HEAD~6..HEAD --` (adjust the commit range if additional logical commits were made) to confirm the changes are terminology-only: no schema-version bump, no dual header, no old-namespace read, no policy/TTL/eligibility change, no template-byte change, and no script shim. Follow `@superpowers:verification-before-completion` and report command evidence before claiming completion.
