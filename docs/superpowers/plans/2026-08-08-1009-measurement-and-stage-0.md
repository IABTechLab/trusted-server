# #1009 Measurement and Stage 0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn off the redundant origin cache bypass that the spec identifies as the
actual TTFB cost, behind an operator flag, and establish the measurement baseline that
later work is compared against.

> **This plan does not close #1009.** It contains no ESI arm and no client-fill arm, so
> completing it cannot answer whether ESI separates cacheable content from per-user
> state. It is a **supporting optimisation and the experimental control** for
> [the ESI validation spike](./2026-08-10-1009-esi-validation-spike.md), which is where
> #1009 is actually decided. Scoped and framed this way after external review on
> 2026-08-10.

**Architecture:** Two investigation tasks that produce recorded findings and no code; one
code task that adds a config-gated timing log and makes the cache bypass operator-
controlled; and one config change that flips it, gated on the first investigation.
Nothing here touches the auction, the `</body>` hold, or bid delivery — those are
Stages 1–2 in the spec and are explicitly out of scope.

**Tech Stack:** Rust 2024 edition, `wasm32-wasip1`, Fastly Compute, `web_time::Instant`
for wasm-safe timing, `log` for instrumentation, Viceroy for adapter tests.

**Spec:** `docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md`
(§3 Steps A/B/C and §4 Stage 0). Read §4 and §5 before starting Task 5.

**Before pushing, run three gates, not two:**

```bash
cd docs && npm run format && npm run build && cd ..
python3 scripts/docs-invariants.py
```

`npm run build` is not optional — `format` passes on documents with dead links, and that
shipped a broken docs build on this branch once already. `docs-invariants.py` catches
cross-document contradictions, which neither of the other two can see.

**Two prettier gotchas, both hit while writing this plan.** CI gate 7
(`cd docs && npm run format`) runs `prettier --check` over all of `docs/`, so both bite.

1. **Not idempotent on embedded markdown fences.** The first `--write` reformats the
   outer document and the embedded ` ```markdown ` block only settles on a second pass.
   If `--check` still warns immediately after a `--write`, run `--write` again before
   concluding anything is wrong.
2. **It mangles bare `snake_case` identifiers inside fences**, reading the underscores as
   emphasis and rewriting `origin_fetch_ms` to `origin*fetch_ms`. **Always wrap
   identifiers in backticks**, including inside fenced blocks and table cells.

---

## Background an implementer needs

Trusted Server proxies a publisher's origin, rewrites the HTML at the edge to inject ad
slot definitions and a JS bundle, and runs a server-side ad auction. For requests that
are eligible for that ad stack, `publisher.rs` currently does three things to the origin
request and response that together make the page uncacheable:

1. strips conditional and range headers so the origin must return a full body,
2. sets a **cache bypass** so the Fastly read-through cache is skipped entirely, and
3. strips every cacheability header from the response.

The spec establishes that (2) is redundant given (1) — by the time the request reaches
the cache it is already unconditional, so a cache HIT returns a full body anyway — and
that (2) is the dominant cost. This plan makes (2) operator-controlled and then turns it
off, after first confirming that is safe.

**Why it might not be safe:** RSC (React Server Component) requests and ordinary HTML
navigations share the same URL and are distinguished only by request headers. RSC
requests are not classified as navigations, so they already flow through the cache while
HTML navigations bypass it. Removing the bypass puts both under one cache key. If the
origin does not declare `Vary` for those headers, the cache could serve one
representation in response to a request for the other. Task 1 checks this.

**Terms:** _POP_ = Fastly edge point of presence. _shield_ = a designated POP that
backs other POPs. _read-through cache_ = Fastly's cache on the backend request path.
_bypass / `Pass`_ = skip that cache.

---

## File structure

| File                                                             | Responsibility in this plan                                                        |
| ---------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md` | **Create.** Recorded output of Tasks 1–2. Gates Task 5.                            |
| `crates/trusted-server-core/src/publisher.rs`                    | **Modify.** Timing log and the bypass flag (Task 3); tests (Task 5).               |
| `crates/trusted-server-core/src/settings.rs`                     | **Modify.** `publisher.bypass_origin_cache` and `debug.publisher_timing` (Task 3). |
| `trusted-server.example.toml`                                    | **Modify.** Document the new key (Task 5).                                         |

No new modules. No adapter changes: the `bypass_cache` platform capability and its
per-adapter mappings stay in place and keep their tests — the publisher-path call site
becomes operator-controlled rather than unconditional.

## Task order and dependencies

Only one edge is real. Do not serialize the rest.

```
Task 1 (origin Vary check) ──────┬──> Task 2 (appends to the findings file Task 1 creates)
                                 │
                                 ├──> Task 5 (flip the flag)
Task 3 (instrumentation + flag) ─┘
```

**Task 1 is externally blocked.** It needs the publisher origin hostname, which lives in
the operator's gitignored `trusted-server.toml`. Arrange access before starting, or the
plan stalls on its first step.

Task 3 is independent and can start immediately. Task 2 only needs Task 1 far enough to
have created the findings document. Task 5 needs Task 1's verdict **and** Task 3's config
flag to exist.

---

## Task 1: Step A — origin `Vary` check

**Files:**

- Create: `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md`

This task is an investigation. It writes no code and gates Task 5.

- [ ] **Step 1: Get the origin URL**

The publisher origin is operator config, not in the repo. Read it from the deployed
service config or ask the operator. Do **not** hardcode it into any committed file — the
findings document records the _result_, not the hostname.

