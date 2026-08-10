# #1009 ESI Validation Spike

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decide #1009 on evidence. Build a shared-template pipeline behind a flag, run
ESI and client-fill against it, and produce a decision record that either adopts ESI,
adopts client-fill, or rejects both — with the Fastly-only maintenance cost priced in.

**Architecture:**
`origin → lol_html transform → fastly::cache::core → finalize headers → stream assembly`.

Headers finalize **before** assembly, not after — streaming responses on this adapter
commit headers first and then pipe chunks, so nothing can be set once assembly starts.

The transform emits **one unconditional marker at the body-close seam**. Not two: the
head seam is not a template hole, because `tsjs.adSlots` presence is request-gated
(Task 3 Step 2). The cached object is a shared template with no per-user bytes and no
request-dependent decisions. Assembly is either the `esi` crate (edge) or a client fetch
(browser), selected per request by the arm allocator so both are measured on one build.

**Tech Stack:** Rust 2024, `wasm32-wasip1`, `fastly` 0.12.1 (`cache::core`, `http::purge`),
`esi` 0.7, `lol_html`, a real Fastly test service for cache behaviour.

**Spec:** `docs/superpowers/specs/2026-08-08-esi-cacheable-root-validation-design.md` —
read the 2026-08-10 correction at the top and
[§6.6](../specs/2026-08-08-esi-cacheable-root-validation-design.md#66-the-esi-pipeline-corrected)
before writing any code.

**Control:** [the Stage 0 plan](./2026-08-08-1009-measurement-and-stage-0.md). Its
instrumentation and its bypass flag are prerequisites — this plan compares against them
and does not duplicate them.

---

## Why this plan exists

An earlier revision of the spec concluded ESI was structurally impossible. It was wrong:
`fastly::cache::core` provides the cache boundary natively, and purge runs inside Compute.
That correction reopens #1009 as an empirical question, and this plan is how it gets
answered.

**What is genuinely uncertain**, and what each arm is for:

1. Does a shared template plus per-request assembly beat today's inline path enough to
   matter?
2. Does **edge** assembly (ESI) beat **client** assembly (a fetch) by enough to justify a
   Fastly-only rendering path that must be maintained alongside the portable one?
3. Can per-user leakage be excluded across cold MISS, warm HIT, stale revalidation,
   transform failure, and fragment failure?

Question 3 is a gate, not a metric. A win on 1 and 2 with a failure on 3 is a rejection.

## Three caches, never conflated

The original error came from treating these as one thing. Every task below names which it
means.

| #   | Cache                             | Contents                      | Status                              |
| --- | --------------------------------- | ----------------------------- | ----------------------------------- |
| C1  | Origin read-through               | raw origin bytes              | Exists. Stage 0 turns it back on.   |
| C2  | Shared transformed template       | post-`lol_html`, pre-assembly | **New.** What this plan builds.     |
| C3  | Assembled-response delivery cache | final per-user output         | **Must never exist.** Not proposed. |

If a task appears to require C3, stop — that is the leakage failure mode, not a design
option.

## Arms

Five, but only four are treatable as equivalent.

| Arm     | Root                    | Bids             | Notes                                                      |
| ------- | ----------------------- | ---------------- | ---------------------------------------------------------- |
| **A0**  | inline, C1 bypassed     | inline `</body>` | Today. The baseline.                                       |
| **A1**  | inline, C1 on           | inline `</body>` | Stage 0. Isolates the bypass from the template change.     |
| **A2**  | shared template from C2 | client fetch     | Portable. Works on all four adapters.                      |
| **A3**  | shared template from C2 | ESI at the edge  | Fastly-only. The thing #1009 proposed.                     |
| **REF** | origin direct, TS off   | publisher's own  | **Reference, not an arm.** Different work, not comparable. |

A0→A1 measures the bypass. A1→A2 measures the template split. A2→A3 measures edge versus
client assembly — **that difference is the entire case for ESI**, and it is the number
this plan exists to produce.

**Do not compare A2 and A3 on root TTFB.** They serve the same C2 template, so their root
timings should be near-identical by construction; a null result there proves nothing.
ESI's claimed advantage is that bids arrive without a client round-trip, so measure:
**bids-ready time**, **`adInit` fire time**, and **first TS-attributed creative paint**.
Root TTFB stays as a guard that the template path did not regress, not as the comparison.

REF is included because #1009 anchors on it, and excluded from pass/fail because TS-off
does no auction and no injection. Comparing against it measures the feature's existence,
not its implementation.

---

## Task order and dependencies

```
Task 1 (esi compiles) ── DONE, PASS ──┐
                                      ├──> Task 3 (C2 cache)  ─┬──> Task 4 (A2 client-fill)
Stage 0 plan (flag + instrumentation) ┘                        ├──> Task 5 (A3 ESI)
                                                               └──> Task 6 (safety gates)
                                                                          │
                        Task 2 (real service) ─────────────────────────────┴──> Task 7 (decision)
```

**Task 2 is not a blocker on Tasks 3–6.** Everything those tasks need is exercisable under
Viceroy 0.17 — verified, see Task 2. The real service is required only for the
measurements Task 7 decides on, so provision it once there is something worth measuring.

Task 6 runs against every arm, not once at the end.

---

## Task 1: Confirm `esi` 0.7 builds on this toolchain

Cheapest possible falsification. Do this before anything else.

**Files:** `crates/trusted-server-adapter-fastly/Cargo.toml`

- [ ] **Step 1: Add the dependency**

```bash
cargo add esi@0.7 --package trusted-server-adapter-fastly
```

It belongs in the **Fastly adapter**, never in `trusted-server-core` — the crate is
hard-bound to `fastly::{Request, Response, Backend}` and core must stay portable.

- [ ] **Step 2: Check it compiles for the real target**

```bash
cargo check-fastly
```

Expected: clean. The crate declares edition 2021 with no `rust-version`, and pulls recent
`rand` and `nom`, so this is a genuine question on Rust 1.95.0 / `wasm32-wasip1`.

- [ ] **Step 3: Check no shared dependency was forced to move**

```bash
git diff --stat Cargo.lock
cargo check --manifest-path crates/trusted-server-integration-tests/Cargo.toml --tests \
  --target "$(rustc -vV | sed -n 's/^host: //p')"
```

**Correction, verified 2026-08-10:** an earlier revision of this step warned about a
desync between the root `Cargo.lock` and `crates/trusted-server-integration-tests/Cargo.lock`.
**That second lockfile does not exist** — the crate is a workspace member (root
`Cargo.toml:10`) and shares the root lockfile. The hazard cannot arise in that form.

What does matter is whether adding `esi` forces an **existing** shared dependency to a new
version, since `regex`, `bytes`, and `log` are used across the workspace. Adding a new
major that coexists is harmless; moving an existing one is not. If one moves, fix with a
targeted `cargo update -p <crate> --precise <version>` — **never a full update**.

**Already run and recorded** in [the findings](./2026-08-08-1009-measurement-findings.md):
no existing shared dependency moved.

- [ ] **Step 4: Record and commit, or stop**

**Task 1 is complete — verdict PASS, recorded 2026-08-10.** `esi` 0.7.1 compiles clean on
Rust 1.95.0 / `wasm32-wasip1`, all six clippy targets pass, and no existing shared
dependency moved. See [the findings](./2026-08-08-1009-measurement-findings.md).

Had Step 2 failed, this plan would have stopped here with #1009 answered "not on this
toolchain." It did not.

```bash
git add crates/trusted-server-adapter-fastly/Cargo.toml Cargo.lock
git commit -m "Add the esi crate to the Fastly adapter for the #1009 validation spike"
```

---

## Task 2: Local validation first, real service only for what needs it

**Verified 2026-08-10 under Viceroy 0.17: the entire Core Cache surface this spike uses
works locally.** A probe exercised `cache::core::insert`, `lookup`, `finish`, `to_stream`,
and — the shape Task 3 Step 4 actually specifies — `Transaction::lookup`,
`must_insert_or_update`, `insert(...).surrogate_keys(...).execute_and_stream_back()`, and
hit-after-insert semantics. All passed. Recorded in
[the findings](./2026-08-08-1009-measurement-findings.md).

That reorders this plan. An earlier revision made provisioning a Fastly service Task 2 and
a blocker on everything after it. It is not a blocker: **almost all of the correctness and
safety work is local**, and only the numbers and the cache topology need real
infrastructure.

| Work                                                         | Where        |
| ------------------------------------------------------------ | ------------ |
| C2 insert / lookup / transaction logic (Task 3)              | **Local**    |
| The `lol_html` transform and template byte-identity (Task 3) | **Local**    |
| ESI assembly — the crate is pure Rust over `BufRead`/`Write` | **Local**    |
| DCA off, dispatcher allowlist, injection refusal (Task 5)    | **Local**    |
| Fragment-failure degradation (Task 5)                        | **Local**    |
| Header-finalization ordering, no-C3 assertions (Task 6)      | **Local**    |
| Cross-user leakage / request-neutrality gates (Task 6)       | **Local**    |
| Shielding behaviour                                          | Real service |
| POP-level cache tiering (`x-cache`, `hit-state`, `age`)      | Real service |
| Request collapsing under genuine concurrency                 | Real service |
| Stale revalidation timing at the edge                        | Real service |
| **Every performance number in Task 7's decision rule**       | Real service |

**So: build and prove correctness locally through Tasks 3, 5, and 6 before provisioning
anything.** If the design is wrong or leaks, that surfaces locally for free, and the
service is only needed once there is something worth measuring.

Two caveats on the local scope. Viceroy is a single instance, so a passing `Transaction`
test proves the API works, **not** that collapsing behaves correctly under load. And local
timings are meaningless for the decision — do not let a fast local run substitute for
Task 7 evidence.

### When the real service is needed

- [ ] **Step 1: Provision it — after local correctness passes, not before**

Separate from production. Confirm and record: whether the publisher backend is
**shielded**, and whether any Delivery service fronts the Compute service. Both change
what the numbers mean.

```bash
fastly service list
fastly backend list --service-id <test-id> --version latest
```

The shielding answer also settles an open question from the Stage 0 findings: #1009's
off-TS win came from a shield HIT, so whether the test service has one determines whether
its numbers transfer to production at all.

- [ ] **Step 2: Extend the harness for lineage, not just correlation**

The existing tester-cookie A/B has no way to join server timings to browser timings. A
root-only request ID is not enough either: under A3 the auction happens in a **fragment
subrequest**, so a root ID never reaches the auction telemetry.

Propagate a **lineage ID plus the experiment arm** through the whole chain:

```
root request → C2 lookup → fragment subrequest → auction telemetry → browser render event
```

Generated at TS entry, forwarded into the fragment request, attached to the
`auction_events_raw` row, echoed as `x-ts-request-id`, and exposed to the browser harness
so render events carry it. Every timing log line includes both fields.

Without this the experiment cannot join hold time, origin time, auction telemetry, browser
TTFB, and render outcome for the same pageview. **That is the difference between an
experiment and a pile of numbers.**

- [ ] **Step 3: Capture C1 and C2 status separately**

`x-cache`, `hit-state`, and `age` describe the **HTTP read-through cache (C1)**. They say
nothing about the **transformed-template cache (C2)**, which is a `cache::core` object
with no HTTP semantics. Recording only the former and calling it "cache status" would
attribute C2 hits and misses to the wrong tier.

Emit both: the C1 headers as-is, plus an explicit `x-ts-c2` field carrying HIT / MISS /
STALE / BYPASS from the transaction outcome. Record the serving POP alongside. A median
that mixes cold-MISS and warm-HIT requests is meaningless, and arms cannot be compared
unless the mix is known — per tier.

- [ ] **Step 4: Build a request-scoped arm allocator**

`AssemblyMode` as specified in Task 3 is a **global** setting, but the sample plan below
requires randomized, non-sequential allocation. A global flip gives sequential blocks
instead, which confounds arm with time of day, cache warmth, and traffic mix.

Allocate per request: hash the lineage ID into buckets, or key off the tester cookie.
The global setting stays as the kill switch and as the way to force a single arm; the
allocator is what the experiment actually uses. Record the assigned arm on every log line
and every telemetry row.

- [ ] **Step 5: Define the sample plan before collecting anything**

Write all of this into the findings document **before** the first measurement, and treat
it as fixed:

| Element              | What to state                                                                                                              |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| Allocation           | Requests per arm per route, and how arms are assigned                                                                      |
| Randomization        | Randomized or blocked by route and cache state — not sequential runs                                                       |
| Pilot variance       | A small pilot to estimate variance, before sizing the real run                                                             |
| MDE and power        | The smallest difference worth detecting, and the N that detects it                                                         |
| CI method            | Which interval, computed how                                                                                               |
| Warmup and carryover | How cold MISS is forced, how warm HIT is confirmed, and how one arm's cache state is prevented from contaminating the next |

Rationale: this whole effort exists because #1009 drew a causal conclusion from N=4 that
did not survive contact with the code. Repeating that with more arms and no power
calculation would be worse, not better — it would look rigorous while being equally
unfalsifiable.

---

## Task 3: Build C2 — the shared transformed-template cache

The core of the spike. Behind a flag, default off.

**Files:**

- `crates/trusted-server-core/src/publisher.rs` — emit **one** unconditional marker at the body-close seam (see Step 2; the head seam is not a template hole)
- `crates/trusted-server-core/src/settings.rs` — the mode flag
- `crates/trusted-server-adapter-fastly/src/` — the `cache::core` read/write

- [ ] **Step 1: Add the assembly-mode setting**

```rust
/// How per-user ad state reaches the page.
///
/// `Inline` is today's behaviour: bids injected before `</body>`, root uncacheable.
/// `ClientFill` and `Esi` both serve a shared template from the transformed-template
/// cache and fill the holes afterwards. Spike-only — remove with the spike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyMode {
    #[default]
    Inline,
    ClientFill,
    Esi,
}
```

Default `Inline` so the flag is a no-op until set. Note the hazards the Stage 0 plan
already documents: `Settings` carries `#[serde(deny_unknown_fields)]`, `ts config push` is
typed, and `Publisher` has a hand-written `Default` plus eight exhaustive test literals
and a live doctest.

- [ ] **Step 2: Make the template strictly request-neutral**

**The obvious design is wrong and would leak.** An earlier draft kept `tsjs.adSlots` in
the shared template on the grounds that it is per-URL. Its _content_ is per-URL; its
_presence_ is not. It is gated on `should_run_ad_stack` (`publisher.rs:2920-2927`), which
is `is_get && is_navigation && !is_prefetch && !is_bot && has_matched_slots &&
consent_allows_auction && auction_enabled`.

So the first request to fill C2 would freeze **its own** consent decision, bot
classification, prefetch status, and kill-switch state into an object every later visitor
reads. A consent-denied first fill serves a no-ads template to consenting users; a
consenting first fill serves ad markup to a user who refused.

**Rule: the template contains an unconditional inert placeholder and nothing else.**

| Element                   | Where it lives                                     |
| ------------------------- | -------------------------------------------------- |
| tsjs bundle script tag    | Template — content-hashed, genuinely per-URL       |
| URL rewrites              | Template — per-host, in the cache key              |
| `tsjs.adSlots`            | **Fragment** — its presence is request-dependent   |
| `tsjs.bids`               | **Fragment**                                       |
| GPT diagnostics bootstrap | **Fragment** — gated on a per-request cookie/query |

Emit **one** unconditional marker at the body-close seam, identical on every request that
reaches the transform. Under `Esi` it is an `<esi:include>`; under `ClientFill` it is
nothing at all, with the client fetching unprompted.

- [ ] **Step 3: Bypass C2 for anything that must not be shared**

`cache::core` is not an HTTP cache — it will happily store whatever you hand it. Nothing
rejects private or authenticated responses for you. Refuse to insert when **any** holds:

- The origin response carries `Set-Cookie`.
- The origin response is `private`, `no-store`, or `no-cache`.
- The request carried `Authorization`.
- The response is not 200 with an HTML content type.
- DataDome's request filter replaced the document.

Audit every request-dependent rewrite before declaring the template neutral — the
integration head-inserts and the GPT-diagnostics bootstrap are both request-scoped and
must not reach C2.

**Assert it, do not assume it.** A unit test over the transform output must fail on any
of: a bid value, an EC ID, a consent string, a geo value, a diagnostics bootstrap, or a
`Set-Cookie`. Then a second test must assert the template is **byte-identical** for two
requests differing in consent, bot classification, and prefetch status. That second test
is the one that catches this class of bug; the first would have passed on the broken
design.

- [ ] **Step 4: Write and read C2 — with the real API**

The builder is move-based and the insert and read handles are different objects. Naïve
code does not compile:

```rust
// WRONG — surrogate_keys consumes the builder and returns it; this discards the
// return value and then uses a moved binding. And execute() gives a WRITE stream,
// so there is nothing to read back from it.
let mut insert = cache::core::insert(key, ttl);
insert.surrogate_keys(["ts-template"]);
let body = insert.execute()?;
```

Correct shape, using a transaction so a cold cache under load transforms once:

```rust
use fastly::cache::core::{Transaction, CacheKey};

let tx = Transaction::lookup(CacheKey::from(key_bytes)).execute()?;

// Order matters: a STALE entry sets BOTH found() and must_insert_or_update().
// Testing found() first would serve the stale bytes and silently never fulfil the
// update obligation, leaving every concurrent waiter blocked until timeout.
let template: Body = if tx.must_insert_or_update() {
    // Fetch and prepare BEFORE consuming `tx`. After `insert()` the transaction is
    // gone and `cancel_insert_or_update()` is unreachable, so anything that can fail
    // and does not need the writer belongs here.
    let origin = match fetch_and_prepare_origin() {
        Ok(origin) => origin,
        Err(e) => {
            tx.cancel_insert_or_update()?;   // releases the obligation to a waiter
            return fallback_uncached(e);
        }
    };

    // `Transaction::insert(self)` consumes `tx` from this line on.
    let (mut writer, found) = tx
        .insert(template_ttl)
        .surrogate_keys(["ts-template", &url_surrogate_key])   // chained, not discarded
        .user_metadata(metadata_envelope)
        .execute_and_stream_back()?;

    match stream_lol_html_output(origin, &mut writer) {
        Ok(()) => {
            writer.finish()?;        // REQUIRED, and consumes `writer`
            found.to_stream()?       // fallible; there is no `to_body()`
        }
        Err(e) => {
            // Also consumes `writer`, marking an unsuccessful end so no partial
            // template is served. (A `StreamingBody` dropped without `finish()` is
            // aborted anyway, but say it explicitly.)
            writer.abandon()?;
            return fallback_uncached(e);
        }
    }
} else if let Some(found) = tx.found() {
    found.to_stream()?               // C2 HIT — skip origin fetch and transform
} else {
    unreachable!("a transaction is either obliged to insert or has found an item")
};
```

Two ownership rules this shape exists to respect, both of which an earlier draft broke:
`Transaction::insert(self)` **consumes** the transaction, so a helper taking `&tx` cannot
call it and `cancel_insert_or_update` is unreachable afterwards; and `finish`/`abandon`
each consume the writer, so neither can be referenced from an arm that did not bind it.

**Decide the stale policy explicitly.** `Found::is_stale()` and `is_usable()` exist, and
`stale_while_revalidate` can be set at insert. Serving stale while revalidating is a real
option — but it is a state machine, and `cache::core` implements none of it for you. The
spike should start by treating stale as a miss and only add stale-serve if the numbers
justify it.

**`cache::core` carries no HTTP semantics.** Status, headers, content encoding, and
revalidation are all yours. Serialize what you need into `user_metadata` — at minimum the
content encoding, the transform schema version, and the origin `Vary` values the key was
built from — and decide explicitly whether the stored template is compressed.

**Cache key must include**, beyond the origin's declared `Vary` (`rsc`,
`next-router-state-tree`, `next-router-prefetch`, `next-router-segment-prefetch`,
`Accept-Encoding` — measured, see the Stage 0 findings):

