# #1009 ESI parser assembly design

**Date:** 2026-08-14

**Status:** Approved implementation delta

**Base:** `1009-esi-cacheable-root-spec`

## Goal

Use the repaired `stackpop/esi` streaming parser for an authorized template-cache cold miss while
preserving the current fast, portable byte-seam stream for a warm template-cache hit.

The operator-facing mode remains:

```toml
[creative_opportunities]
assembly_mode = "esi"
```

## Approaches considered

### 1. Keep byte splitting on every path

This is the smallest and fastest implementation, and it is the current behavior. It does
not validate that the ESI implementation requested by #1009 works on real publisher HTML.

### 2. Run the ESI parser on cold and warm responses

This most literally uses ESI, but the current adapter API returns a complete `String`.
Using it on a warm hit would buffer the whole response and reintroduce the measured
auction-sized TTFB regression.

### 3. Run ESI on cold misses and byte-stream warm hits

This is the selected design. A cold miss already buffers the transformed response before
storing it, so ESI adds no new buffering boundary there. A warm hit retains the existing
head-first stream and waits for the auction only at the final seam.

## Cached representation

The template cache continues to store exactly one inert marker:

```html
<!--ts-ad-seam-->
```

The template schema stays at version 4 because the cached bytes do not change. The stored
object never contains executable ESI markup, per-reader slots, bids, cookies, or other
reader state.

Keeping the inert marker preserves the current collision normalization and makes an
unassembled cached object harmless. It also avoids making an obsolete HTTP fragment route
part of the cache schema.

## Cold-miss data flow

1. Fetch and transform the origin response.
2. Normalize and validate the single inert marker.
3. Refuse template-cache storage and platform parsing if publisher bytes contain any `<esi:`
   directive; serve that response with the safe byte seam instead.
4. Store the reader-neutral template in the shared template cache.
5. Build the current reader's combined slots-and-bids seam script.
6. Ask the platform template assembler to assemble the stored template.
7. The Fastly assembler replaces the one inert marker in its private working copy with a
   synthetic `<esi:include>`.
8. `stackpop/esi` parses the complete publisher document. Its dispatcher returns the
   already-built seam as `PendingFragmentContent::CompletedRequest`; no HTTP request or
   nested executor is involved.
9. Compress and serve the assembled response as `private, no-store`.

The template is stored before assembly. Reversing those operations would cache one
reader's ad state and is forbidden.

## Warm-hit data flow

A warm hit does not invoke the ESI parser. It keeps the existing streaming finalizer:

1. Validate the cached marker before committing headers.
2. Stream the template prefix immediately.
3. Collect the already-dispatched auction at the seam.
4. Stream the reader's slots-and-bids script.
5. Stream the template suffix and finish the encoder.

This retains the observed millisecond TTFB while the response may continue downloading
until the auction resolves.

## Platform boundary

Core regains a small byte-oriented `PlatformTemplateAssembler` trait. Fastly supplies the
ESI-backed implementation. Other adapters receive an unavailable implementation by
default. Core retains the validated original bytes until assembly succeeds, so fallback
never depends on recovering parser output.

An unavailable or failed platform assembler falls back to the already-validated byte
split for that cold response. This preserves availability and adapter portability without
silently changing the cached object.

## ESI safety contract

The Fastly adapter:

- pins `stackpop/esi` to verified commit
  `4c53feab4d22ad9a84641b4c46f3f63bc6d197e2`;
- disables include caching, rendered caching, `edge_control`, DCA inheritance, and nested
  includes;
- rejects publisher-authored ESI directives before inserting its synthetic include;
- validates that the dispatcher receives only the synthetic internal include;
- treats fragment bytes as data and never reparses them as ESI;
- requires the inert seam marker exactly once, even though core already validates it;
- verifies parser output is byte-for-byte the original template with only that seam
  replaced; any truncation or unrelated parser mutation triggers fallback.

Any violation returns an assembler error and core uses the safe byte-seam fallback.
Core performs the publisher-ESI rejection before template-cache storage as the primary guard; the
adapter repeats it as defense in depth.

## Request-visible diagnostics

Assembled template-cache responses carry an `x-ts-assembly` header:

| Path                                                                                    | Value                |
| --------------------------------------------------------------------------------------- | -------------------- |
| Authorized cold miss using the repaired parser                                          | `esi-parser`         |
| Cold response whose platform parser is unavailable, disallowed, or rejects the document | `byte-seam-fallback` |
| Warm cache hit                                                                          | `byte-seam`          |

Together with `x-ts-template-cache`, this lets an operator prove the internal path with two
ordinary requests instead of inferring it from timing.

## Testing

Tests must prove:

- a cold miss invokes the platform assembler once;
- a warm hit does not invoke it again;
- platform failure produces a complete byte-seam response;
- the real ESI adapter resolves the synthetic include after scripts larger than 16 KiB;
- publisher ESI directives are rejected rather than executed;
- nested ESI in the fragment is emitted verbatim;
- the stored template remains reader-neutral and contains the inert schema-v4 marker;
- cold and warm responses both contain populated slots and the same bid state;
- request-visible assembly headers identify each path;
- all existing cache, privacy, policy-header, TTL, compression, and SPA-race tests remain
  green.

## Non-goals

- No self-referencing Fastly backend.
- No browser-visible fragment request.
- No restoration of `format=fragment` on `/_ts/page-bids`.
- No ESI parsing on warm hits.
- No change to cache TTL policy or the operator configuration shape.
