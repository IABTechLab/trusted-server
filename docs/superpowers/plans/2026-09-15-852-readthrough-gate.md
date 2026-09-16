# Issue #852 implementation plan — part 3 of 3: the readthrough gate

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop forcing every ad-serving pageview to origin, and make the resulting sharing
decision deliberate for the whole request population rather than half of it.

**Architecture:** One condition, at two call sites. Readthrough is enabled by **omitting**
`set_pass`, not by adding a TTL override. Safety rests on probe-verified operator preconditions,
because no response-side hook is reachable on this adapter.

**Tech Stack:** Rust 2024, `wasm32-wasip1` via Viceroy, `fastly` 0.12.1 `CacheOverride`.

**Spec:** `docs/superpowers/specs/2026-09-15-852-template-and-origin-caching-design.md` — read
"Origin readthrough" end to end before starting. Not just the gate: the mechanism section and the
response-side gap are the parts that constrain the implementation.

**Part 3 of 3.** Parts 1 and 2 must be complete. This part is inert without part 1's
`origin_response_is_shareable` binding, and unsafe to enable without part 2's probe.

---

## Read this before writing any code

**This is the only change in #852 with new runtime blast radius.** Everything else is
instrumentation and tooling. Ask for this commit to be reviewed on its own.

Three things are settled and must not be re-litigated mid-implementation:

1. **Do not add a TTL override.** `Request::set_ttl` (`fastly-0.12.1/src/http/request.rs:2381`)
   "overrides any previous `Request::set_pass` call and sets the `pass` behavior to `false`", and
   overrides the origin's `Cache-Control` including `private` and `no-store`. Adding it would turn
   the hazard this work exists to close into a first-class API. `set_ttl(0)` does not help either
   — it caches with zero TTL, it does not bypass.
2. **`after_send` / `CandidateResponse` is unreachable.** Viceroy 0.17 stubs the HTTP Cache ABI
   and the SDK converts that into a send error, so setting `after_send` makes every publisher
   origin fetch fail under `fastly compute serve`, `cargo test-fastly`, and the parity suite.
   Recorded at `adapter-fastly/src/template_cache.rs:6-15`.
3. **`set_pass` and `set_surrogate_key` are mutually exclusive, and order-dependent.**
   `set_surrogate_key` (`request.rs:2462`) carries the same override note. Calling it after
   `set_pass(true)` reverses the bypass. The platform layer must make that unrepresentable.

---

## File structure

| File                                                       | Responsibility   | Change                                                     |
| ---------------------------------------------------------- | ---------------- | ---------------------------------------------------------- |
| `crates/trusted-server-core/src/publisher.rs`              | Bypass decision  | The condition at `:4415` and `:4716`                       |
| `crates/trusted-server-core/src/platform/http.rs`          | Platform request | Replace the `bypass_cache` bool with a cache-intent enum   |
| `crates/trusted-server-adapter-fastly/src/platform.rs:445` | Fastly mapping   | Apply the enum; attach `ts-origin` on the shareable branch |
| `docs/guide/configuration.md`                              | Runbook          | Enablement, rollback, preconditions                        |

---

## Task 1: Make the pass/surrogate-key conflict unrepresentable

**Files:** `crates/trusted-server-core/src/platform/http.rs:16-37`, `adapter-fastly/src/platform.rs:445`