- The full URL, explicitly. Do not rely on an ambient request key.
- **The assembly mode.** A2 and A3 emit different template bytes and would otherwise
  poison each other's entries.
- **A template schema version**, bumped whenever the transform changes, so a deploy does
  not read yesterday's shape.
- Request host and scheme, the enabled-integration set, and the tsjs content hash.

Per-user signals must never appear in the key. If a signal cannot be excluded from the
template, it does not belong in C2 at all.

**Design choice to make explicitly before writing code.** Two viable shapes:

1. **Read-through with `after_send` + `set_body_transform`** — keeps HTTP semantics,
   revalidation, and stale handling for free; less control over the key.
2. **`cache::core` as above** — full control; you own metadata, revalidation, and the
   stale state machine.

This plan assumes (2). If (1) is chosen, Step 4 is rewritten and the metadata envelope
disappears. Either way, the platform boundary must sit **before** the origin request, or
a C2 HIT cannot actually skip the fetch — which is the entire point.

- [ ] **Step 5: Unit tests, then the target suite**

```bash
cargo test -p trusted-server-core --target aarch64-apple-darwin assembly_mode
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo fmt --all -- --check && cargo clippy-fastly
```

`ClientFill` must work on all four adapters. `Esi` is Fastly-only and must not break the
others' compilation.

