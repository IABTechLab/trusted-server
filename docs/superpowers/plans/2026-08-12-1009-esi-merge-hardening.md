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

> **Implementation status, 2026-08-12:** Tasks 1–12 are complete on the branch. The Viceroy
> harness passed in both modes after running outside the filesystem sandbox so it could read the
> macOS native-certificate keychain.

---

### Task 1: Merge live main and preserve auction contracts

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/integrations/gpt_diagnostics.rs`
- Modify: `crates/trusted-server-js/lib/src/core/types.ts`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Test: adjacent Rust and Vitest modules

- [x] Merge `origin/main` with `git merge --no-ff origin/main`.
- [x] Resolve the `AdBidsState`/`write_bids_to_state` conflict by building one structured map with
      `auction_id`, storing both map and script, and returning its delivered slot IDs.
- [x] Add/adjust tests proving ESI and inline retain `hb_auction_id`, APS renderer metadata, and
      delivered-winner attribution.
- [x] Run the focused Rust and GPT tests.
- [x] Complete the merge commit.

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

- [x] Update mode tests to specify only `inline` and `esi`; watch the old client-fill expectations
      fail or stop compiling.
- [x] Remove `ClientFill`, executable fragment serialization, assembler traits/registration, and
      the `esi` crate.
- [x] Update comments to call the production path byte-seam assembly.
- [x] Run focused configuration, publisher, and Fastly adapter tests.
- [x] Commit the scope cleanup.

### Task 3: Canonicalize and bound the template key

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [x] Write failing tests for absent versus empty `Vary`, repeated raw values, invalid configured
      names, punctuation-colliding purge URLs, changed origin host override, and changed creative
      configuration.
- [x] Replace string pairs with a typed canonical `Vary` value preserving presence and all bytes.
- [x] Hash a length-prefixed canonical key and hash the URL-specific surrogate key.
- [x] Include publisher origin identity and the complete template-shaping fingerprint.
- [x] Run focused key/configuration tests and commit.

### Task 4: Enforce request and origin cache semantics

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/Cargo.toml` if HTTP-date parsing needs a direct dependency

- [x] Write failing tests for response `max-age=0`, positive max age, repeated/malformed cache
      directives, `Age` exhaustion, expired/malformed `Expires`, missing freshness, invalid `Vary`,
      and request no-cache/no-store/range/conditional bypasses.
- [x] Add a typed cache eligibility result carrying the positive remaining TTL.
- [x] Parse relevant response directives fail-closed and cap, never extend, origin freshness.
- [x] Add request-side bypass classification before lookup.
- [x] Make unsupported/backend-failed cache lookups fall back to inline processing on non-Fastly
      adapters rather than buffering a cacheless ESI path.
- [x] Run focused eligibility tests and commit.

### Task 5: Move request collapse before the origin fetch

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`

- [x] Verify the reservation is acquired before origin work and the Fastly transaction contract
      blocks same-key waiters. Viceroy is single-threaded, so it cannot directly reproduce two
      truly concurrent cold requests.
- [x] Introduce a lookup outcome with an opaque insert reservation and explicit cancellation.
- [x] Implement Fastly `Transaction::lookup` before origin work and consume/cancel its obligation
      on every exit path.
- [x] Ensure invalid fresh entries become replaceable rather than causing repeated refetches.
- [x] Run focused Core Cache/Viceroy tests and commit.

### Task 6: Make privacy and policy-header parity final

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/response_privacy.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs` tests

- [x] Write failing tests for repeated CSP/CSP-Report-Only, omitted COOP/COEP/CORP/HSTS/Link,
      unknown cached header metadata, duplicate required metadata fields, and a late integration
      changing `Cache-Control` to public.
- [x] Capture all ordered values, expand the safe allowlist, and decode metadata strictly.
- [x] Replay with `append`, then apply the assembled-response privacy policy last.
- [x] Preserve and reassert private/no-store after request-filter effects in Fastly's final send.
- [x] Run focused header/privacy tests and commit.

### Task 7: Bypass shared templates for request-private diagnostics

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [x] Write a failing warm-cache test activated by diagnostics query and another by diagnostics
      cookie.
- [x] Make `requires_private_no_store()` a lookup/store disqualifier.
- [x] Verify ordinary diagnostics-disabled requests still hit the template cache.
- [x] Run focused diagnostics/template-cache tests and commit.

### Task 8: Re-encode assembled responses for the reader

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [x] Write failing cold/warm tests requiring gzip/br clients to receive a matching encoded body
      and proving reader encoding no longer partitions the stored template.
- [x] Keep the origin offer within the reader's supported codings so a response-gate bypass remains
      lossless, while decoding every stored template to identity.
- [x] Carry the selected response encoding separately from identity template metadata.
- [x] Encode buffered assembly after splicing and stream hit prefix/seam/suffix through one encoder.
- [x] Handle `identity;q=0` without serving an unacceptable representation.
- [x] Emit the correct `Vary: Accept-Encoding` response semantics after final encoding.
- [x] Run focused compression tests and commit.