Today `PlatformHttpRequest` carries `bypass_cache: bool`. Once the shareable branch also attaches
a surrogate key, two booleans could express "bypass **and** key", which on Fastly silently means
"do not bypass". Encode the intent instead.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cache_intent_cannot_request_bypass_and_a_surrogate_key_at_once() {
    let bypass = PlatformCacheIntent::Bypass;
    let shared = PlatformCacheIntent::Shared { surrogate_key: "ts-origin".to_owned() };

    assert!(bypass.surrogate_key().is_none());
    assert_eq!(shared.surrogate_key(), Some("ts-origin"));
    assert!(!shared.is_bypass());
    assert!(bypass.is_bypass());
}
```

- [ ] **Step 2: Run to verify it fails.** `cargo test-fastly -- platform::http::tests::cache_intent`

- [ ] **Step 3: Implement**

```rust
/// What the caller wants the platform's intermediary cache to do with this request.
///
/// One enum rather than two flags because on Fastly they are not independent:
/// `set_surrogate_key` and `set_ttl` each reverse a prior `set_pass(true)`
/// (`fastly-0.12.1/src/http/request.rs:2462`, `:2381`). Two booleans could express a
/// combination that silently means the opposite of what it reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformCacheIntent {
    /// Let the platform apply its default behavior, honoring origin freshness.
    Default,
    /// Do not use the intermediary cache for this request.
    Bypass,
    /// Allow caching, tagged for purge.
    Shared { surrogate_key: String },
}
```

Replace `bypass_cache: bool` with `cache_intent: PlatformCacheIntent`. Keep
`with_cache_bypass()` as a builder that sets `Bypass` so existing call sites need no change, and
add `with_shared_cache(surrogate_key)`.

In `apply_fastly_cache_bypass` (`adapter-fastly/src/platform.rs:445`), rename to
`apply_fastly_cache_intent` and match: `Bypass` → `set_pass(true)`; `Shared` →
`set_surrogate_key(...)` and **no** `set_pass`; `Default` → nothing.

Other adapters ignore the intent as they ignore `bypass_cache` today — Spin hard-rejects only
`stream_response` (`adapter-spin/src/platform.rs:304`), so nothing there needs to change.

- [ ] **Step 4: Run to verify it passes**, plus `cargo check-fastly && cargo check-axum && cargo check-cloudflare && cargo check-spin`.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/http.rs crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Model platform cache intent as one enum rather than two flags

On Fastly, setting a surrogate key or a TTL reverses a prior set_pass. Two
booleans could express a combination that means the opposite of how it reads."
```

## Task 2: Gate the bypass on shareability, at both sites

**Files:** `crates/trusted-server-core/src/publisher.rs:4415`, `:4716`

**Both sites change.** They are alternative paths for the same fetch: `:4415` populates
`pending_origin` inside the EC-preload fan-out block, and `:4716` is the `else` branch of
`if let Some(pending) = pending_origin` (`:4703`). Changing only one makes readthrough eligibility
depend on whether EC preload fired — and `should_preload_ec_snapshot` (`:2955`) is
`is_navigation && is_get && has_ec_id && has_kv`, much of the population this exists for.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn ineligible_requests_bypass_the_platform_cache() {
    // cookie-bearing, Authorization-bearing, non-GET, request_requires_origin
    // each assert the recorded intent is Bypass
}

#[test]
fn eligible_requests_do_not_bypass() {
    // cookieless GET navigation asserts the recorded intent is not Bypass
}

#[test]
fn preload_and_non_preload_paths_agree_on_cache_intent() {
    // same inputs through both sites produce the same intent
}

#[test]
fn ineligible_bot_traffic_now_bypasses_where_it_previously_did_not() {
    // the tightening half of the change
}
```

`recorded_cache_bypass_flags()` (`platform/test_support.rs:450`) captures what is needed; widen it
to record the intent rather than a bool.

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Implement**

```rust
if !origin_response_is_shareable {
    platform_request = platform_request.with_cache_bypass();
}
```

at both `:4415` and `:4716`. `should_run_ad_stack` is **gone** from the condition.

- [ ] **Step 4: Run to verify they pass**, then the whole module: `cargo test-fastly`.

- [ ] **Step 5: State the two-sided effect in the commit**

This is **not** "strictly more conservative", and the commit message must not say so:

- **Tightening** for cookie-bearing, `Authorization`-bearing, or otherwise ineligible bot and
  prefetch traffic, which now gets `set_pass` where today it does not.
- **Widening** for cookieless traffic: eligible ad-serving requests begin reading objects that
  cookieless bots and prefetchers store. Today those objects are written and read only by
  non-ad-stack requests.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Gate the origin cache bypass on shareability rather than the ad stack

should_run_ad_stack means 'this page serves ads', which is unrelated to
whether the origin response may be shared, and left every non-ad-stack
request sharing the readthrough cache with no eligibility check at all.

Tightening for ineligible bot and prefetch traffic, which now bypasses.
Widening for cookieless traffic, which begins reading objects those requests
store. The probe's blocking verdicts are the control for the widening."
```

## Task 3: Attach the `ts-origin` surrogate key on the shareable branch

**Files:** `crates/trusted-server-core/src/publisher.rs:4415`, `:4716`