---

## Task 4: Arm A2 — client-fill

Mostly already specified. See
[the spec's Appendix B](../specs/2026-08-08-esi-cacheable-root-validation-design.md#appendix-b--stage-1-plumbing-condensed)
for the client plumbing, the two-condition join gate, and the server contract; and
[§5](../specs/2026-08-08-esi-cacheable-root-validation-design.md#5-the-trap-in-the-deferred-work--read-this-before-scheduling-stages-12)
for the silent-empty-bids trap, which applies in full.

- [ ] **Step 1: Hoist the closure-trapped client state** — `pageBidsEndpoint`,
      `requestPageBids`, and the `inflight`/`currentPath`/`lastAppliedPath` state, per
      Appendix B. Do **not** route the initial load through `onNavigate`.
- [ ] **Step 2: Make `installScheduleInitialAdInit` a hydration-ready AND bids-settled
      join**, with a bounded timeout that fires `adInit` untargeted rather than stranding
      the slot. Derive the timeout from measured fetch latency, not a constant.
- [ ] **Step 3: Suppress the navigation-path dispatch** so exactly one auction runs per
      pageview. Add a new `AuctionSource` for initial loads **plus the mechanism that
      delivers it** — a header behind the same-origin gate, not a query parameter.
- [ ] **Step 4: Relocate terminal telemetry.** Navigation `Completed` is emitted only from
      the collect functions; the `ts-debug` dump rides the same string. Both move.
- [ ] **Step 5: Verify exactly one auction per pageview** in `auction_events_raw`. Two is
      a doubling of SSP spend and an immediate fail.

---

## Task 5: Arm A3 — ESI at the edge

- [ ] **Step 1: Wire `process_stream`, not the wrappers**

`process_response` and `process_response_streaming` consume `self` _and_ send the response
themselves, which takes ownership away from the finalize / `ec_finalize` / apply-effects
ordering. `process_stream(&mut self, src: impl BufRead, out: &mut impl Write, …)` keeps it.

Source is the C2 body. Sink is the client response body.

**The ordering an earlier draft described is impossible.** It said EC cookie, geo, and the
privacy net run _after_ assembly. They cannot: streaming responses on this adapter
**commit headers first and then pipe chunks**
(`adapter-fastly/src/main.rs`, `send_edgezero_response`). Once ESI starts writing, no
header can change.

The correct invariant:

> **Finalize every header before a single body byte is written** — EC `Set-Cookie`, geo
> suppression, and an unconditional `Cache-Control: private, no-store` — **then** stream
> the assembly with no further header mutation.

That means `private, no-store` is set unconditionally up front rather than derived from
what the assembly turns out to contain. Deriving it after the fact is not available, and
assuming it was is how a per-user response ends up shared-cacheable.

- [ ] **Step 2: Disable DCA explicitly and allowlist the dispatcher**

```rust
let config = esi::Configuration::default()
    .with_escaped(false)
    .with_default_dca(esi::DcaMode::None)   // call the setter; do not rely on the default
    .with_inherit_parent_dca(false);
```

Comments are not configuration. An earlier draft said DCA "stays at its default" — on a
pre-1.0 crate whose default could move in a patch release, and where this setting fails
**open**, that is not good enough. Call the setters.

Also disable **fragment caching** explicitly, or mark the include `no-store="on"`. A
cached auction fragment is a per-user object in a shared cache — the C3 failure mode by
another route.

The dispatcher must be **exact-path allowlisted**: a fragment URL that is not the bids
endpoint is refused, not fetched. The built-in dispatcher builds a dynamic backend per URL
host and panics on a hostless URL — never use it.

Rationale in the spec's §2: bid payloads carry partner-controlled creative markup, so a
recursive parse would let an SSP make the edge fetch an arbitrary URL. **Add a unit test
that feeds `<esi:include src="http://attacker.example/">` through a creative payload and
asserts no fetch is attempted.**

- [ ] **Step 3: The fragment must be a script, not the JSON endpoint**

**`/_ts/page-bids` cannot be the ESI target.** It returns
`serde_json::json!({"slots":…, "bids":…})` (`publisher.rs:3987`), and ESI splices fragment
bytes in literally — the page would contain raw JSON where an executable script belongs.
Nothing would call `scheduleInitialAdInit`.

Add a **dedicated fragment endpoint** returning the executable script — the same shape
`build_bids_script` produces today, plus the `adSlots` assignment that moved out of the
template in Task 3 Step 2. Either that, or use the `esi` crate's fragment-response
processor to wrap the JSON; the dedicated endpoint is simpler and easier to assert on.

Three more things the naïve marker gets wrong:

- **The same-origin gate will reject it.** `page_bids_request_allowed`
  (`publisher.rs:3644`) requires `Sec-Fetch-Site: same-origin` or the `X-TSJS-Page-Bids`
  header. An internal ESI subrequest carries neither. Give the fragment endpoint an
  internal contract and a fixed backend rather than weakening that gate — it exists to
  stop third parties burning SSP quota.
- **Parent context does not propagate.** EC identity, consent state, client IP, geo, User
  Agent, and the correlation ID all live on the parent request. Forward an **explicitly
  approved allowlist** of them into the fragment request. Forwarding everything is how a
  fragment ends up more privileged than the parent.
- **Root dispatch must be suppressed.** The navigation path already dispatches an
  auction. If A3 does not suppress it, every pageview runs two — doubling SSP and APS
  spend. This applies to **A2 and A3 alike**.

- [ ] **Step 4: Validate the whole URL, not the path**

An exact-path allowlist alone permits `https://attacker.example/_ts/page-bids`. Validate
**scheme, authority, method, path, and query** — or better, ignore the marker's URL
entirely and dispatch to a fixed internal backend, treating the `esi:include` as a signal
rather than an address.

Add a test that feeds `<esi:include src="https://attacker.example/_ts/page-bids" />`
through a creative payload and asserts no outbound fetch is attempted.

- [ ] **Step 5: Deterministic synthetic fragment first**

Before wiring the real auction, point the include at a fixed-content endpoint. This
separates "does the pipeline assemble correctly" from "does the auction behave," and the
two fail very differently. Only once assembly is proven does the fragment become the real
one.

- [ ] **Step 6: Handle the flush hazard**

`esi` flushes its output writer after each parse batch. Fastly's `StreamingBody` is a
`BufWriter`, so anything between esi and it must propagate `flush()` or nothing leaves the
Wasm heap.

- [ ] **Step 7: Fragment failure must degrade, not break**

Assert that a fragment timeout or non-2xx yields a page with empty bids rather than a 5xx
or a truncated document. Note the crate's non-obvious semantics: `alt` is attempted before
`onerror="continue"`, and `<esi:try>` runs **all** attempts and concatenates every
non-failed output — it is not first-success-wins.

---

## Task 6: Safety gates — run against every arm

Not a phase. Every one of these is a hard fail, independent of any performance result.

- [ ] **Zero cross-user leakage.** Request the same URL as two synthetic users differing
      in consent state, EC identity, and geo. Assert the C2 template is byte-identical
      and that no bid, EC ID, consent string, or geo value appears in it.
- [ ] **Cold MISS, warm HIT, stale revalidation** each produce a correct page.
- [ ] **Transform failure** (the 16 MB buffer cap, a malformed body) does not insert a
      partial template into C2 and does not serve one.
- [ ] **Request collapsing** works: concurrent cold requests transform once.
- [ ] **DCA disabled**, verified by the injection test in Task 5 Step 2.
- [ ] **Exactly one auction per pageview**, from `auction_events_raw`.
- [ ] **Cookie and privacy finalization ran BEFORE assembly**, not after — EC
      `Set-Cookie` on first visit, geo suppression, and an unconditional
      `Cache-Control: private, no-store`. Headers commit before the body streams on this
      adapter, so "finalize after assembly" is not available; asserting it that way is how
      a per-user response ends up shared-cacheable. ESI's streaming mode dropping
      `$add_header` is a consequence of the same constraint, not a separate hazard.
- [ ] **Slot and bid attribution unchanged.** Same slots matched, same bids applied, same
      renders attributed. Use TS-attributed renders — the SSAT line item, non-empty
      `ts.bids`, `hb_adid` presence — **never slot fill**, which is blind to empty bids
      because `adInit` defines slots regardless.
- [ ] **No C3 — assert positively, not by absence.** Forbidding `public`, `s-maxage`, and
      `Surrogate-Control` is **not sufficient**: a bare `Cache-Control: max-age=60` passes
      that check and is still shared-cacheable, and that is exactly what the measured
      origin sends. Require instead that every assembled response carries
      `Cache-Control: private, no-store` and that `Expires`, `ETag`, `Last-Modified`, and
      all four CDN cache directives are stripped. Test it for **returning** users
      specifically — they set no EC cookie, so the cookie privacy net never fires and is
      not a backstop here.

---

## Task 7: The decision record

**Files:** `docs/superpowers/plans/2026-08-10-1009-esi-decision-record.md`

- [ ] **Step 1: Record every arm** with N, confidence interval, cache-tier mix, route mix,
      and POP. Any arm missing those is not reportable.

- [ ] **Step 2: Apply the decision rule, stated here before the data exists**

**Adopt ESI only if all three hold:**

1. Every Task 6 gate passes on A3.
2. A3 beats A2 on **bids-ready time, `adInit` fire time, and first TS-attributed creative
   paint** — by a margin the reviewers ratify **before** collection, not chosen after
   seeing the numbers. **Not root TTFB:** A2 and A3 serve the same C2 template, so their
   root timings are near-identical by construction and a difference there would be noise.
   Root TTFB is a non-regression guard only.
3. Render outcomes on A3 are non-inferior to A0.

**Otherwise adopt A2 (client-fill)** if its gates pass and it beats A1. It is portable
across all four adapters and carries no Fastly-only maintenance burden.

**Otherwise keep A1** — Stage 0 alone — and record #1009 as answered in the negative with
evidence.

The margin in (2) exists because A3's cost is not its diff. It is a second rendering
architecture, Fastly-only, on a pre-1.0 crate, in the critical render path. A small win
does not pay for that.

- [ ] **Step 3: Record what would change the answer**, so this does not get re-litigated
      from scratch. At minimum: React #418 / [#938](https://github.com/IABTechLab/trusted-server/issues/938)
      being fixed such that `adInit` can run synchronously, which is what would make edge
      assembly's round-trip saving actually worth something.

- [ ] **Step 4: Clean up.** Remove the spike flag or promote it to a real setting; purge
      C2 (`purge_surrogate_key` on `ts-template`); remove the synthetic fragment endpoint;
      and either land or delete the `esi` dependency. **A spike flag left in place becomes
      permanent configuration surface.**

---

## Reproducibility metadata

Record with every result, or it cannot be re-run or trusted: commit SHA; `esi` and
`fastly` crate versions; Fastly service and version IDs; whether the backend is shielded;
`template_ttl`; the origin's `Cache-Control` and `Vary` at collection time; assembly mode;
routes; N per arm; and the cache-tier mix.

## Out of scope

- **Stages 1–2 of the spec** as production work. This spike may build parts of the
  client-fill path to measure it; shipping it is a separate decision behind the
  correctness defects.
- **Full RSC/flight partitioning.** `rsc_flight.rs` has no static/dynamic split.
- **Publisher-authored ESI.** Breaks the no-origin-changes promise.
- **A C3 delivery cache.** Not a deferred item — a thing that must not exist.

## Definition of done

- [ ] Task 1 verdict recorded: `esi` 0.7 builds on Rust 1.95.0 / `wasm32-wasip1`, or it
      does not and the spike stopped.
- [ ] All four arms measured on one build, with correlation IDs joining server and browser
      timings, and cache tier recorded per request.
- [ ] Every Task 6 gate has an explicit pass/fail per arm.
- [ ] Decision record exists, applies the pre-ratified rule, and names what would change
      the answer.
- [ ] Cleanup complete: flag resolved, C2 purged, synthetic endpoint removed, dependency
      landed or dropped.
- [ ] All CI gates pass: `cargo fmt --all -- --check`; the six clippy targets; the four
      adapter test suites; the parity suite; JS build, test, and format; docs format.