```bash
# The key is `publisher.origin_url` in the operator's trusted-server.toml
# (gitignored). Confirm the value before proceeding.
```

- [ ] **Step 2: Request the HTML representation and capture `Vary`**

```bash
curl -sS -D - -o /dev/null "https://<origin-host>/<an-article-path>" \
  -H 'Sec-Fetch-Dest: document' \
  -H 'Accept: text/html'
```

Expected: response headers. Record whether a `Vary` header is present and its value.

- [ ] **Step 3: Request the RSC representation at the same URL**

```bash
curl -sS -D - -o /dev/null "https://<origin-host>/<the-same-article-path>" \
  -H 'RSC: 1' \
  -H 'Accept: text/x-component'
```

Expected: a different `Content-Type` (`text/x-component`) than Step 2, proving the two
representations share a URL. Record `Vary` again.

- [ ] **Step 4: Probe the `Next-Router-*` headers**

Do not skip this. The PASS criterion below names these headers, and an implementer who
tests only HTML and `RSC` can record a PASS that is wrong — which routes to Task 5a, the
one outcome this plan calls dangerous.

```bash
for H in 'Next-Router-Prefetch: 1' 'Next-Router-State-Tree: %5B%22%22%5D'; do
  echo "--- $H"
  curl -sS -D - -o /dev/null "https://<origin-host>/<the-same-article-path>" \
    -H 'RSC: 1' -H "$H" \
    | grep -iE '^(vary|content-type|content-length|cache-control|set-cookie):'
done
```

Compare `Content-Type` and `Content-Length` against the plain `RSC: 1` request from
Step 3. If either differs, the origin varies on that header and `Vary` must name it.

Capture `Cache-Control` and `Set-Cookie` on every request in this task, not just this
one — see Step 5.

- [ ] **Step 5: Probe cookie personalization — the bigger hole**

The representation check above covers RSC-vs-HTML. It does **not** cover the larger
class: TS forwards client cookies to origin unchanged, so any cookie-personalized HTML
(logged-in state, paywall meter, publisher-side A/B assignment) becomes cross-servable
once the cache is on.

**Do not compare body hashes.** Verified on the live origin: this page regenerates
~170 ad-slot container IDs as fresh 32-hex UUIDs on every request, so three requests give
three different hashes with byte-identical lengths, cookie or not. A hash comparison
reports a false FAIL every time.

Normalize per-request identifiers, establish the no-cookie baseline drift first, then ask
whether the cookie arm differs by _more_ than that baseline:

```bash
ORIGIN="https://<origin-host>"; HOSTH="Host: <origin_host_header_override>"
norm() { sed -E 's/[0-9a-f]{32}/UUID/g' "$1"; }

for n in a b; do
  curl -sS "$ORIGIN/<path>" -H "$HOSTH" \
    -H 'Sec-Fetch-Dest: document' -H 'Accept: text/html' > "nc_$n.html"
done
curl -sS "$ORIGIN/<path>" -H "$HOSTH" \
  -H 'Sec-Fetch-Dest: document' -H 'Accept: text/html' \
  -H 'Cookie: <a-real-session-cookie>' > ck.html

echo "baseline drift:  $(diff <(norm nc_a.html) <(norm nc_b.html) | grep -c '^[<>]')"
echo "with cookie:     $(diff <(norm nc_a.html) <(norm ck.html)   | grep -c '^[<>]')"
diff <(norm nc_a.html) <(norm ck.html) | head -20
```

Send the `Host` override — the origin is a shared vhost and will not return the right
document without it. Read it from `publisher.origin_host_header_override`.

**Step A has been run once and returned a PROVISIONAL PASS**, which is **not** sufficient
to flip the flag. See [the findings](./2026-08-08-1009-measurement-findings.md) for the
five untested conditions. Complete them and record a `FINAL PASS` before Task 5.

Three failure shapes, any of which blocks Stage 0 independently of the `Vary` verdict:

- Bodies differ by cookie **and** `Vary` does not name `Cookie` → cross-serving of
  personalized HTML.
- Origin emits `Set-Cookie` alongside a shared-cacheable `Cache-Control` → the cache can
  replay one visitor's cookie to the next. TS's privacy net does not help; it downgrades
  **TS's** response, after the cache has already stored the origin's.