### Task 9: Make marker failures safe

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`

- [x] Write failing tests for HTML with no explicit `</body>`, a publisher-authored marker
      collision, and a corrupt cached marker.
- [x] Record/validate a schema-bound seam location or use a collision-resistant marker contract.
- [x] Cancel storage and fall back safely when the optimization cannot produce one seam.
- [x] Run focused miss/hit assembly tests and commit.

### Task 10: Add operational observability and harden the harness

**Files:**

- Modify: `crates/trusted-server-core/src/platform/template_cache.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Modify: `scripts/template-cache-local-test.sh`
- Modify: `.github/workflows/test.yml`

- [x] Write failing tests for distinct backend-error versus not-found status and template-cache response-state
      reporting.
- [x] Preserve backend errors and emit bounded template-cache status without exposing key material.
- [x] Change the harness to operate on a temporary manifest and fail on missing/non-numeric probe
      output or empty response bodies.
- [x] Test both cold and warm integrity and execute the generated scheduler payload contract.
- [x] Add the ESI harness to CI where Viceroy prerequisites are available.
- [x] Run shell syntax/static checks and commit.

### Task 11: Document configuration, semantics, and rollback

**Files:**

- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/superpowers/archive/2026-08-10-1009-esi-validation-spike.md`
- Modify: `docs/superpowers/specs/2026-08-11-1009-streaming-assembly-architecture.md`
- Modify: relevant #1009 findings documents

- [x] Document that `esi` means Fastly shared template cache plus byte-seam assembly, not parser execution or final
      assembled-response caching.
- [x] Document `template_cache_vary`, cookie independence, freshness, metrics, purge, rollback
      ordering, and limitations on non-Fastly adapters.
- [x] Close or supersede stale spike checkboxes and remove claims contradicted by the final code.
- [x] Run docs format/build and commit.

### Task 12: Full verification

**Files:** none expected beyond fixes discovered by verification

- [x] Run `cargo fmt --all -- --check`.
- [x] Run all four adapter test aliases and the parity suite.
- [x] Run all six clippy aliases.
- [x] Build the Fastly release WASM.
- [x] Run JS tests, build, and format under pinned Node 24.12.0.
- [x] Run docs format/build.
- [x] Run `scripts/template-cache-local-test.sh esi` and `inline` if the environment exposes the required
      local certificate store; otherwise report the exact environment blocker.
- [x] Run `git diff --check`, inspect the merge graph, and confirm the worktree contains only
      intended changes.

### Task 13: Interpret Fastly Surrogate-Control conservatively

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/superpowers/specs/2026-08-12-1009-esi-merge-hardening-design.md`

- [x] Write a failing gate test using `Cache-Control: max-age=60` plus the observed publisher
      `Surrogate-Control` policy (`max-age=1200`, `stale-while-revalidate=21600`, and
      `stale-if-error=604800`).
- [x] Write failing tests proving the shorter standard/surrogate freshness wins, stale windows do
      not extend fresh reuse, restrictive directives are refused, and unknown, duplicate, or
      malformed directives fail closed.
- [x] Parse only Fastly's supported `max-age`, `stale-while-revalidate`, and `stale-if-error`
      directives; continue refusing every other vendor CDN policy field.
- [x] Keep request `Cache-Control: max-age=0` as an intentional template-cache bypass so reload preserves its
      revalidation semantics.
- [x] Run focused tests, `cargo test-fastly`, target-matched formatting/clippy, both local harness
      modes, and verify the observed publisher policy progresses from `miss-stored` to `hit` in
      the local Fastly runtime on an ordinary navigation.

### Task 14: Allow browser reloads to reuse a fresh ESI template

Task 14 supersedes Task 13's conservative request `max-age=0` bypass after end-to-end testing
proved that the template cache reuses only the neutral template and still creates a new private response and
auction.

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `docs/superpowers/specs/2026-08-12-1009-esi-merge-hardening-design.md`

- [x] Write a failing end-to-end test proving `Cache-Control: max-age=0` reruns the auction but
      does not refetch the reader-neutral publisher template.
- [x] Treat only a valid zero request max age as compatible with the template cache; continue bypassing positive
      or malformed constraints and every explicit revalidation directive.
- [x] Verify the focused tests, formatting, and Fastly clippy, then commit independently.

### Task 15: Make the ESI template-cache ceiling configurable

Task 15 supersedes Task 13's shorter-of-standard-and-surrogate rule. The final behavior follows
Fastly edge precedence while retaining restrictive directives as hard refusals.

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/template_cache.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/configuration.md`
- Modify: `docs/superpowers/specs/2026-08-12-1009-esi-merge-hardening-design.md`

- [x] Add failing configuration tests for the 60-second default, an explicit 1,200-second ceiling,
      zero, values above one day, and omission from serialized rollback-compatible config.
- [x] Add failing freshness tests proving Fastly precedence, age deduction, and the configured
      ceiling for the observed `Cache-Control: max-age=60` plus
      `Surrogate-Control: max-age=1200` response.
- [x] Implement `template_cache_max_age_seconds` under `[creative_opportunities]` and thread its
      resolved duration into template-cache eligibility.
- [x] Remove the Fastly adapter's second hard-coded 60-second cap; the already-authorized
      per-entry max age becomes the sole insertion lifetime.
- [x] Update the example and operator guide, without editing the tracked deployment
      `fastly.toml`.
- [x] Run focused red/green tests, full adapter tests and clippy gates, documentation checks, and
      inspect the final diff with `fastly.toml` excluded.
