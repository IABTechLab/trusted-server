# Streaming assembly: the architecture #1009 actually needs

**Date:** 2026-08-11
**Status:** Decision record. Supersedes the delivery half of the
[ESI validation spike](../archive/2026-08-10-1009-esi-validation-spike.md); the cache half
stands.
**Issue:** IABTechLab/trusted-server#1009

> **Implementation update, 2026-08-14.** Warm C2 hits still use Design C exactly as
> specified here. Authorized cold misses now also validate the repaired
> `stackpop/esi` parser, pinned by commit: the inert C2 marker becomes one synthetic
> include only in a private working copy, resolved from the already-built reader
> fragment without an HTTP request. Parser failure falls back to the byte seam. See
> [the hybrid implementation design](./2026-08-14-1009-esi-parser-assembly-design.md).
> Sections 2–2c and Design B remain investigation history; the native self-subrequest
> design is still not implemented.

---

## 1. The correction this document exists for

An earlier reading of the latency, recorded in
[the measurement findings](../plans/2026-08-08-1009-measurement-findings.md), said the
`</body>` hold costs approximately nothing and the whole cost is the origin-cache bypass.

That is true **today, and only today.** It is true for a reason that stops holding the
moment the rest of this work lands:

|                                     | Origin fetch | Auction              | Reader waits |
| ----------------------------------- | ------------ | -------------------- | ------------ |
| Today                               | ~650 ms      | hidden inside it     | ~650–800 ms  |
| Cached root, **buffered** assembly  | 0            | fully exposed        | ~auction cap |
| Cached root, **streaming** assembly | 0            | overlapped with send | ~ms          |

The auction is dispatched before the origin fetch and both run concurrently, so the hold
costs `max(0, auction − origin)` — zero while the origin is slow. Make the root cacheable
and the origin fetch disappears; the auction then has nothing left to hide behind and
becomes the _entire_ remaining cost.

**So the two problems are coupled, and neither fix shows a win alone.** That is why the
issue is right to treat both as prerequisites, and why measuring one at a time misleads.

Two distinct problems get bundled in the issue as one blocker. They need different fixes:

1. **Bids live in the response body** → the page is _uncacheable_. Fixed by templatizing.
   **Done.**
2. **The response is held for the auction** → the page is _slow_. Fixed by streaming the
   shell and filling the seam late. **Not done** — this document.

## 2. What the current implementation gets wrong

On a C2 hit, `collect_and_assemble_cached_template` awaits the auction, then assembles,
then returns a fully buffered `PublisherResponse::Buffered`. The reader receives nothing
until bids resolve.

That relocates the hold rather than removing it, and on a hit it is _worse than today_ in
one respect: there is no origin fetch left to hide it behind, so the full auction latency
lands on first byte.

The routing decision that caused it — shared modes take the buffered finalizer — was made
because **a store needs complete transformed bytes.** True on a miss. Irrelevant on a hit,
where the template is already materialized.

## 2b. Demonstrated, not argued

Run locally under `viceroy serve` against a stub origin with a **self-imposed 1.5 s bid
endpoint**. These are synthetic numbers from a delay chosen to be observable — not a
measurement of any real deployment, and not comparable to publisher data.

| Request | Cache | TTFB      | Total     | Origin fetched |
| ------- | ----- | --------- | --------- | -------------- |
| 1       | miss  | ~injected | ~injected | yes            |
| 2       | hit   | ~injected | ~injected | **no**         |
| 3       | hit   | ~injected | ~injected | **no**         |

Two things are visible, and both matter more than the absolute values:

1. **The cache works.** One origin fetch across three requests; the C2 log shows one
   miss, one store, two hits.
2. **The reader waits exactly as long anyway.** Time-to-first-byte equals total on every
   request, so nothing streams — the entire response lands at once, after the auction.
   On the hits the origin fetch is gone and first byte still tracks the injected bid
   delay.

That is the claim in §1 and §2 reproduced on demand: a cached root delivers **no latency
benefit to the reader** while the response is held for the auction. It also gives the
harness a pass/fail shape for the change this document proposes — under streaming
assembly, TTFB must fall away from total by approximately the injected delay.

