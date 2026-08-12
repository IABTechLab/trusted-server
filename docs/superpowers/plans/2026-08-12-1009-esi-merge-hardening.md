# #1009 ESI Merge and Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement
> this plan task-by-task. This plan is intentionally executed inline because the operator
> explicitly prohibited subagents.

**Goal:** Merge current `main` and make the opt-in ESI byte-seam/shared-template path correct,
private, cache-semantic, compressed, observable, and operationally reversible.

**Architecture:** Fastly Core Cache holds identity-encoded reader-neutral templates behind a
transaction acquired before origin work. Every request assembles its own slots and structured bid
map at an exact inert seam, encodes the result for that client, and receives a final immutable
private/no-store policy.

**Tech Stack:** Rust 1.95, Fastly Compute/Core Cache, `edgezero_core` HTTP types, `lol_html`,
TypeScript/Vitest, Viceroy, shell harness.

---

### Task 1: Merge live main and preserve auction contracts

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Test: adjacent Rust and Vitest modules

- [ ] Merge `origin/main` with `git merge --no-ff origin/main`.
- [ ] Resolve the `AdBidsState`/`write_bids_to_state` conflict by building one structured map with
      `auction_id`, storing both map and script, and returning its delivered slot IDs.
- [ ] Add/adjust tests proving ESI and inline retain `hb_auction_id`, APS renderer metadata, and
      delivered-winner attribution.
- [ ] Run the focused Rust and GPT tests.
- [ ] Complete the merge commit.

### Task 2: Remove mechanisms outside the approved ESI byte-seam design

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Delete: `crates/trusted-server-core/src/platform/template_assembly.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Delete: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-adapter-fastly/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] Update mode tests to specify only `inline` and `esi`; watch the old client-fill expectations
      fail or stop compiling.
- [ ] Remove `ClientFill`, executable fragment serialization, assembler traits/registration, and
      the `esi` crate.
- [ ] Update comments to call the production path byte-seam assembly.
- [ ] Run focused configuration, publisher, and Fastly adapter tests.
- [ ] Commit the scope cleanup.

### Task 3: Canonicalize and bound the template key

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] Write failing tests for absent versus empty `Vary`, repeated raw values, invalid configured
      names, punctuation-colliding purge URLs, changed origin host override, and changed creative
      configuration.
- [ ] Replace string pairs with a typed canonical `Vary` value preserving presence and all bytes.
- [ ] Hash a length-prefixed canonical key and hash the URL-specific surrogate key.
- [ ] Include publisher origin identity and the complete template-shaping fingerprint.
- [ ] Run focused key/configuration tests and commit.

### Task 4: Enforce request and origin cache semantics

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/Cargo.toml` if HTTP-date parsing needs a direct dependency

- [ ] Write failing tests for response `max-age=0`, positive max age, repeated/malformed cache
      directives, `Age` exhaustion, expired/malformed `Expires`, missing freshness, invalid `Vary`,
      and request no-cache/no-store/range/conditional bypasses.
- [ ] Add a typed cache eligibility result carrying the positive remaining TTL.
- [ ] Parse relevant response directives fail-closed and cap, never extend, origin freshness.
- [ ] Add request-side bypass classification before lookup.
- [ ] Make unsupported/backend-failed cache lookups fall back to inline processing on non-Fastly
      adapters rather than buffering a cacheless ESI path.
- [ ] Run focused eligibility tests and commit.

### Task 5: Move request collapse before the origin fetch

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`

- [ ] Write a failing cache test showing two concurrent cold requests currently perform two
      origin fetches/transforms.
- [ ] Introduce a lookup outcome with an opaque insert reservation and explicit cancellation.
- [ ] Implement Fastly `Transaction::lookup` before origin work and consume/cancel its obligation
      on every exit path.
- [ ] Ensure invalid fresh entries become replaceable rather than causing repeated refetches.
- [ ] Run focused Core Cache/Viceroy tests and commit.

### Task 6: Make privacy and policy-header parity final

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/response_privacy.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs` tests

