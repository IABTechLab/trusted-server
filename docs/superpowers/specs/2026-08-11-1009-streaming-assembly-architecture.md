# Streaming assembly: the architecture #1009 actually needs

**Date:** 2026-08-11
**Status:** Decision record. Supersedes the delivery half of the
[ESI validation spike](../plans/2026-08-10-1009-esi-validation-spike.md); the cache half
stands.
**Issue:** IABTechLab/trusted-server#1009

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

The template carries an **inert HTML comment sentinel** where the bids go, emitted by the
existing `Marker` variant:

```
<!--ts-bids-->
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

**Why a comment rather than `esi:include`.** An HTML comment is inert. If assembly ever
fails to substitute, the reader sees nothing; an unresolved `esi:include` renders as
visible text. Failure degrades to "no ads" instead of "broken page".

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

Design C makes that question unnecessary. It gets the full latency win on **all four
adapters**, with no `esi` dependency on the critical render path, no self-referencing
backend, and no second rendering architecture to maintain. The issue's own "Negative view"
lists portability, dependency risk, and operational weight as the case against ESI — C
removes all three.

**ESI is therefore sufficient but unnecessary.** For a single insertion point at a known
location, its parsing generality buys nothing a byte split does not. That is a stronger
answer than validating ESI, and it is the opposite of what the issue expected.

Keep Design B reachable behind the existing `esi` mode: #1009 asks a literal question
about ESI, the mechanism is already proven, and retaining it is cheap. Measure C.

## 7. Sequencing

1. Emit the comment sentinel instead of `esi:include`; keep `esi:include` behind the `esi`
   mode for Design B.
2. Add the seam split to the C2 hit path, returning `PublisherResponse::Stream` rather than
   `Buffered`. Strip `Content-Length`.
3. Store decoded; drop `accept_encoding` from the key.
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
- **Sentinel absence** must fail loudly. A template without the sentinel means the
  transform changed and the seam moved; silently appending bids would half-work.
- **`template_cache_vary` remains spike-grade.** Operator-stated with a drift guard; the
  correct answer is a two-phase lookup.
- **Request collapsing is untested.** Viceroy is single-threaded, so the concurrent
  cold-request case cannot be produced locally.
