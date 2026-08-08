# #1009 measurement findings

Recorded output of the checks in
[the plan](./2026-08-08-1009-measurement-and-stage-0.md). Results only — the origin
hostname is operator config and is deliberately not reproduced here.

## Step A — origin `Vary` declaration and cookie exposure

**Date:** 2026-08-08 · **Method:** direct `curl` against the publisher origin with the
configured `origin_host_header_override`, homepage path.

### Representation split

| Representation                       | `Content-Type`     | `Cache-Control` | `Set-Cookie` |
| ------------------------------------ | ------------------ | --------------- | ------------ |
| HTML navigation                      | `text/html`        | `max-age=60`    | none         |
| `RSC: 1`                             | `text/x-component` | `max-age=60`    | none         |
| `RSC: 1` + `Next-Router-Prefetch: 1` | `text/x-component` | `max-age=60`    | none         |

`Vary`, identical on every response:

```
vary: rsc, next-router-state-tree, next-router-prefetch, next-router-segment-prefetch, Accept-Encoding
```

The origin declares **every** header that distinguishes the representations, including
`next-router-segment-prefetch`, which the plan's probe list did not think to check. The
HTML/RSC split at one URL is real and correctly declared.

### Cookie personalization

Hash comparison was useless here and the plan's probe as written would have produced a
false FAIL — see the method note below. After normalizing per-request identifiers:

| Comparison                     | Differing lines |
| ------------------------------ | --------------- |
| no-cookie A vs no-cookie B     | 2               |
| no-cookie A vs **with cookie** | 2               |

Both diffs are the same single `generationTimestamp` field in the RSC payload. **The
cookie changes nothing.** Byte lengths were identical across all three responses
(1,432,944).

Cookie sent: `ts-tester=true; sessionid=abc123; ts-ec=probe`.

### Verdict: **PASS**

- `Vary` names every request header the origin varies on. ✅
- Bodies do not differ by cookie, so `Vary: Cookie` is not required. ✅
- No `Set-Cookie` on a shared-cacheable response. ✅
- Origin returns 200 without credentials, so no `Authorization` exposure at this layer.
  (#1009's basic-auth gate is on the Trusted Server side, not the origin.) ✅

**Consequence:** Stage 0 takes the simple path — the operator flag plus a config flip,
not the cache-key discriminator. No live production defect: the origin's `Vary` covers
the RSC variants that already transit the read-through cache today.

## Two findings the checks were not looking for

### 1. The origin already intends this page to be shared-cached

`cache-control: max-age=60` with a correct `Vary` and no `Set-Cookie`. The origin has
been cacheable all along; Trusted Server opted out of it. That is the spec's §4 framing
confirmed from the other side, and it strengthens the case that the bypass was
belt-and-braces rather than load-bearing.

It also bounds the win: a 60-second TTL means Stage 0 buys a cache hit only within that
window. Whether that translates into a meaningful hit rate depends on request volume per
URL, which is not measured here.

### 2. Ad-slot div IDs are randomized per request — and this interacts with Stage 0

The only per-request variance in the document is ~170 lines of ad-slot container IDs,
each a fresh 32-hex UUID:

```
ad-in_content-f75fa7fba54a4fc2a2d787f51c1837dd-in_content-0
ad-in_content-a968b27e3ee2424f8bb1c19560abf2b1-in_content-0   ← same slot, next request
```

Under the bypass, Trusted Server sees fresh IDs on every request. **Once the cache is on,
every visitor within a 60-second window receives the same IDs.**

This is very likely fine — `tsjs.adSlots` is built from configured slot definitions, not
scraped from origin markup, and injection is a prefix match on the configured `div_id`.
But it is an untested interaction between Stage 0 and the slot-matching path, and it was
not in anyone's risk list. **Verify slot matching still resolves against a cached
document before flipping the flag**, and watch TS-attributed renders across the flip
rather than only `origin_fetch_ms`.

## Method note — a defect in the plan's Step A probe

The plan's cookie check compares `shasum` of the response bodies. On this origin that
test always fails, cookie or not, because of the randomized div IDs above. Three requests
produced three different hashes with byte-identical lengths.

**Correct method:** normalize per-request identifiers before comparing, e.g.
`sed -E 's/[0-9a-f]{32}/UUID/g'`, and diff the normalized bodies rather than hashing
them. Establish the no-cookie baseline drift first, then compare the cookie arm against
that baseline — a cookie arm is only interesting if it differs by _more_ than the
baseline does. Fix the plan before anyone re-runs this.

## Residual uncertainty

#1009 states the origin varies on an experiment header as well as `rsc` and
`next-router-*`. **No experiment header appears in the origin's `Vary` list**, and the
RSC payload's `experiments` key did not differ across any of the requests made here.

Three readings, unresolved: the issue was imprecise; experiments are assigned
client-side; or they key on a cookie value this probe did not supply. The `Vary`
declaration is authoritative for cache correctness and it is thorough enough to name four
Next-specific headers, so this is unlikely to be a cache-safety gap. Worth one question
to whoever wrote that line in #1009 rather than further probing.

## Step B — consumers of TS's own response headers

Not yet run.

## Step C — hold and origin fetch timings

Not yet run.