- [ ] Write failing tests for repeated CSP/CSP-Report-Only, omitted COOP/COEP/CORP/HSTS/Link,
      unknown cached header metadata, duplicate required metadata fields, and a late integration
      changing `Cache-Control` to public.
- [ ] Capture all ordered values, expand the safe allowlist, and decode metadata strictly.
- [ ] Replay with `append`, then apply the assembled-response privacy policy last.
- [ ] Preserve and reassert private/no-store after request-filter effects in Fastly's final send.
- [ ] Run focused header/privacy tests and commit.

### Task 7: Bypass shared templates for request-private diagnostics

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] Write a failing warm-cache test activated by diagnostics query and another by diagnostics
      cookie.
- [ ] Make `requires_private_no_store()` a lookup/store disqualifier.
- [ ] Verify ordinary diagnostics-disabled requests still hit C2.
- [ ] Run focused diagnostics/C2 tests and commit.

### Task 8: Re-encode assembled responses for the reader

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] Write failing cold/warm tests requiring gzip/br clients to receive a matching encoded body
      and proving reader encoding no longer partitions the stored template.
- [ ] Use one canonical compressed `Accept-Encoding` offer for every shared-template origin miss.
- [ ] Carry the selected response encoding separately from identity template metadata.
- [ ] Encode buffered assembly after splicing and stream hit prefix/seam/suffix through one encoder.
- [ ] Handle `identity;q=0` without serving an unacceptable representation.
- [ ] Emit the correct `Vary: Accept-Encoding` response semantics after final encoding.
- [ ] Run focused compression tests and commit.

### Task 9: Make marker failures safe

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`

- [ ] Write failing tests for HTML with no explicit `</body>`, a publisher-authored marker
      collision, and a corrupt cached marker.
- [ ] Record/validate a schema-bound seam location or use a collision-resistant marker contract.
- [ ] Cancel storage and fall back safely when the optimization cannot produce one seam.
- [ ] Run focused miss/hit assembly tests and commit.

### Task 10: Add operational observability and harden the harness

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Modify: `scripts/c2-local-test.sh`
- Modify: `.github/workflows/test.yml`

- [ ] Write failing tests for distinct backend-error versus not-found status and C2 response-state
      reporting.
- [ ] Preserve backend errors and emit bounded C2 status without exposing key material.
- [ ] Change the harness to operate on a temporary manifest and fail on missing/non-numeric probe
      output or empty response bodies.
- [ ] Test both cold and warm integrity and execute the generated scheduler payload contract.
- [ ] Add the ESI harness to CI where Viceroy prerequisites are available.
- [ ] Run shell syntax/static checks and commit.

### Task 11: Document configuration, semantics, and rollback

**Files:**

- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/superpowers/plans/2026-08-10-1009-esi-validation-spike.md`
- Modify: `docs/superpowers/specs/2026-08-11-1009-streaming-assembly-architecture.md`
- Modify: relevant #1009 findings documents

- [ ] Document that `esi` means Fastly C2 plus byte-seam assembly, not parser execution or final
      HTTP shared caching.
- [ ] Document `template_cache_vary`, cookie independence, freshness, metrics, purge, rollback
      ordering, and limitations on non-Fastly adapters.
- [ ] Close or supersede stale spike checkboxes and remove claims contradicted by the final code.
- [ ] Run docs format/build and commit.

### Task 12: Full verification

**Files:** none expected beyond fixes discovered by verification

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run all four adapter test aliases and the parity suite.
- [ ] Run all six clippy aliases.
- [ ] Build the Fastly release WASM.
- [ ] Run JS tests, build, and format under pinned Node 24.12.0.
- [ ] Run docs format/build.
- [ ] Run `scripts/c2-local-test.sh esi` and `inline` if the environment exposes the required
      local certificate store; otherwise report the exact environment blocker.
- [ ] Run `git diff --check`, inspect the merge graph, and confirm the worktree contains only
      intended changes.