- The deployment is `Authorization`-gated (as #1009 describes) and authorized responses
  are cacheable → same problem, different header.

- [ ] **Step 6: Request with the experiment header, if the operator uses one**

Repeat Step 2 with the publisher's experiment header set to two different values.
Record whether the bodies differ and whether `Vary` names that header.

- [ ] **Step 7: Record the finding**

Create `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md`:

```markdown
# #1009 measurement findings

## Step A — origin `Vary` declaration

**Date:** <date> · **Checked by:** <name>

| Representation        | `Content-Type` returned | `Content-Length` | `Vary` present? | `Vary` value |
| --------------------- | ----------------------- | ---------------- | --------------- | ------------ |
| HTML navigation       |                         |                  |                 |              |
| RSC                   |                         |                  |                 |              |
| RSC + `Next-Router-*` |                         |                  |                 |              |
| Experiment variant    |                         |                  |                 |              |

**Cookie / auth exposure:** bodies differ by cookie? `Vary: Cookie` present? origin
`Set-Cookie` on a shared-cacheable response? `Authorization`-gated responses cacheable?

**Verdict:** FINAL PASS / PROVISIONAL PASS / FAIL

`FINAL PASS` = `Vary` names every request header the origin varies on (`RSC`, any
`Next-Router-*` or experiment header whose value changed the body, **and `Cookie` if
bodies differ by cookie**), no `Set-Cookie` rides a shared-cacheable response, **and** all
five conditions in Task 5's gate are recorded — a real authenticated session cookie, Basic
Auth through TS, the experiment variant, representative routes, and cached-hit
slot/render attribution.

`PROVISIONAL PASS` = the `Vary` and cookie checks hold, but one or more of those five is
untested. **Not a release gate.** A first pass lands here.

`FAIL` = any `Vary` or `Set-Cookie` criterion is unmet.

**Consequence:** `FINAL PASS` → Task 5a (flip the flag). `PROVISIONAL PASS` → close the
gaps before Task 5 starts. `FAIL` → Task 5b (cache-key discriminator). See spec §4.

**A FAIL is also a live production defect, not only a Stage 0 blocker.** RSC fetches are
not navigations, so they never set the bypass and **already transit the read-through
cache today**. If the origin varies undeclared on `Next-Router-*`, TS is cross-serving RSC
variants in production right now. File it immediately rather than deferring with Task 5b.
```

- [ ] **Step 8: Commit**

CI gate 7 runs `prettier --check` across all of `docs/`, so format the findings file
before staging it — a filled-in markdown table will not be prettier-clean by hand.

```bash
cd docs && npx prettier --write superpowers/plans/2026-08-08-1009-measurement-findings.md && cd ..
git add docs/superpowers/plans/2026-08-08-1009-measurement-findings.md
git commit -m "Record origin Vary findings for #1009 Stage 0 gate"
```

---

## Task 2: Step B — what consumes TS's own response headers

**Files:**

- Modify: `docs/superpowers/plans/2026-08-08-1009-measurement-findings.md` — **created by
  Task 1 Step 6.** If Task 1 has not reached that step, create the file with just its
  `# #1009 measurement findings` heading rather than blocking.

Investigation. Determines whether the spec's Stage 3b has a consumer. Does not gate
Task 5, but it appends to Task 1's findings document — do not run the two concurrently
against that file.

- [ ] **Step 1: Pick a path that already emits shared-cache headers**

`serve_static_with_etag` emits `public, max-age=300, s-maxage=300` plus
`Surrogate-Control` — see `crates/trusted-server-core/src/http_util.rs:294-311`. It backs
the `/static/tsjs=<hash>` bundle route (`publisher.rs:303`, `:322`). Use that URL against
the deployed service.

- [ ] **Step 2: Request it twice and inspect for cache markers**

```bash
URL="https://<ts-host>/static/tsjs=<hash>"
curl -sS -D - -o /dev/null "$URL" | grep -iE 'x-cache|age:|x-served-by|hit-state'
sleep 2
curl -sS -D - -o /dev/null "$URL" | grep -iE 'x-cache|age:|x-served-by|hit-state'
```

Expected on the second request: if a cache sits in front of the Compute service, an `age`
greater than zero or an `x-cache` containing `HIT`.

**The probe above is weak evidence** — absence of `age` is equally consistent with "no
cache" and "cold cache". **The topology check below is the actual answer; run it first and
skip the probe if it is conclusive.**

```bash
fastly service list
fastly service-version list --service-id <id>
# Look for a Delivery service fronting the Compute service, and for shielding
# configured on the service rather than only on the origin backend.
```

A Compute service with no Delivery service in front and no fronting shield does not have
its own output cached — that is the configuration the spec assumes, and this step exists
to confirm or refute it rather than to leave it assumed.

**While you have the service open, answer a second question that matters more than this
task does:** is the _publisher backend_ shielded on the TS service?

```bash
fastly backend list --service-id <id> --version active
# Look for a shield on the publisher origin backend.
```

#1009's entire off-TS advantage came from a **shield** HIT, not a POP HIT. Whether
Stage 0 recovers a shield HIT or only a single-POP HIT changes the size of the win
materially, and nothing else in this plan establishes it.

- [ ] **Step 3: Record the finding**

Append to the findings document:

```markdown
## Step B — consumers of TS's own response headers

**Verdict:** SHARED CACHE PRESENT / NO SHARED CACHE

**Evidence:** <paste the two header captures>

**Consequence:** NO SHARED CACHE → spec Stage 3b is inert until a topology change;
deprioritize it and ship only Stage 3a (browser caching). SHARED CACHE PRESENT →
Stage 3b gains a consumer AND the per-user `x-geo-*` header leak in spec §7 becomes an
active privacy exposure rather than a theoretical one. Escalate immediately in that case.
```

- [ ] **Step 4: Commit**

```bash
cd docs && npx prettier --write superpowers/plans/2026-08-08-1009-measurement-findings.md && cd ..
git add docs/superpowers/plans/2026-08-08-1009-measurement-findings.md
git commit -m "Record response-header cache consumer findings for #1009"
```

---

## Task 3: Step C — origin fetch timing, and the bypass flag

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/settings.rs` — `publisher.bypass_origin_cache`,
  `default_bypass_origin_cache`, `debug.publisher_timing`, the `Publisher` `Default` impl,
  eight test literals, and the `origin_host` doctest
- Modify: `crates/trusted-server-core/src/test_support.rs` (log-capture helper)
- Test: inside `mod ssat_cache_policy_tests` at `crates/trusted-server-core/src/publisher.rs:4541`

**Use `web_time::Instant`, not `std::time::Instant`** — the workspace targets
`wasm32-wasip1` and `web_time` is the wasm-safe clock already used at
`crates/trusted-server-core/src/auction/orchestrator.rs:7`.

### Two timings, and why these two

Measure **`hold_wait_ms`** and **`origin_fetch_ms`**. Not the rewrite.

`hold_wait_ms` is the decision. The hold's cost is literally the duration of one
`.await` — `collect_stream_auction` at `publisher.rs:793`, plus the two EOF variants in
`hold_finish_ready_segments` (`:869`) and `hold_finish_tail_segments` (`:896`). Two
`Instant`s around those calls answer "does the hold block?" directly, instead of
inferring it by comparing origin fetch against auction duration.

`origin_fetch_ms` is attribution — how much of any win Stage 0 can claim.

`rewrite_ms` decides nothing. Step C's verdict compares origin fetch against auction
collect, and the ceiling argument in spec §6.4 is structural — it needs no number.
Measuring the rewrite would mean instrumenting two finalizers
(`buffer_publisher_response_async` at `publisher.rs:1114`, and the
`async_stream::try_stream!` block at `publisher.rs:1286`), working around moves out of
`params` inside that block, and finding a correlation key that does not exist —
`OwnedProcessResponseParams` (`publisher.rs:1065-1087`) has no `request_path`, and adding
one means touching all 26 construction sites.

None of that buys a decision. Skip it. If a rewrite figure is later wanted to set a
target, add it as a separate follow-on once the verdict is known.

**Why a log line and not `Server-Timing`:** for `origin_fetch_ms` alone a response header
would in fact work — the value is known before headers commit. A log line is still
preferred because it is server-side (no dependence on a browser harness to collect it),
`log` is this project's instrumentation crate per `CLAUDE.md`, and the auction path
already measures itself the same way. The spec previously claimed `Server-Timing` cannot
work at all; that overbroad claim has already been corrected there.

### Log volume — gate it

The line sits after the origin send, so it fires for every publisher request that reaches
origin — tagged `ad_stack=false` for ineligible ones, not only for eligible navigations.
That is more useful for comparison and more log spend, and the instrumentation is
temporary either way. Gate it behind the existing debug surface rather than
emitting unconditionally: add a `#[serde(default)] pub publisher_timing: bool` to
`DebugConfig` (`crates/trusted-server-core/src/settings.rs:1872`), following
`ja4_endpoint_enabled` and `auction_html_comment` alongside it. Default `false`; enable
via `ts config push` for the measurement window, then disable.

This also means the Step 1 test must set that flag in its settings fixture.

The split is also what makes the Step 1 test achievable — `run_with_slots`
(`publisher.rs:4769`) invokes only `handle_publisher_request` and never drives either
finalizer, so a test asserting on a combined line could never pass.

**What `origin_fetch_ms` actually measures.** `publisher.rs:2863-2865` sets
`.with_stream_response()` when the adapter supports it, so on Fastly `send()` returns at
response _headers_, not after the body downloads. `origin_fetch_ms` is therefore **origin
TTFB**, not full download time. Name it that way in the findings document. It is still
the correct before/after signal for Stage 0 — the bypass affects whether the request hits
a cache at all — but when comparing against auction `total_time_ms` in Step 9, compare
like with like and say which quantity each column holds.

- [ ] **Step 1: Write the failing test**

**Placement matters.** Add the test **inside `mod ssat_cache_policy_tests`**
(`publisher.rs:4541`), not the outer `mod tests` (`:4035`). Every helper it uses is
private to that nested module: `settings_with_enabled_auction_and_creative_opportunities`
(`:4684`), `article_slot` (`:4721`), `conditional_navigation_request` (`:4740`),
`queue_cacheable_html_response` (`:4752`), `run_with_slots` (`:4769`). Placed in the outer
module it will not resolve — and because two _other_ `article_slot` functions exist
(`:9593`, `:10276`) returning a different type, the failure surfaces as a confusing type
error rather than a missing-name error.

**First, add the log-capture helper.** `crates/trusted-server-core/src/test_support.rs`
has none. Note its shape: the whole file is `#[cfg(test)] pub mod tests { … }`, so the
path is `crate::test_support::tests::capture_logs`, not `crate::test_support::capture_logs`
— see existing consumers at `auth.rs:103` and `config_payload.rs:48`.

Two constraints the helper must respect or the test fails for unrelated reasons:

- `log::set_boxed_logger` succeeds **once per process**. Install via a `OnceLock`/`Once`
  and have `capture_logs()` return a guard that clears and then reads a shared buffer.
- Call `log::set_max_level(log::LevelFilter::Info)` or higher, or `log::info!` is filtered
  out before it reaches the logger.
- **Do not have the guard hold the buffer's own `Mutex`.** The test body runs code that
  calls `log::info!` on the same thread, and the logger must lock that same mutex to
  append — `std::sync::Mutex` is not reentrant, so this **hangs** rather than failing.
  Use two locks: a separate process-wide serialization mutex held by the guard, and the
  buffer's own mutex taken and released per line by the logger.
- The buffer is process-global and every other concurrently-running `trusted-server-core`
  test logs into it, so a `got: {captured}` diagnostic will be large. Assert with
  `contains`, not equality.
- `log::set_max_level` is global for the test binary. Setting it to `Info` is fine, but it
  affects every test in the process.

```rust
#[tokio::test]
async fn eligible_navigation_logs_origin_fetch_duration() {
    // Arrange
    let logs = crate::test_support::tests::capture_logs();
    let mut settings = settings_with_enabled_auction_and_creative_opportunities();
    // The log line is gated; without this the assertions below can never pass.
    settings.debug.publisher_timing = true;
    let stub = Arc::new(StubHttpClient::new());
    queue_cacheable_html_response(&stub);
    let services = build_services_with_http_client(
        Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
    );
    let slots = [article_slot()];

    // Act
    let _ = run_with_slots(&settings, &services, &slots, conditional_navigation_request()).await;

    // Assert
    let captured = logs.contents();
    assert!(
        captured.contains("publisher_timing"),
        "eligible navigation should emit a publisher_timing log line, got: {captured}"
    );
    assert!(
        captured.contains("origin_fetch_ms="),
        "publisher_timing should record origin_fetch_ms, got: {captured}"
    );
}
```

This test deliberately asserts only on the `publisher_timing` line. `run_with_slots` never
drives a finalizer, so `publisher_rewrite` is out of its reach — cover that separately if
at all, rather than contorting this test.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p trusted-server-core --target aarch64-apple-darwin \
  eligible_navigation_logs_origin_fetch_duration -- --nocapture
```

Expected: FAIL — no `publisher_timing` in the captured logs. (Substitute your host
triple; core tests run natively for fast iteration. The Viceroy run comes in Step 6.)

- [ ] **Step 3: Time the origin fetch**

In `publisher.rs`, at the top with the other imports, add:

```rust
use web_time::Instant;
```

Then wrap the origin send. The current code is at `publisher.rs:2870`:

```rust
let mut response = match services.http_client().send(platform_request).await {
```

Change it to:

```rust
let origin_fetch_start = Instant::now();
let mut response = match services.http_client().send(platform_request).await {
```

and immediately after the `match` completes (after the existing `};` that closes it,
before the existing `log::debug!("Publisher origin response received: ...")` at `:2888`):

```rust
let origin_fetch_ms = u64::try_from(origin_fetch_start.elapsed().as_millis()).unwrap_or(u64::MAX);
```

**Make the bypass config-driven in the same change.** This is what lets Stage 0 ship as a
config flip rather than a second deploy — see Task 5. Replace the block at
`publisher.rs:2866-2868`:

```rust
// Single source of truth for the request and the log line below. Operator-
// controlled so the read-through cache can be re-enabled without a release;
// see docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md §4.
let cache_bypass = should_run_ad_stack && settings.publisher.bypass_origin_cache;
if cache_bypass {
    platform_request = platform_request.with_cache_bypass();
}
```

Add the setting to `Publisher` in `crates/trusted-server-core/src/settings.rs:29`,
**defaulting to today's behaviour** so this change is a no-op until deliberately flipped:

```rust
/// Bypass the platform read-through cache on ad-eligible publisher navigations.
///
/// `true` preserves the historical behaviour introduced by the SSAT 304-prevention
/// design. `false` lets those navigations use the read-through cache; the
/// conditional-header strip already guarantees a complete body on a cache HIT.
/// Temporary operator control for the Stage 0 rollout — remove once settled.
#[serde(default = "default_bypass_origin_cache")]
pub bypass_origin_cache: bool,
```

```rust
fn default_bypass_origin_cache() -> bool {
    true
}
```

**Adding this field breaks nine sites. Update them in the same commit or Step 2 fails to
compile before it can produce the intended RED failure:**

- The hand-written `Default` impl at `settings.rs:81-97`.
- Eight exhaustive test literals. The line numbers below anchor each
  `let publisher = Publisher {` **opening**, not a field — add the new field inside each
  brace: `settings.rs:3553`, `:3564`, `:3575`, `:3586`, `:3597`, `:3608`, `:3621`,
  `:3635`. `clippy-fastly` runs `--all-targets`, so these gate lint too.
- The rustdoc example for `origin_host`, whose literal opens at `settings.rs:130`.
  **This is a live doctest** and the host-triple test command below does not skip
  doctests.

While there, mirror the existing default-agreement test
`publisher_default_max_buffered_body_bytes_matches_config_default` (`settings.rs:3648`) —
it exists to catch a hand-written `Default` diverging from a serde default, which is
exactly the shape this field re-introduces. One assertion.

Then emit the line, immediately after computing `origin_fetch_ms`, gated on the debug
flag from the section above:

```rust
if settings.debug.publisher_timing {
    log::info!(
        "publisher_timing origin_fetch_ms={origin_fetch_ms} \
         cache_bypass={cache_bypass} ad_stack={should_run_ad_stack}"
    );
}
```

- [ ] **Step 4: Instrument `hold_wait_ms` — the decision metric**

This is the number the whole effort turns on, and it needs **one edit in one function**.

`collect_stream_auction` (`publisher.rs:2431`) is the only function that awaits the
auction collect, and all three call sites reach it:

| Call site           | Path                                                         |
| ------------------- | ------------------------------------------------------------ |
| `publisher.rs:793`  | `hold_collect_close_tail` — Fastly lazy stream               |
| `publisher.rs:2257` | `body_close_hold_loop`, EOF arm — Axum, Cloudflare, Spin     |
| `publisher.rs:2311` | `body_close_hold_loop`, mid-stream arm — same three adapters |

Instrument the callee, not the callers. It already destructures `settings` out of
`AuctionCollectDeps` (`:2436`), so the debug flag is in scope with no new plumbing, and
one edit covers every adapter.

Wrap the `collect_dispatched_auction` await at `:2447-2449`:

```rust
    let hold_wait_start = Instant::now();
    let result = orchestrator
        .collect_dispatched_auction(dispatched, services, &collect_ctx)
        .await;
    if settings.debug.publisher_timing {
        let hold_wait_ms =
            u64::try_from(hold_wait_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        log::info!("publisher_hold hold_wait_ms={hold_wait_ms}");
    }
```

`settings` here is `&&Settings` from the destructure — deref as needed; the compiler will
say so.

**Do not instrument `hold_finish_ready_segments` (`:869`) or `hold_finish_tail_segments`
(`:896`).** Neither awaits the collect. The first returns `close_found` for its caller to
act on; the second delegates to `hold_collect_close_tail` at `:909`. Instrumenting them
would double-count.

**Do not instrument the auction itself.** `OrchestrationResult::total_time_ms`
(`orchestrator.rs:285`, struct at `:1449`, per-provider at `:365`) already flows to
`auction_events_raw`. `hold_wait_ms` measures something different and more useful: how
long the _response_ waited, which is near zero when the auction finished during transfer
even though `total_time_ms` is large.

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p trusted-server-core --target aarch64-apple-darwin \
  eligible_navigation_logs_origin_fetch_duration -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Run the full publisher test module under the real target**

A format-changing edit to this file can break tests far from the one you added, and the
Viceroy runner aborts on the first panic — so run the whole suite, not a filtered subset.

```bash
cargo test-fastly
```

Expected: PASS. `app::tests` DNS `Error` lines in the output are pre-existing noise.

- [ ] **Step 7: Verify format and lint**

```bash
cargo fmt --all -- --check
cargo clippy-fastly
```

Expected: both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs \
        crates/trusted-server-core/src/settings.rs \
        crates/trusted-server-core/src/test_support.rs
git commit -m "Add an operator switch for the origin cache bypass and log origin fetch time"
```

Staging without `settings.rs` leaves a tree that does not compile.

- [ ] **Step 9: Deploy and collect**

Deploy first. Then enable the log — it is gated and off by default:

```bash
# In the operator's trusted-server.toml, under [debug]:
#   publisher_timing = true
ts config push
```

**Deploy before pushing, not after.** `Settings`, `Publisher`, and `DebugConfig` all carry
`#[serde(deny_unknown_fields)]`, and `ts config push` validates against the typed schema
(`crates/trusted-server-cli` → `run_config_push_typed::<TrustedServerAppConfig>`). So the
`ts` binary must be rebuilt from this commit (`cargo install-cli`), and pushing the new
keys before the new WASM is live would break config load on the deployed build.
`trusted-server.example.toml:121-125` records this same hazard for
`auction.rewrite_creatives`.

Then capture the **bypass-on baseline only**. Do not try to collect an off arm here —
turning the bypass off _is_ Task 5, which is gated on Task 1's verdict and forbidden on a
FAIL. The off arm is collected in Task 5 Step 8.

Capture enough navigations to separate the medians with confidence, across both a homepage and an article path, with the bypass
both on and off. Record the N alongside the result.

Append to the findings document:

```markdown
## Step C — server-side latency breakdown

**N per arm:** <n> · **Paths:** <paths> · **Date:** <date>

| Arm        | `origin_fetch_ms` = origin TTFB (median) | auction `total_time_ms` (median) | `rewrite_ms` (median) |
| ---------- | ---------------------------------------- | -------------------------------- | --------------------- |
| bypass on  |                                          |                                  |                       |
| bypass off |                                          |                                  |                       |

Read the asymmetry carefully. `origin_fetch_ms` is origin **TTFB** — the send returns at
response headers because `.with_stream_response()` is set — whereas `total_time_ms` is
the auction's full duration. The comparison below is still the right one, but it is not
comparing two like quantities.

**Verdict:** HOLD IS FREE / HOLD IS COSTING

Read it off `hold_wait_ms` directly — no model, no comparison against auction duration.

HOLD IS FREE = `hold_wait_ms` median near zero. The auction finishes during body
transfer. Proceed as staged in spec §7: Stage 0 primary, Stage 2 protects its win.

HOLD IS COSTING = `hold_wait_ms` median materially non-zero. **Staging inverts** —
Stage 2 becomes primary and Stage 0 secondary. The work does not change, only its order.
Spec §6.2 argues for the first outcome but explicitly does not prove it, so treat the
second as a live possibility.
```

- [ ] **Step 10: Commit the findings**

```bash
cd docs && npx prettier --write superpowers/plans/2026-08-08-1009-measurement-findings.md && cd ..
git add docs/superpowers/plans/2026-08-08-1009-measurement-findings.md
git commit -m "Record server-side latency breakdown for #1009"
```

---

> **Task 4 (spec correction) was completed while this plan was being written.** §3's
> mechanism bullet, §4's operator-flag framing, and the `unexpected_origin_304` watch are
> all already in the spec. Nothing to do; the task is removed rather than left as a
> no-op an implementer would stall on.

---

## Task 5: Stage 0 — turn the origin cache bypass off

**Gate:** do not flip the flag until Task 1 has recorded a **`FINAL PASS`**. There are
three verdicts, not two.

- **`FINAL PASS`** → Task 5a (config flip).
- **`PROVISIONAL PASS`** → **stop.** Not a release gate. This is the current state. It
  means the representation split is declared correctly under the conditions tested, and
  that those conditions were too narrow to flip production on.
- **`FAIL`** → Task 5b. Do **not** flip; it can serve an RSC payload to an HTML
  navigation.

**`FINAL PASS` requires all five, each recorded in the findings document:**

| Condition                                                | Why the provisional run is insufficient              |
| -------------------------------------------------------- | ---------------------------------------------------- |
| A real authenticated or state-bearing session cookie     | `sessionid=abc123` is synthetic and proves nothing   |
| Basic Auth exercised **through TS**, not just the origin | #1009 describes a gated deployment                   |
| The experiment variant named in #1009                    | Absent from the origin's `Vary`; unexplained         |
| Representative routes — article, section, search         | Only the homepage was probed                         |
| Cached-hit slot and render attribution                   | The randomized div IDs are an unverified interaction |

Any one of these unrecorded means the verdict stays `PROVISIONAL PASS` and Task 5 does
not start.

Because Task 3 made the bypass config-driven, Stage 0 ships as a **config change on an
already-deployed build** — no second release, and the read path reverts with another
config push rather than a revert. That matters here: the failure mode this gates on is
cache poisoning, where minutes of exposure are worse than a slow rollout.

**But a config push is not a full rollback.** It stops HTML navigations reading from
cache; it evicts nothing already stored. See Step 4's rollback sequence — flip, then purge
or roll a versioned namespace, then observe past the origin TTL. Until a C1 purge path
exists, the tail is "wait out the origin TTL," and that must be an accepted, recorded
risk before the flip.

### Task 5a: flip the flag (Task 1 verdict = `FINAL PASS`)

**Files:**

- Modify: the operator's `trusted-server.toml` (gitignored)
- Modify: `crates/trusted-server-core/src/publisher.rs` — the test, and later the default
- Modify: `trusted-server.example.toml` — document the key

- [ ] **Step 1: Add a test covering the flag in both positions**

The existing test at `publisher.rs:4824`
(`eligible_navigation_bypasses_cache_and_returns_non_storable_html`) asserts `vec![true]`
and must **keep passing** while the default is `true` — it now documents the default
rather than the only behaviour. Leave it, and add a sibling next to it:

```rust
#[tokio::test]
async fn eligible_navigation_uses_read_through_cache_when_bypass_disabled() {
    // Arrange
    let mut settings = settings_with_enabled_auction_and_creative_opportunities();
    settings.publisher.bypass_origin_cache = false;
    let stub = Arc::new(StubHttpClient::new());
    queue_cacheable_html_response(&stub);
    let services = build_services_with_http_client(
        Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
    );
    let slots = [article_slot()];

    // Act
    let response =
        run_with_slots(&settings, &services, &slots, conditional_navigation_request()).await;
    let response_head = response_head(response);

    // Assert
    assert_eq!(
        stub.recorded_cache_bypass_flags(),
        vec![false],
        "disabling bypass_origin_cache should let the navigation use the read-through \
         cache; the conditional-header strip already guarantees a full body on a HIT"
    );
    assert_eq!(
        recorded_header(
            stub.recorded_request_headers().first().expect("should record request"),
            header::IF_NONE_MATCH.as_str()
        ),
        None,
        "conditional headers must still be stripped with the bypass disabled"
    );
    assert!(
        response_head
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("no-store")),
        "the synthesized document must stay non-storable regardless of the bypass flag"
    );
}
```

Those last two assertions are the point of the test: the flag must change **only** the
cache mode, leaving the conditional-header strip and the response non-storability intact.

**Leave `publisher.rs:4941` and `:5160` unchanged** — they already assert `vec![false]`
for non-eligible requests and must keep doing so. `Range`/`If-Range` stripping is covered
by `eligible_range_navigation_fetches_complete_html` (`publisher.rs:4883`), unaffected.

- [ ] **Step 2: Run both tests**

```bash
cargo test -p trusted-server-core --target aarch64-apple-darwin \
  eligible_navigation -- --nocapture
```

Expected: both the existing default-behaviour test and the new flag-disabled test PASS.
If Task 3's `bypass_origin_cache` field is not yet in place, the new test will not
compile — land Task 3 first.

- [ ] **Step 3: Document both keys in the example config**

Add to `trusted-server.example.toml` under `[debug]` (line 149, alongside
`ja4_endpoint_enabled` and `auction_html_comment`):

```toml
# Emit a `publisher_timing` log line per publisher origin fetch. Temporary
# instrumentation for the #1009 latency measurement; leave false in production.
publisher_timing = false
```

And under `[publisher]`:

```toml
# Bypass the platform read-through cache on ad-eligible navigations.
# `true` is the historical default. Set `false` to let those navigations use the
# read-through cache — only after confirming the origin declares `Vary` for every
# header it varies on (see the Stage 0 precondition).
bypass_origin_cache = true
```

- [ ] **Step 4: Flip it in the operator config and push**

```bash
# In the operator's trusted-server.toml, under [publisher]:
#   bypass_origin_cache = false
ts config push
```

Note from prior operational experience in this repo: the environment-variable overlay is
scalar-only **and** only overrides keys that already exist in the TOML. Adding the key to
the operator's file is required; setting only an env var will be silently dropped.

**Rollback is a config push plus an eviction — not a config push alone.** Pushing `true`
again stops HTML navigations reading from cache, but evicts nothing: objects already
cached, including those RSC and other request classes keep reading, persist until they
expire. The origin's `max-age=60` bounds that, but does not remove it.

Full rollback:

1. Push `bypass_origin_cache = true`.
2. Purge — **and note this is C1, not C2.** `InsertBuilder::surrogate_keys` belongs to
   the Core Cache API and applies to the transformed-template cache the ESI spike builds.
   It has no effect on the HTTP read-through cache that Stage 0 turns on. Purging C1
   requires either surrogate keys the **origin** supplies on its responses, or the HTTP
   cache's own request/candidate surrogate-key surface. Confirm which is available before
   relying on it.

   **Neither is wired today.** If the flip ships before one exists, the rollback story is
   "wait out the origin TTL" — roughly a minute, per the Step A findings. That is
   survivable, but it must be an accepted risk recorded before the flip rather than a
   discovery during an incident.

3. Observe past the origin TTL before declaring the incident closed.

- [ ] **Step 5: Run the full suite across every adapter**

```bash
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
```

Expected: all PASS. If `platform/test_support.rs:797` or `:888` fail, they are testing
the stub's own recording behaviour rather than publisher behaviour — read them before
changing anything.

- [ ] **Step 6: Format and lint every target**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare \
  && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm
```

Expected: all clean.

- [ ] **Step 7: Commit the code and config-template changes**

```bash
git add crates/trusted-server-core/src/publisher.rs trusted-server.example.toml
git commit -m "Add an operator switch for the publisher origin cache bypass"
```

- [ ] **Step 8: Watch for the failure modes, not just the win**

After the flip, check three things before declaring success. The first two are regression
signals, not confirmations.

1. **`unexpected_origin_304` abandonment telemetry.** This reason
   (`publisher.rs:2896`, emitted via `emit_abandoned_auction` at `:2360`) exists because
   the ad-stack path refuses cached and conditional origin responses. Re-enabling the
   cache is precisely what could revive it. **Any non-zero rate is a rollback signal** —
   it means a 304 is reaching TS, which the conditional-header strip was supposed to make
   impossible. Push `true` and investigate before continuing.
2. **Representation mixing.** Spot-check that HTML navigations still return HTML and RSC
   fetches still return `text/x-component`. A mismatch is the Task 1 risk having
   materialized despite a PASS verdict — roll back immediately, this is cache poisoning.
3. **`origin_fetch_ms` and `cache_bypass=false`** in the `publisher_timing` logs. This is
   the win, and it is the _last_ thing to check, not the first.

- [ ] **Step 9: Record and commit**

Append the before/after medians and the three checks above to the findings document,
format it, and commit.

- [ ] **Step 10: Retire the flag (follow-up, not now)**

Once the flip has held for a sustained period, flip the default to `false` in
`default_bypass_origin_cache`, then remove the setting and the branch entirely. Track it;
a temporary flag left in place becomes permanent configuration surface.

### Task 5b: cache-key discriminator (Task 1 verdict = FAIL)

**Do not implement from this plan.** A FAIL means the origin serves multiple
representations at one URL without declaring `Vary`, so removing the bypass requires TS
to add its own cache-key discriminator — a feature, not a deletion, and materially larger
than Stage 0 as scoped here.

Escalate with the Task 1 findings and write a separate plan. Two things that plan must
address, both from spec §4:

1. The discriminator must key on the request headers that actually distinguish the
   representations (`RSC`, `Next-Router-*`, the experiment header), **not** on the
   navigation classification. `is_navigation_request`
   (`crates/trusted-server-core/src/http_util.rs:73-98`) falls back to the `Accept`
   header when Fetch Metadata is absent, and its own comment warns that `fetch()` can set
   `Accept: text/html` — so a fetch-based request can be misclassified as a navigation.
2. Whether the origin should simply be asked to declare `Vary`, which is cheaper than
   building the discriminator and fixes the problem for every consumer rather than only
   for TS.

---

## Out of scope

Named so nobody widens this plan mid-flight. All are specified in the spec.

- **Stages 1–2** — moving bid delivery off the response body and deleting the `</body>`
  hold. Spec §7 and §8 put these behind the correctness defects. Spec §5 explains why
  starting them casually produces a silent revenue loss.
- **Stages 3a/3b** — response cacheability. 3b is additionally gated on Task 2.
- **Stages 4–5** — purge capability, TS-owned template cache, ESI.
- **Removing the `bypass_cache` platform capability.** Task 5a removes one call site only.

---

## Definition of done

- [ ] Findings document records verdicts for Steps A, B, and C, each with its date, its
      N where applicable, and the consequence spelled out.
- [ ] `publisher_timing` and `publisher_hold` lines are emitted in production and
      readable, and `hold_wait_ms` has a recorded median.
- [ ] Task 1 recorded a **`FINAL PASS`** — all five conditions in Task 5's gate closed,
      not merely the provisional run.
- [ ] Either Task 5a is shipped, or Task 1 returned FAIL and both a production defect and
      a follow-up plan for 5b exist.
- [ ] A purge path or versioned cache-key namespace exists **before** the flip, or the
      "wait out the TTL" rollback is explicitly accepted and recorded as a risk.
- [ ] **The win is measured client-side, not from `origin_fetch_ms`.** That figure is
      origin TTFB and excludes body download, rewrite, and post-processing — it is
      attribution, not the outcome. #1009 already has a working tester-cookie browser A/B
      measuring the TTFB the publisher actually complained about; use it for before/after.
- [ ] `unexpected_origin_304` rate is zero and representations are not mixed (Task 5a
      Step 8) — both checked **before** the win is claimed.
- [ ] **Cross-document invariants hold:** `python3 scripts/docs-invariants.py`. This is a
      named gate, not a courtesy check. `npm run format` and `npm run build` catch
      formatting and dead links; neither catches a claim corrected in one document and
      left standing in another, which is the failure mode this document set has hit on
      four separate review rounds. Add a check whenever a correction lands.
- [ ] All CI gates pass: `cargo fmt --all -- --check`; the six clippy targets; the four
      adapter test suites; the parity suite; JS build, test, and format; docs format.