**Gated on the staging verdict from part 2's final verification.** If `set_surrogate_key` does not
tag readthrough objects on a real service, skip this task and take Task 4's fallback wording.

The key may be applied **only** on the shareable branch. Stamping it unconditionally would
disable the bypass for every request, including disqualified ones — see the mutual-exclusion note
at the top.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn shareable_requests_carry_a_purgeable_origin_key() {
    // eligible request: intent is Shared with both the global and per-URL origin keys
}

#[test]
fn ineligible_requests_carry_no_surrogate_key() {
    // the mutual-exclusion invariant, asserted at the call site not just the type
}
```

- [ ] **Step 2–4:** run, implement `with_shared_cache(...)` on the eligible branch using
      `ts-origin` plus a per-URL variant from part 2's `reader_url_surrogate_key`, run again.

- [ ] **Step 5:** extend the purge endpoint and CLI from part 2 to purge `ts-origin` alongside
      `ts-template`, so `--all` means both caches. Update their tests.

- [ ] **Step 6: Commit**

## Task 4: Runbook and preconditions

**Files:** `docs/guide/configuration.md`

- [ ] **Step 1: Write the enablement procedure**

1. Run `ts origin probe-shareability --url <representative URLs>`.
2. **Every axis and every verdict must pass.** Do not enable on a partial pass. The probe is the
   only control — the gate is decided before the origin responds, so none of the template cache's
   response-side refusals apply to this path.
3. Set `origin_is_cookie_independent = true`.
4. Watch the `origin_cache_shareable` breakdown from part 1. (`template_cache_bypass_reason` was
   designed alongside it and cut as out of scope for #852 — do not reach for it here.)
5. Confirm hit rate before widening to more URLs.

- [ ] **Step 2: Write the rollback procedure, honestly**

Two levers, in order of speed:

1. **Config:** set `origin_is_cookie_independent = false`. Takes effect on the next request; no
   deploy. This is the real rollback.
2. **Purge:** `ts cache purge --all` or the admin endpoint.

Then state what purge covers, matching the staging verdict:

- If Task 3 landed: both template-cache and readthrough objects.
- If it did not: template-cache objects **only**. Readthrough rollback is the config flag plus
  waiting out the origin TTL. Say this plainly — do not ship a rollback step that does not affect
  the cache being rolled back.

- [ ] **Step 3: State the residual risk**

An operator who enables this against an unverified origin can cross-serve, including session
fixation via a cached `Set-Cookie`. This is a weaker guarantee than the template cache's, and the
docs must say so rather than implying parity.

- [ ] **Step 4: Format and commit**

```bash
cd docs && ./node_modules/.bin/prettier --check guide/configuration.md
```

---

## Final verification for the whole PR

- [ ] **Full gate set**, as in part 2, plus `./scripts/template-cache-local-test.sh purge`.

- [ ] **Write the refusal-by-refusal comparison into the PR description**

For each `template_cache_ttl` refusal, state whether the readthrough path covers it and how:

| Refusal               | Anchor              | Covered on readthrough?                                 |
| --------------------- | ------------------- | ------------------------------------------------------- |
| `NoPositiveFreshness` | `publisher.rs:6100` | By probe verdict only                                   |
| `OriginSetCookie`     | `:6147`             | By probe verdict only                                   |
| `CspNonce`            | `:6117`             | By probe verdict only                                   |
| `OriginNotShareable`  | `:6153`             | Platform honors origin headers                          |
| `NonOkStatus`         | `:6190`             | Platform honors status                                  |
| `NotHtml`             | `:6198`             | Not applicable — readthrough caches any type per origin |
| `VaryNotCovered`      | `:6184`             | Platform keys on origin `Vary`                          |

Anything in the "probe verdict only" rows is an accepted risk, and the PR should name it as such
rather than leaving a reviewer to derive it.

- [ ] **Confirm the gate is inert by default**

With `origin_is_cookie_independent` unset, `cookie_disqualifies` is true for every cookie-bearing
request, so `origin_response_is_shareable` is false and the bypass behaves as it does today for
every repeat visitor. Prove it with a test, not by reasoning — it is the claim that makes this
change safe to merge ahead of any operator decision.

- [ ] **Deploy ordering**

The Tinybird datasource migration from part 1 must reach Tinybird **before** this code deploys, or
every emitted telemetry row is quarantined. Record the confirmation on the PR.