## 2c. Measured against the shipped path — a ~100x TTFB regression

`scripts/c2-local-test.sh` runs both modes against the same stub, with a self-imposed
1.5 s bid endpoint. Synthetic numbers, not a measurement of any deployment.

| Mode                      | TTFB              | Total   |
| ------------------------- | ----------------- | ------- |
| `inline` (shipped)        | **0.010–0.019 s** | ~1.51 s |
| `esi` (buffered assembly) | **1.524–1.532 s** | ~1.53 s |

**The shipped path already streams correctly.** First byte in ~10 ms; the article paints
while the auction runs; only `</body>` waits. Buffered assembly turns that into a wait
for the whole auction before the first byte — roughly **100x worse TTFB than doing
nothing**.

This corrects §1 and §2, which framed buffered assembly as capturing the origin-fetch
saving and merely failing to add the streaming benefit. It is worse than that: it
**removes** a benefit today's code already delivers. The origin-fetch saving is
irrelevant beside losing the stream.

It also sharpens where production's latency actually goes. Locally the stub origin
answers in ~2 ms, so `inline` TTFB is ~10 ms. In production the origin fetch is slow and
uncached, and TS cannot send a first byte until the origin sends one — so production TTFB
is the **origin fetch**, with the auction hidden behind the remainder of the body plus the
`</body>` hold. The fix is therefore a fast origin _while keeping the stream_: exactly
Design C, and exactly what buffered assembly gives up.

**Consequence for the plan:** `esi` mode must not be exposed to any traffic in its current
form. It is not a smaller win than hoped, it is a regression.

### A harness bug worth recording

The first version of this comparison reported `inline` fetching the origin zero times —
nonsense that still printed four passes. Viceroy was launched inside a subshell, so `$!`
was the subshell rather than the server; cleanup killed the wrapper and orphaned viceroy.
The next run then failed to bind and **silently answered from the previous run's process**,
carrying that run's config and warm cache.

A harness that answers from the wrong server is worse than one that crashes, because its
output looks like data. Fixed with no subshell, a pre-flight port check, and a startup
wait that fails loudly. The regression above was invisible until the control worked.

## 3. The decisive facts

Three, all verified in the codebase rather than assumed:

1. **The existing streaming path already implements stream-then-stall-at-the-seam.**
   `publisher.rs` builds an `async_stream::try_stream!` that streams body chunks and holds
   **only** at `</body>` for the auction (`hold_auction`, `AuctionHoldState`). This is
   shipping behaviour, not new work.
2. **`EdgeBody::Stream` is an async stream** — consumers call `stream.next().await` — so an
   `await` may sit between chunks. Nothing needs a nested executor.
3. **`BodyCloseInjection::Marker(String)` already exists**, and the streaming finalizers
   already strip `Content-Length`.

## 4. Three designs

|                                     | Streams | Auctions       | Requires                 | Adapters  |
| ----------------------------------- | ------- | -------------- | ------------------------ | --------- |
| **A** — buffered assembly (current) | No      | 1              | nothing                  | Fastly    |
| **B** — native ESI subrequest       | Yes     | 1, in fragment | self-referencing backend | Fastly    |
| **C** — cached shell + seam split   | Yes     | 1              | nothing                  | **All 4** |

### Design B, for the record

`PendingFragmentContent::PendingRequest` is what the `esi` crate is built for: the
dispatcher fires a real subrequest and the processor blocks on the handle. Fastly's
`send_async`/`wait` is **synchronous**, so this sidesteps the sync-dispatcher problem
without any executor.

It also vindicates the _original_ dispatch gate. Under B the root must **not** dispatch,
because the fragment request runs the auction. The later reversal to
`root_auction_is_useful(Esi) = true` is correct for buffered assembly and wrong for
streaming. **Dispatch-usefulness is a function of the delivery mechanism**, which is the
non-obvious coupling in this design space.

### Design C — the recommendation

The template carries an **inert HTML comment sentinel** where the reader's ad slots and
bids go, emitted by the existing `Marker` variant:

```
<!--ts-ad-seam-->
```

On a C2 hit:

```
commit headers (private, no-store; no Content-Length)   ← must precede any byte on Fastly
stream template[..sentinel]                             ← the article paints here
await the auction                                       ← the only stall, at the very end
write the bids script
stream template[sentinel+len..]
```

Since a hit has the whole template in hand, this is a `split_once`, not a streaming
search. Three yields from a `try_stream!`.

**Why a comment sentinel rather than a byte offset in metadata.** An offset is O(1), but
capturing it means plumbing the writer position into a `lol_html` end-tag handler, and it
does not survive re-encoding. A `find` over a ~100 KB buffered template is free by
comparison.

**Why a comment rather than executable ESI markup.** An HTML comment is inert. If
assembly ever fails to substitute, the reader sees nothing; an unresolved ESI include
tag renders as visible text. Failure degrades to "no ads" instead of "broken page".

**Why not re-run `lol_html` over the cached template.** It would inject a second tsjs
`<script>` at `<head>` and re-rewrite already-rewritten URLs. The hit path must do the
seam and nothing else.

## 5. Two simplifications that fall out

**Store the template decoded.** The current key includes `accept_encoding` because the
pipeline pairs input to output encoding. If C2 stores identity bytes and encodes at serve
time, that key dimension disappears: one entry per URL instead of one per encoding —
better hit rate, smaller key, and one fewer thing to get wrong.

**`Content-Length` must go.** The assembled length is not known until bids resolve. The
streaming finalizers already strip it; `build_cached_template_response` currently _sets_
it and must stop.

## 6. What this means for #1009

The issue's gating decision is: _is Fastly-first acceptable for the flagship perf path,
with a portable fallback?_

Design C keeps the latency-critical warm path portable, but only Fastly currently supplies
the shared C2 cache; the other adapters degrade to their existing inline origin transform.
No self-referencing backend or browser-visible fragment surface exists.

The `esi` mode uses a hybrid implementation. A cold miss is already buffered for cache
insertion, so Fastly runs the repaired ESI parser there to validate the issue's requested
mechanism without adding a new TTFB hold. A warm hit uses Design C's exact byte split,
because parsing the full cached document would buffer the response and expose the auction
at first byte. `X-TS-Assembly: esi-parser` and `X-TS-Assembly: byte-seam` make those two
paths observable.

The original parser truncation was real: its streaming loop discarded an incomplete
element whenever that element crossed the 16 KiB read boundary. The pinned fork preserves
the incomplete buffer, and an adapter test protects a 120 KiB Next.js-style script. The
stored template remains an inert comment; executable ESI exists only in a request-private
working copy and can dispatch only TS's synthetic completed fragment.

## 7. Sequencing

1. Emit the schema-bound comment sentinel instead of an executable ESI include tag.
2. Add the seam split to the C2 hit path, returning `PublisherResponse::Stream` rather than
   `Buffered`. Strip `Content-Length`.
3. Store decoded identity; negotiate and encode the assembled response per reader.
4. **Verify first byte arrives before the auction resolves.** This is the measurement that
   distinguishes C from A, and it is the point of the whole exercise. A delayed-response
   stub origin plus a slow auction makes it observable locally.
5. Re-run every Task 6 gate against the streaming path — the C3 gate especially, because
   headers now commit before any byte and cannot be corrected afterwards.

## 8. Open risks

- **Ordering is enforced by statement order, not by types.** Three properties already
  depend on it: the C2 gate must run before the app stamps its own `private, no-store`
  (a bug already hit and fixed), the store must precede assembly, and headers must be
  final before the first byte. Streaming adds no new ordering hazard but makes the third
  unrecoverable rather than merely wrong.
- **Sentinel corruption on a hit** purges the URL variant and refetches the origin before
  headers commit. Fresh documents without an explicit body close receive a terminal seam;
  publisher-authored copies are neutralized by the HTML parser.
- **`template_cache_vary` remains spike-grade.** Operator-stated with a drift guard; the
  correct answer is a two-phase lookup.
- **Request collapsing is implemented by Fastly's pre-origin Core Cache transaction.**
  Viceroy proves reservation-before-insert and hit-after-insert; its single-threaded local
  runtime still cannot reproduce two truly concurrent cold requests.
