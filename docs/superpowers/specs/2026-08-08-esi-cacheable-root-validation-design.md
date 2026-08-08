# ESI and the Cacheable Root

**Issue:** IABTechLab/trusted-server#1009 · **Date:** 2026-08-08
**Baseline:** citations verified at `cfb98f4`; unchanged as of `b0ce56c3` (the two
commits between touch only CI workflows and Cargo aliases).

**Decision requested:** approve the four items below. Three are "yes/no"; one funds
about three days of measurement.

> **Orientation.** #1009 asks whether Edge Side Includes (ESI) can cache page fragments
> so that cacheable publisher HTML is separated from per-user ad state, recovering a
> TTFB regression that Trusted Server (TS) adds to navigations on a Next.js App Router
> publisher running on Fastly Compute. The answer is no to ESI, and the regression has a
> cheaper cause than the issue assumes.
>
> **This document deliberately carries no performance measurements.** Every conclusion
> below is derived from code at the pinned baseline, so it can be checked by reading the
> repository rather than by trusting a benchmark. Where a quantity is needed and unknown,
> it is named as unknown and [§3](#3-monday-morning) says how to obtain it.
>
> Terms used throughout: **the hold** = TS holding the HTTP response open at `</body>`
> until the server-side auction (SSAT) resolves. **#418** = a React hydration-mismatch
> defect caused by `adInit()` mutating ad-slot subtrees during hydration; it is why bid
> application is deferred to `window.load`. **The SSAT price defect** = a live
> mispricing bug named in #1009 (prices reading 100× high) — cited from #1009 and prior
> investigation, not re-verified here.

---

## 1. Decision requested

| #   | Decision                                                                                                                         | Owner needed  |
| --- | -------------------------------------------------------------------------------------------------------------------------------- | ------------- |
| D1  | **ESI is deferred.** Revival condition: #418 resolved _and_ the `window.load` gate removed. Not a rejection — a dated condition. | Eng + product |
| D2  | **Fund ~3 days of measurement** (§3). No dependencies. Can start immediately.                                                    | Eng           |
| D3  | **Approve Stage 0** — an operator flag disabling the origin cache bypass, subject to the `Vary` check in §3.                     | Eng           |
| D4  | **Stages 1–2 queue behind the SSAT price defect and #418.** Stages 3b–5 unscheduled.                                             | Product       |

Rationale for D4 in [§8](#8-priority). Everything this document recommends _against_
doing is in [§7](#7-deferred-work-specified-not-scheduled), at deliberately lower
detail than the work it recommends.

---

## 2. Why — the three findings

**ESI does not work here, for a structural reason #1009 misses.** ESI's input is pull
(`BufRead`); `lol_html`'s is push (`HtmlRewriter::write`). ESI cannot sit downstream of
the rewriter without an intermediate buffer, and in the two-stage design the cache
boundary _is_ that buffer. **ESI presupposes a TS-owned template cache** rather than
being independent of one — and that cache is blocked on purge capability TS does not
have (no `Surrogate-Key` anywhere; the Fastly management token is scoped without purge
permission). ESI is also Fastly-only at every API level. Its one advantage over a
client fetch — no round trip — is worth nothing while bids are not consumed until
`window.load`. Separately, enabling ESI's Dynamic Content Assembly would be an SSRF
vector: bid payloads carry partner-controlled creative markup, so an SSP could embed
`<esi:include src="…">` and make the edge fetch an arbitrary URL. Details in
[Appendix E](#appendix-e--esi-notes-condensed).

**The auction is already out of band; the hold is ~free.** It is dispatched _before_
the origin fetch and does not block — dispatched at [publisher.rs:2751-2755](../../../crates/trusted-server-core/src/publisher.rs#L2751-L2755), sent at [:2870](../../../crates/trusted-server-core/src/publisher.rs#L2870) —
with a 500 ms budget. The actual cost is `with_cache_bypass`
([publisher.rs:2867](../../../crates/trusted-server-core/src/publisher.rs#L2867)),
which forces every ad-eligible navigation to miss the Fastly readthrough cache.

**The two fixes are multiplicative.** Removing the bypass alone lets the previously
hidden auction surface as the new bottleneck. Removing the hold alone changes nothing,
because the auction was never the bottleneck. **Shipping the hold removal without the
bypass removal will measure no improvement and will read as the effort having failed** —
the most likely way this work gets judged unfairly.

**Ordering is established; magnitude is not.** The ordering above follows from code and
needs no measurement. The _size_ of the win does — and the one quantity it depends on,
the origin build time under `Pass`, has never been measured. #1009's timings do not
supply it: they compare cached fetches against each other, not against an origin build.
**Quote no figure to a publisher until §3 Step C runs.** Full reasoning in
[§6](#6-the-analysis).

---

## 3. Monday morning

Three checks, ordered cheapest-first. Each needs a named owner before starting.

**Step A — origin `Vary` check (minutes).** `curl` the origin with and without `RSC`,
`Next-Router-*`, and the experiment header; inspect the `Vary` response header.
**Gates Stage 0**, the only build item recommended now. Do this first because it is the
cheapest thing that unblocks anything.

**Step B — what consumes TS's own response headers (under a day).** Request a TS-served
path that already emits `public, s-maxage`
([http_util.rs:294-311](../../../crates/trusted-server-core/src/http_util.rs#L294-L311))
twice and look for `x-cache`/`age` on TS's _own_ response. **Gates the Stage 3a/3b
split** — see [§7](#7-deferred-work-specified-not-scheduled).

**Step C — measure the hold directly (1 day + a measurement window).**

The hold's cost is literally the duration of one `.await`: `collect_stream_auction` at
[publisher.rs:793](../../../crates/trusted-server-core/src/publisher.rs#L793), plus the
two EOF variants in `hold_finish_ready_segments` and `hold_finish_tail_segments`. Two
`Instant`s around it yield **`hold_wait_ms`** — the number this entire document is
arguing about, measured rather than modelled.

Emit two timings per ad-eligible navigation:

| Metric            | Why                                                                    |
| ----------------- | ---------------------------------------------------------------------- |
| `hold_wait_ms`    | **The decision.** How long the response was actually held for bids.    |
| `origin_fetch_ms` | Attribution — how much of the win Stage 0 can claim. Origin TTFB only. |

`hold_wait_ms` replaces the proxy comparison an earlier draft proposed. Comparing `O`
against `A` was an indirect way of asking "does the hold block?"; this asks it directly,
costs less to build, and removes the modelling error corrected in
[§6.2](#62-what-the-hold-actually-costs).

Deliberately not measured: auction collect duration is already instrumented
(`OrchestrationResult::total_time_ms`, `auction/orchestrator.rs:285`, flowing to
`auction_events_raw`) — read it, don't rebuild it. Rewrite duration decides nothing and
would mean touching two finalizers.

- **Mechanism: a `log::info!` line behind a debug flag, not `Server-Timing`.** A response
  header would in fact work for the origin-fetch figure — that value is known before
  headers commit — but a server-side log needs no browser harness to collect it, `log` is
  this project's instrumentation crate, and the auction path already measures itself with
  `web_time::Instant`. Gate it behind config: one line per eligible navigation is real log
  spend and the instrumentation is temporary.
- **Sample: enough navigations per arm to separate the medians with confidence**, across
  both page types, and state the N alongside any result. #1009's sample was small enough
  that its conclusion did not survive contact with the code; replacing it with another
  underpowered sample would repeat the error.

**Step C has two outcomes, both actionable:**

| `hold_wait_ms` median | Meaning                 | Effect on staging                                            |
| --------------------- | ----------------------- | ------------------------------------------------------------ |
| Near zero             | The hold is free        | Proceed as staged: Stage 0 primary, Stage 2 protects its win |
| Materially non-zero   | The hold **is** costing | **Staging inverts** — Stage 2 primary, Stage 0 secondary     |

The work does not change; its order and justification do. **The staging in §7 is
conditional on this measurement**, and the second outcome is a live possibility rather
than a formality — §6.2's argument for the first is weaker than an earlier draft claimed.

Stage 1's bids-fetch timeout still needs a measured client-side figure rather than an
invented constant, but Step C is server-side and does not supply it. Capture it from the
browser harness when Stage 1 is actually scheduled.

---

## 4. Stage 0 — the only build item recommended now

Stop bypassing the read-through cache on ad-eligible navigations
([publisher.rs:2867](../../../crates/trusted-server-core/src/publisher.rs#L2867)).

**Ship it as an operator flag, not a deletion.** Add
`publisher.bypass_origin_cache`, defaulting to today's behaviour, in the same release as
the Step C instrumentation. Then turn it off with `ts config push`.

The diff is slightly larger than deleting a line, and that is the point. The risk being
gated here is **cache poisoning** — serving one representation in response to a request
for another. For that class of failure, rollback speed dominates diff size: a config push
reverts in seconds, a release does not. The flag also buys an A/B on a byte-identical
build, removing build difference as a confound in the very measurement this depends on,
and allows flipping for a tester-cookie population before all traffic.

Retire the flag once the change has held: flip the default, then delete the setting and
its branch. A temporary flag left in place becomes permanent configuration surface.

### What to watch after the flip

Two regression signals, both checked before the win is:

- **`unexpected_origin_304` abandonment rate.** That reason
  ([publisher.rs:2894-2916](../../../crates/trusted-server-core/src/publisher.rs#L2894-L2916),
  emitted via `emit_abandoned_auction` at `:2360`) exists precisely because the ad-stack
  path refuses cached and conditional origin responses. Re-enabling the cache is what
  could revive it. **Any non-zero rate is a rollback signal** — it means a 304 is reaching
  TS that the conditional-header strip was supposed to make impossible.
- **Representation mixing.** Spot-check that HTML navigations still return HTML and RSC
  fetches still return `text/x-component`. A mismatch means the `Vary` risk materialized
  despite a PASS verdict. Roll back immediately; this is cache poisoning, not a
  performance regression.

**Why it is safe in principle.** The conditional-header strip runs 34 lines earlier
under the same gate ([publisher.rs:2832-2836](../../../crates/trusted-server-core/src/publisher.rs#L2832),
which also strips `Range`/`If-Range`), so the request already reaches the cache
unconditional and a HIT returns a full body. [The 304-prevention design](./2026-07-22-ssat-root-document-304-prevention-design.md)
added the bypass as belt-and-braces and listed the TTFB cost under its own Risks. The
strip alone satisfies its invariant.

**But it carries a risk that design never considered — and this is the blocking
precondition.** RSC fetches are not navigations
([is_navigation_request](../../../crates/trusted-server-core/src/http_util.rs#L73-L98)
requires `Sec-Fetch-Dest: document`), so they never set the bypass and **already flow
through the readthrough cache**, while HTML navigations are `PASS`. Removing the bypass
puts both representations under one cache key. #1009 states the origin varies on
`rsc`, `next-router-*`, and a publisher-specific experiment header — if that variance is
not declared via `Vary`, the
cache can serve a flight payload to an HTML navigation.

The classification is also not airtight: `is_navigation_request` falls back to the
`Accept` header when Fetch Metadata is absent, and its own comment warns _"this path is
weaker — `fetch()` can set Accept: text/html"_
([http_util.rs:84-88](../../../crates/trusted-server-core/src/http_util.rs#L84)).

**A FAIL is not merely a Stage 0 blocker — it is a live production defect.** RSC fetches
already transit the read-through cache today, because they never set the bypass. If the
origin varies on `Next-Router-*` without declaring it, TS is cross-serving RSC variants
right now. On a FAIL, file that immediately and treat "ask the origin to declare `Vary`"
as urgent rather than as the cheaper of two options.

**The `Vary` check is necessary but not sufficient.** Turning the read-through cache on
for HTML navigations exposes three things a representation check does not cover, and all
three are a larger class than the RSC split:

- **Client `Cookie`.** TS forwards client cookies to origin unchanged — there is no
  `COOKIE` strip on the publisher path. Any cookie-personalized HTML (logged-in state,
  paywall meter, publisher-side A/B assignment) becomes cross-servable unless the origin
  declares `Vary: Cookie` or marks those responses private.
- **Origin `Set-Cookie`.** If the origin emits `Set-Cookie` alongside a shared-cacheable
  `Cache-Control`, the read-through cache can replay one visitor's cookie to the next.
  TS's own privacy net downgrades **TS's** response — it runs after the cache has already
  stored the origin's.
- **`Authorization`.** #1009 describes a basic-auth-gated deployment. Responses to
  authorized requests entering a shared cache needs its own check.

So Step A must capture `Cache-Control` and `Set-Cookie` too, and repeat each request with
and without a session cookie. Same minutes of work; closes the bigger hole.

**Two effort branches, and Step A decides which:**

| Step A result          | Stage 0 is…                                   | Effort |
| ---------------------- | --------------------------------------------- | ------ |
| Origin declares `Vary` | the flag, its tests, then a config push       | 1–2 d  |
| Origin does **not**    | a TS-side cache-key discriminator — a feature | 4–8 d  |

The discriminator is the safer design either way, because it keys on the headers that
actually distinguish the representations rather than on the navigation classification.

**Two benefits beyond TTFB, worth stating to a publisher:**

- **Origin load drops.** The 304-prevention design explicitly accepted _"increasing
  origin load"_ as a cost. This reverses it.
- **`stale-if-error` becomes reachable.** Under `Pass` an origin outage is a hard
  failure. This needs a decision rather than a default: stale HTML carries stale slot
  markup, and whether that beats an error is a product call.

---

## 5. The trap in the deferred work — read this before scheduling Stages 1–2

The hold is load-bearing for something other than latency. The invariant is:

> `ad_bids_state` must be `Some(..)` when `lol_html` processes the `<body>` end tag.

The end-tag handler ([html_processor.rs:381-395](../../../crates/trusted-server-core/src/html_processor.rs#L381-L395))
locks that mutex once and falls back to `build_empty_bids_script()` on `None`.

**Removing the hold without relocating collection renders a normal page with
`tsjs.bids = {}` and no server-side ads** — no error, no non-2xx, no ERROR log. On
Axum, Cloudflare, and Spin the loss is fully silent:
[publisher.rs:2248](../../../crates/trusted-server-core/src/publisher.rs#L2248) holds a
bare `Option<DispatchedAuction>` with no guard, so not even a drop warning fires. **The
SSPs are billed regardless.**

This is why Stage 2 is gated on three companions and a production soak, and why slot
fill cannot be the canary — see [§7](#7-deferred-work-specified-not-scheduled).

---

## 6. The analysis

### 6.1 Corrections to #1009's premises

| #   | #1009 states                           | Verified against `cfb98f4`                                                                                                                                                         |
| --- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Two per-user injection seams           | `tsjs.adSlots` is per-URL — [build_slot_json](../../../crates/trusted-server-core/src/publisher.rs#L3501-L3525) emits config- and path-derived fields only. **One per-user hole.** |
| 2   | Identity off-inline is a prerequisite  | Privacy net is cookie-gated and returning navs set no cookie ([ec/finalize.rs:86-94](../../../crates/trusted-server-core/src/ec/finalize.rs#L86-L94)). **First-visit only.**       |
| 3   | Stamp at `:2882-2888`                  | [`:2945-2963`](../../../crates/trusted-server-core/src/publisher.rs#L2945-L2963), `private, no-store`, also removing `ETag`/`Last-Modified`/four CDN headers. **Seven headers.**   |
| 4   | Three cacheability killers             | Two more: `bypass_cache` and the [304→502 guard](../../../crates/trusted-server-core/src/publisher.rs#L2894-L2916). **The bypass is the cost.**                                    |
| 5   | Two `!Send` pipelines is the hard part | `?Send` already pervasive. **Not the obstacle.**                                                                                                                                   |
| 6   | Goal: root as a shared Fastly HIT      | Nothing caches TS's own response on Compute; the A/B's `x-cache` is the **backend readthrough** cache. **Reframes the goal.**                                                      |

Rows on #1009's `esi` compatibility check (holds), its drifted line numbers, and its
two broken `#1`/`#3` cross-references are in [Appendix A](#appendix-a--full-1009-correction-table).

**Credit where due.** #1009 names the hold as blocker 1 and states it correctly. What
changes here is its _causal weight_. Likewise, #1009's own observation that TS _"shifts
the auction cost from client-side to server-side rather than adding new work"_ is the
argument for client-fill, which the issue then declines in favour of ESI.

### 6.2 What the hold actually costs

**An earlier draft of this section claimed a stronger argument than the code supports.
It was wrong, and the correction matters.**

The hold does not key off `lol_html` at all. `BodyCloseHoldBuffer::push`
([publisher.rs:2190-2202](../../../crates/trusted-server-core/src/publisher.rs#L2190))
scans the **decoded origin input** for `</body`, and `hold_collect_close_tail` awaits
`collect_stream_auction` ([publisher.rs:793](../../../crates/trusted-server-core/src/publisher.rs#L793))
the moment that byte sequence appears — with `is_last = false`, before post-processing
runs. Post-processor buffering is irrelevant to when the hold fires, so the argument
applies identically to every publisher or to none.

What is true, and all that is true:

> Dispatch precedes the origin fetch, so the hold costs `max(0, A − T)`, where `A` is the
> auction collect duration and `T` is origin TTFB plus body transfer up to the `</body>`
> byte. Since `</body>` sits at the end of a document, `T` is close to the full download.

`A` is bounded by `auction_timeout_ms`, resolved as
`creative_opportunities.auction_timeout_ms` falling back to `auction.timeout_ms`
([publisher.rs:2680-2684](../../../crates/trusted-server-core/src/publisher.rs#L2680-L2684))
— check the resolution order against your own config rather than trusting a number; the
shipped example sets different values at each level.

**This is a claim requiring measurement, not a proof.** §3 Step C measures the hold's
cost directly rather than inferring it.

A finding that does survive, and belongs with [the ceiling](#64-the-ceiling): because
`HtmlWithPostProcessing` withholds all output until the final chunk, the streaming-prefix
design at [publisher.rs:1343-1348](../../../crates/trusted-server-core/src/publisher.rs#L1343-L1348)
— whose comment promises "the client receives the document up to `</body>` while the
auction rides alongside transfer" — is **inert on a Next.js publisher**. Every
`step.ready` yields empty bytes. That comment is misleading on exactly the publisher
under discussion.

### 6.3 The quantity nobody has measured

Write the fetch time under `Pass` as `O`. Recovery depends on it, and it has never been
captured. #1009's timings cannot supply it: they compare a POP hit against a
shield-served fetch, both of which are _cached_ paths, whereas `CacheOverride::Pass`
bypasses TS's read-through cache and its shield.

Note `Pass` bypasses **TS's** caches only. It has no authority over any CDN the publisher
runs in front of their own origin — and #1009's `x-cache: MISS, MISS` on the TS-on arm
hints one may exist. So `O` may not be origin build time at all. Since `O` is the single
quantity this model depends on, that ambiguity is worth resolving in Step C rather than
assuming.

What follows from code alone, without any number:

| Configuration       | Long pole after the change | Recovery                       |
| ------------------- | -------------------------- | ------------------------------ |
| Hold removal only   | origin (still `PASS`)      | **none**                       |
| Bypass removal only | the auction budget         | partial — the auction surfaces |
| **Both**            | the rewrite                | **the full available win**     |

That ordering is what the staging rests on, and it is measurement-independent. The
magnitude of each row is not, and §3 Step C supplies it.

### 6.4 The ceiling

#1009 targets "approach the TS-off warm numbers." **Unreachable, structurally.** Those
numbers are TS-off _streaming_ a POP HIT. TS buffers the whole document before emitting
a byte (16 MB cap), so its floor is `full origin body download + full rewrite` — above a
streamed hit by construction, whatever the timings turn out to be. Set the target from
Step C's measured rewrite cost rather than from the TS-off baseline. Going below the
floor requires true origin streaming (#849), out of scope. A non-Next.js publisher with
no post-processor takes the streaming path and would see a lower floor.

### 6.5 Confidence

**High on the structural claims.** §6.2's argument, the bypass forcing a cache miss, the
ESI push/pull mismatch, the silent-empty-bids failure mode, the geo and purge blockers,
and the fill-canary blindness are all read directly out of the code at `cfb98f4`. Anyone
can check them without running anything.

**None on magnitude.** `O` is unmeasured and the rewrite cost is unmeasured. This
document does not estimate them, and no figure in it should be quoted as one.

Worth stating plainly: #1009 reached the opposite causal conclusion from a small sample.
That is a caution about small samples generally, not only about that one — which is why
§3 Step C specifies the measurement rather than this document supplying a substitute
for it.

---

## 7. Deferred work, specified not scheduled

Lower detail is deliberate. Full specifications are in the appendices.

**Stage 1 — bid delivery off the response body.** The client fetches `/_ts/page-bids` at
navigation generation 0. Endpoint, same-origin gate, wire shape, and client consumer
already exist. Three decisions must be made before planning: the `slots: []` precedence
rule when head-open already injected a non-empty `ts.adSlots`; the new terminal-event
emission point; and whether the dispatch/collect split survives at all. Plumbing detail
in [Appendix B](#appendix-b--stage-1-plumbing-condensed). Estimated 8–13 d, low-to-medium
confidence, uncertainty concentrated client-side.

Three companions are mandatory, not optional: **suppress the server bids script
entirely** (not an empty one), **fail loud** (the end-tag handler takes bids by value so
a missing auction is a compile error), and **relocate telemetry** (navigation
`Completed` rows are emitted only from the collect functions, and the `ts-debug` dump
rides the same string). Behaviour change to accept: under client-fill the auction runs
only if the browser executes the fetch, so bots and JS-disabled clients stop triggering
server-side auctions — revenue-relevant, sign unknown.

**Stage 2 — delete the hold.** 5–8 d. **Rollback is one-way**: it deletes the hold, the
dispatch/collect split, and twelve tests, so the only revert is a release. Ships only
after Stage 1 has run flag-on in production for a window defined _before_ Stage 1
starts, with TS-attributed renders flat and `auction_events_raw` navigation rows intact.
Secondary wins: removes the duplicated per-codec decoder/encoder wiring, six compression
imports, and the non-parser-context `</body` byte-scan false positive (#850).

**Regression gate — do not use slot fill.** `adInit` calls `defineSlot` and
`addService` regardless of bids ([gpt/index.ts:652](../../../crates/trusted-server-js/lib/src/integrations/gpt/index.ts#L652),
`const bid = bids[slot.id] ?? {}`), so GAM still requests and fills from its own demand.
#1009 measured identical fill on both arms, confirming fill is not TS-attributable. Use
SSAT reservation line-item renders, non-empty `ts.bids`, and `hb_adid` presence
instead. Test inventory in [Appendix D](#appendix-d--test-inventory).

**Stage 3a — browser caching.** 2–4 d. Replace `no-store` with `private, max-age=N`
plus a TS-minted `ETag`. `private` keeps it out of shared caches, so none of the geo or
`Vary` work applies. Restores revalidation and bfcache eligibility the 304-prevention
design traded away — that rationale is **retired, not outweighed**: once bids leave the
body the document is no longer per-user. Record it as a deliberate decision.

**Stage 3b — shared cacheability.** 5–10 d, and **blocked on two things**. First, geo:
`apply_finalize_headers` writes IP-derived `x-geo-*` on every response
([fastly/middleware.rs:194-200](../../../crates/trusted-server-adapter-fastly/src/middleware.rs#L194-L200)),
which shared-cached replays one visitor's geo to the next; geo is not a request header
so suppression is the only option. Second, `Vary`: the publisher path emits none, and at
least eleven request signals change the rewritten bytes for one URL — five are per-user
and can never be shared-cached ([Appendix C](#appendix-c--vary-signals-condensed)).
**Also gated on Step B**: if nothing consumes TS's response headers, this tier is inert
until a topology change.

**Stage 4 — purge capability.** Not sized. Prerequisite for anything beyond the backend
readthrough cache. TS today has no `Surrogate-Key` emission and no purge permission, so
any TS-owned cache is TTL-only and a config push takes up to one TTL to take effect.

**Stage 5 — TS-owned template cache, then ESI.** Not sized, and gated on D1's revival
condition.

**Identity needs no work.** A new visitor's first navigation sets the EC cookie and the
privacy net downgrades that one response; every later navigation sets no cookie and is
cacheable. First-visit parity is a non-goal; if ever wanted, move cookie issuance onto
the `/_ts/page-bids` response — natural under client-fill, **not available under ESI**,
whose streaming mode drops `$add_header`.

**Hydration — do not build a client shim.** URL reconciliation is already solved
server-side (`rsc_flight.rs` plus `integrations/nextjs/`, ~4,100 lines) by rewriting
hydration's _input_; a shim would be a second source of truth producing the exact
mismatch both exist to prevent. Late-bid rendering has no foothold and the obvious
interception point is measured-unsafe: a controlled capture found the `__next_f` gate
reproducing #418 on every run and destroying the creative on half of them — it patches
`__next_f.push` shortly after React's first commit, while hydration continues for
thousands more. Two retractions to carry forward: that gate is
measured-unsafe rather than merely unproven, and "#418 at ~5% and not impression-costing"
is retracted — it came from pages whose slots are not React-owned. Note
`docs/superpowers/specs/2026-07-24-adinit-hydration-gate-design.md` exists only on the
unmerged branch `958-adinit-hydration-chunk-gate`, so `publisher.rs:3461-3464` points at
a path that 404s in a fresh checkout.

---

## 8. Priority

**Run §3 Steps A–C now, regardless of everything else.** Under three days combined, and
useful independent of this effort. Step C's instrumentation is deliberately temporary and
config-gated; if these timings become a standing regression gate, the right home is the
access-log telemetry already scaffolded but unwired in `TinybirdSettings`
(`settings.rs:1718-1731` — `access_enabled`, `access_dataset`, and a sample rate, with the
comment that it is _"rejected until an access-log emitter is wired"_). That is a
follow-on, not part of this work.

**Stage 0 next**, shipped as the operator flag in §4 rather than a deletion. Gated on a
`curl`, reverses an origin-load cost the prior design explicitly accepted, and rolls back
with a config push. Closer to a defect fix than an optimization — TS opted out of a cache
it did not need to opt out of.

**Stages 1–2 queue behind the correctness defects.** Their failure mode is silent
revenue loss, against a publisher whose ads currently fill reliably. The SSAT price
defect misprices live auctions, and **a slow correct auction loses less money than a
fast wrong one**. #418 sits ahead too: Stage 1's join gate must be reconciled with
whatever hydration gate lands, and doing that twice is waste.

**Stages 3b–5 are not competitive** on current evidence and should not be scheduled.

---

## 9. Decisions needed from this review

1. **`slots: []` precedence** — which slot list wins when page-bids returns empty under
   consent denial while head-open already injected a non-empty `ts.adSlots`? Blocks
   Stage 1 planning.
2. **Is branch `958-adinit-hydration-chunk-gate` landing?** Its gate and Stage 1's join
   gate must be reconciled by whichever lands second.
3. **Does D4's priority ordering hold?** This is the call the document most needs a
   human to make.

Implementation-level open items for unscheduled work are in
[Appendix F](#appendix-f--deferred-open-items-condensed).

---

## Appendix A — full #1009 correction table

Rows 1–6 are in [§6.1](#61-corrections-to-1009s-premises). The remainder:

| #   | #1009 states                       | Verified                                                                                                      |
| --- | ---------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| 7   | `esi` 0.7.1 compatible, greenfield | Confirmed. Published 2026-06-18, MIT, depends on `fastly = "^0.12"`, unifies with the locked `fastly 0.12.1`. |
| 8   | Appendix line numbers              | Drifted v394 → `cfb98f4`. Pin to a SHA.                                                                       |
| 9   | Cross-refs `#1` / `#3`             | GitHub auto-linked them to unrelated PRs. Fix before anyone follows them.                                     |

**`should_run_ad_stack` carries four meanings across six sites:**

| Line                                                                        | Meaning                                                         | Disposition                                                                          |
| --------------------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| [2651](../../../crates/trusted-server-core/src/publisher.rs#L2651), `:2660` | eligibility and auction gate                                    | keep                                                                                 |
| [2832-2836](../../../crates/trusted-server-core/src/publisher.rs#L2832)     | strip `If-None-Match`, `If-Modified-Since`, `Range`, `If-Range` | **keep** — needed for any injection                                                  |
| [2866](../../../crates/trusted-server-core/src/publisher.rs#L2866)          | `with_cache_bypass()`                                           | **make operator-controlled** — [§4](#4-stage-0--the-only-build-item-recommended-now) |
| [2894](../../../crates/trusted-server-core/src/publisher.rs#L2894)          | 304 → 502 guard                                                 | keep as safety net                                                                   |
| [2920](../../../crates/trusted-server-core/src/publisher.rs#L2920)          | build `adSlots`                                                 | keep — per-URL                                                                       |
| [2945](../../../crates/trusted-server-core/src/publisher.rs#L2945)          | strip cacheability                                              | **replace** — Stage 3a                                                               |

**Cache tiers.** T1 backend readthrough (already available; TS opts out). T2 TS-owned
template cache (adds KV latency, eventual consistency, a full invalidation design).
T3 delivery cache of TS output (Fastly topology change; **ESI cannot run**, since
Compute is not invoked on a HIT). T2 and T3 both introduce a cache TS cannot purge.

---

## Appendix B — Stage 1 plumbing (condensed)

Full detail lives in the plan when Stage 1 is scheduled. The decisions that must be made
before any of it is written:

- **Suppress the server bids script entirely** under client-fill, not emit an empty one.
  The body end-tag handler is gated on `has_slots`, which stays true; the gate must become
  "did this response carry bids."
- **`adInit` snapshots bids at call time** (`gpt/index.ts:566`) and applies `hb_*`
  targeting at `:657-661`. Bids arriving later are lost, so
  `installScheduleInitialAdInit` must become a hydration-ready AND bids-settled join with
  a bounded timeout that fires untargeted rather than stranding the slot.
- **Do not route the initial load through `onNavigate`** — `gpt/index.ts:949` increments
  `navGeneration` and cancels generation 0.
- **`handle_page_bids` is not a drop-in.** It runs a fresh `run_auction`
  (`publisher.rs:3903`) tagged `AuctionSource::SpaNavigation` (`:3859`) and cannot reuse
  in-flight dispatched requests. Needs dispatch suppression (or spend doubles), a new
  `AuctionSource` **plus the mechanism that delivers it**, and a `slots: []` precedence
  rule (`:3975-3985`).
- **Telemetry moves with it.** Navigation `Completed` is emitted only from the collect
  functions (`publisher.rs:2410`, `:2456`); `Abandoned` only via `emit_abandoned_auction`
  (`:2360`). The `ts-debug` dump rides the same `ad_bids_state` string (`:2478`) and
  disappears with the hold.
- **Sequencing is strict:** decide bid delivery, then whether dispatch/collect survives,
  then delete the hold. Any other order produces the silent failure in [§5](#5-the-trap-in-the-deferred-work--read-this-before-scheduling-stages-12).

---

## Appendix C — `Vary` signals (condensed)

Needed for Stage 3b only, which is unscheduled.

At least eleven request signals change the rewritten bytes for one URL. **Five are
per-user and can never be shared-cached:** consent state (`euconsent-v2`, `__gpp`,
`__gpp_sid`, `us_privacy`, `Sec-GPC`, IP-derived jurisdiction); the GPT-diagnostics
`__Host-ts-console` cookie; the `tsjs.bids` payload (removed by Stage 1, which is what
makes the rest tractable); IP-derived geo; and DataDome's request filter, which can
replace the document entirely.

**Six are per-variant and safe in a cache key:** request host and scheme,
`Accept-Encoding`, request-class headers, the origin `Content-Type` fork, the enabled
integration set, and the tsjs content hash.

Two pre-existing holes worth filing regardless of this work: the consent-denied / bot /
prefetch / no-slot variant keeps the origin's cacheability while carrying per-user
`x-geo-*`; and the RSC-versus-HTML split is unguarded because TS never reads `RSC` or
`Next-Router-*`.

Invalidation signals TS has today: config push — none; consent change — request-side,
belongs in the cache key; experiment rollover — origin-side only; article edit — origin
`Cache-Control`; tsjs rebuild — content hash already in the URL; integration toggle —
none.

---

## Appendix D — test inventory

**Coverage gaps to close before Stage 2, not after.** No test exercises the hold
together with post-processors — `streaming_html_with_post_processors_rewrites_body`
(`publisher.rs:7966`) and `document_state_placeholders_substitute_through_accumulating_path`
(`publisher.rs:8043`) both pass `dispatched_auction: None`, which is exactly the Next.js
configuration in question. No parity coverage of the hold or bids injection either;
`parity.rs` has ten multi-adapter tests including `publisher_proxy_fallback_parity`
(`:762`), but none reaches the hold — extend that rather than building a second harness.

`geo_header_parity_on_all_responses` (`parity.rs:613`) encodes the all-responses
invariant Stage 3b narrows, but currently covers only
`/.well-known/trusted-server.json`, `POST /auction`, and `POST /verify-signature`, and
asserts the boolean `x-geo-info-available` rather than per-user values — so it may need
no change. Check deliberately.

**Twelve `publisher.rs` tests change with hold removal:** four die (`:5492`, `:5546`,
`:5630`, `:5649`); three assert hold-injected bids (`:6915`, `:6979`, `:7748`); two FCP
guards go vacuous (`:7306`, `:7341`); three are conditional on dispatch/collect
(`:5358`, `:7046`, `:7575`). Dead helpers: `ChunkedReader` (`:4226`),
`RecordingProcessor` (`:4252`).

`html_processor.rs:1601` and `:1636` pre-populate the mutex directly — they stay green
while production injects empty bids, which is precisely why the fail-loud companion
exists.

---

## Appendix E — ESI notes (condensed)

For if and when [D1](#1-decision-requested)'s revival condition is met. Expand then;
recording only what would otherwise be re-derived:

- Pin `esi = "0.7"`. Pre-1.0, irregular cadence, two yanked betas in the 0.7 line.
- **Use `process_stream`, not the wrappers.** `process_response` and
  `process_response_streaming` consume `self` _and_ send the response themselves, taking
  ownership away from the finalize / `ec_finalize` ordering.
- **Order esi → lol_html**, never the reverse, via a newtype implementing `io::Write`.
  Mind the `StreamingBody`-is-a-`BufWriter` hazard: esi flushes per parse batch, so any
  adapter in between must propagate `flush()`.
- **Always supply a custom fragment dispatcher.** The built-in one builds a dynamic
  backend per URL host and panics on a hostless URL; dynamic backends are also the known
  Viceroy local-dev failure mode here.
- **DCA off, asserted explicitly** — not merely left at its default. Rationale is the SSRF
  vector in [§2](#2-why--the-three-findings): partner-controlled creative markup would
  become ESI-executable at the edge.
- **`<esi:try>` runs _all_ attempts and concatenates every non-failed output** — not
  first-success-wins, so primary/fallback pairs render both. Least obvious behaviour in
  the crate.
- Single include, not per-slot: the auction is one operation producing all slots' bids.

---

## Appendix F — deferred open items (condensed)

Implementation-level, for unscheduled work only. Decisions needing a human are in
[§9](#9-decisions-needed-from-this-review).

Should `collect_non_html_auction` (`publisher.rs:2388`) go with the hold or stay? Is
`body_close_hold_loop_stream` (`:2109`, no production caller) safe to delete, or is the
buffered-adapter streaming cutover (#495) still live? Does hidden-tab rAF behaviour
interact badly with a bids timeout? What are Fastly's pending-request semantics when a
`DispatchedAuction` drops mid-flight? Does `stale-if-error` on a cached root serve
acceptable content given stale slot markup? And the googletag shim discards listeners
queued before it loads (#1009 Part 1) — not filed, should be.

---

## Appendix G — code-grounded seams

All pinned to `cfb98f4`.

| Concern                                     | Location                                                                        |
| ------------------------------------------- | ------------------------------------------------------------------------------- |
| Eligibility decision                        | `publisher.rs:2651`, `:2660`                                                    |
| `is_navigation_request`                     | `http_util.rs:73-98`                                                            |
| Auction dispatch (pre-origin, non-blocking) | `publisher.rs:2698-2760`                                                        |
| Auction overlap intent                      | `auction/orchestrator.rs:950-952`                                               |
| Auction timeout resolution                  | `publisher.rs:2680-2684` (creative_opportunities, else auction.timeout_ms)      |
| Conditional/range header strip              | `publisher.rs:2832-2836`                                                        |
| Origin cache bypass                         | `publisher.rs:2866-2868`                                                        |
| Origin 304 → 502 guard                      | `publisher.rs:2894-2916`                                                        |
| `adSlots` build (per-URL)                   | `publisher.rs:2920`, `:3558-3577`, `:3501-3525`                                 |
| Uncacheable stamp                           | `publisher.rs:2945-2963`                                                        |
| `</body>` hold — sync / async / Fastly lazy | `publisher.rs:2235` / `:2109` / `:1318-1390`                                    |
| Hold buffer                                 | `publisher.rs:2177-2218`                                                        |
| Auction collect (HTML / non-HTML)           | `publisher.rs:2431` (emits `:2456`) / `:2388` (emits `:2410`)                   |
| Abandonment emitter                         | `publisher.rs:2360`                                                             |
| Bids script build                           | `publisher.rs:3438-3491`                                                        |
| `/_ts/page-bids`                            | `publisher.rs:3611`, handler `:3723`, auction `:3903`                           |
| Injection seams (head / body-close)         | `html_processor.rs:310-363` / `:381-395`                                        |
| Post-processor buffering                    | `html_processor.rs:62-94`                                                       |
| Next.js post-processor registration         | `integrations/nextjs/mod.rs:107`                                                |
| Max buffered body (16 MB)                   | `settings.rs:77-79`                                                             |
| EC cookie issuance policy                   | `ec/finalize.rs:86-107`                                                         |
| Cookie-privacy net                          | `response_privacy.rs:20-61`                                                     |
| Geo response headers                        | `adapter-fastly/src/middleware.rs:194-200`                                      |
| Cacheable-header precedent                  | `http_util.rs:294-311`                                                          |
| No purge permission                         | `adapter-fastly/src/management_api.rs:12`                                       |
| Client initial-ad gate                      | `js/lib/src/integrations/gpt/index.ts:536-555`                                  |
| `adInit` bid application                    | `js/lib/src/integrations/gpt/index.ts:566`, `:652`, `:657-661`                  |
| Client SPA auction hook                     | `js/lib/src/integrations/gpt/index.ts:806`, `:859`, `:892-949`                  |
| Prior design that introduced the killers    | `docs/superpowers/specs/2026-07-22-ssat-root-document-304-prevention-design.md` |
