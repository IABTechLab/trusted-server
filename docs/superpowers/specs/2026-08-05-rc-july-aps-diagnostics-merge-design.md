# rc/july APS Diagnostics Merge Correction

**Date:** 2026-08-05  
**Status:** Approved  
**Scope:** Correct the diagnostics seams introduced when merging
`feat/gpt-diagnostics-delivery-attribution` into `rc/july`

## Context

The diagnostics feature branch understood two render sources: inline creative
markup and PBS Cache coordinates. `rc/july` additionally supports a validated APS
renderer descriptor and resizes collapsed GAM creative shells after it posts a PUC
response. The mechanical merge preserved both implementations, but their seam has
three evidence-contract errors:

1. A valid APS renderer is classified as `unrenderable_candidate`.
2. Creative-response evidence is recorded after shell resizing, even though the
   evidence boundary is the successful `MessagePort.postMessage` call.
3. The UI and guide say "markup response" even though APS posts a renderer
   descriptor rather than markup.

The obsolete `gpt_diagnostics_bootstrap.js` file and its test remain deleted. They
have no production references and were superseded by server-recognized activation.

## Approaches Considered

### 1. Extend the shared evidence model to APS — selected

Treat a fully validated APS descriptor with a usable same-origin renderer endpoint
as a render source. Keep the existing opportunity and delivery enums, but generalize
operator wording from "markup response" to "creative response." Record success
immediately after `postMessage` returns, before any resize, trace, beacon, or logging
work.

This preserves one evidence ladder across inline, cache, and APS delivery without
exposing APS payload data.

### 2. Leave APS as unrenderable and rely on later selection evidence — rejected

This produces contradictory output: the same cycle can say that no render source
exists and that a creative response was successfully sent. It makes the direct
opportunity field unreliable for APS inventory.

### 3. Add APS-specific opportunity, response, and failure enums — rejected

The existing concepts already express the required facts. New public enum values
would expand the V1 contract without adding useful diagnostic resolution.

## Design

### Opportunity classification

`trustedServerOpportunity` continues to require a non-empty Trusted Server targeting
field and `hb_adid`. Its source precedence must match the bridge's fail-closed APS
security boundary:

- When `renderer !== undefined`, the candidate is renderable only when
  `validateApsRenderer(renderer)` succeeds and `apsRendererUrl()` is usable. An
  invalid or unusable present renderer remains `unrenderable_candidate`, even if
  inline markup or cache coordinates are also present; the bridge intentionally does
  not fall back around a rejected APS capability.
- Only when `renderer` is absent may non-empty inline `adm` or complete non-empty
  cache host/path coordinates make the candidate renderable.

Validation is safe to repeat because the APS validator caches validated object
identities.

### Creative-response boundary

For the matched direct-SSAT APS, inline, and cache branches:

1. Attempt `port.postMessage` in a narrow `try` block.
2. On a thrown post, record exactly one `response_post_failed` failure for the
   existing attempt, record no creative-response timestamp, log, and return.
3. Immediately after a successful post, record the creative response using the
   originating opaque attempt ID.
4. Perform shell resize, beacons, render tracing, and success logging afterward.

Post-send work cannot retroactively turn a successful response into a post failure.
Diagnostics writers remain no-throw and cannot suppress delivery.

### Wording

Public fields and enums stay unchanged. Type comments, overlay facts, design/guide
text, and tests use "creative response" where the statement covers all render
sources. Markup-specific explanations remain only where the code actually handles
markup.

### Testing

Tests must first fail on the merged implementation and then cover:

- valid APS renderer opportunity → `renderable_candidate`;
- invalid APS renderer opportunity → `unrenderable_candidate`;
- invalid present APS renderer plus otherwise valid inline/cache sources remains
  `unrenderable_candidate`, and the bridge posts no fallback response;
- valid APS descriptor with an unavailable renderer endpoint →
  `unrenderable_candidate`;
- valid APS request → response recorded with the same attempt after `postMessage`;
- invalid renderer → `missing_render_source` and no response;
- valid descriptor with an unavailable renderer endpoint → `missing_render_source`
  and no response;
- throwing APS `postMessage` → exactly one `response_post_failed` for the existing
  attempt and no response timestamp;
- successful post followed by resize failure → response remains recorded and no
  post-failure evidence is emitted;
- throwing diagnostics writers do not suppress an APS response;
- inline and cache response evidence is recorded before shell resizing;
- registered client-Prebid APS rendering calls none of the direct creative request,
  response, or failure writers.

The affected Vitest files, complete JS suite, lint, formatting, bundle build, docs
checks, and repository formatting gates must pass before deployment.
