# #1009 ESI Merge and Hardening Design

**Date:** 2026-08-12
**Status:** Approved for implementation
**Branch:** `1009-esi-cacheable-root-spec`

## Goal

Merge current `main` into the #1009 branch and leave the opt-in
`creative_opportunities.assembly_mode = "esi"` path safe, testable, and ready for a
controlled Fastly rollout. The setting keeps its existing operator spelling, while the
render path uses an inert marker and an exact byte seam rather than the `esi` parser.

The result caches only a reader-neutral transformed template in Fastly Core Cache. The
assembled navigation response remains request-specific and must always leave Trusted Server
as `private, no-store`.

## Non-goals

- Restoring a top-level HTTP `x-cache: HIT`. Compute still executes for every assembled
  navigation; the hit is in the internal C2 template cache.
- Shipping `client_fill`. That comparison arm is incomplete and outside the requested ESI
  scope.
- Retaining a general-purpose ESI parser. One known marker does not justify a second HTML
  parser or its dependency surface.
- Changing GAM line-item, creative-selection, or win-notification behavior.

## Merge contract

`origin/main` is merged with a normal merge commit, never rebased. Conflict resolution in the
auction state must preserve both sides:

- the branch's structured bid map used by byte-seam assembly;
- main's per-auction `hb_auction_id`;
- main's delivered-winner slot set used by auction telemetry;
- APS typed-renderer metadata carried on winning bids;
- the current GPT slot-handoff and duplicate-request protections;
- the generation-zero guard that applies ESI slots and bids atomically.

Tests must cover the same non-empty winning bid through inline and ESI output after the merge.

## Shared-cache contract

### Transaction starts before origin work

The platform cache lookup returns one of three outcomes:

1. a usable template hit;
2. an insert reservation owned by this request;
3. unsupported/backend failure, which bypasses the optimization.

On Fastly, `Transaction::lookup` begins before the publisher origin request. A miss owner carries
an opaque reservation through the origin fetch and transform. It either inserts the neutral
template or explicitly cancels the obligation when the response is ineligible or processing
fails. Concurrent requests wait on the transaction and reuse the inserted object instead of each
fetching and transforming the origin.

### Key is canonical and bounded

The key hashes a length-prefixed canonical representation rather than sending raw URL/header
values to Core Cache. Inputs include:

- schema version and assembly mode;
- reader-facing scheme/host and full target URI;
- publisher origin URL and host-header override;
- a digest of all template-shaping configuration and the TSJS bundle;
- each configured `Vary` header with an explicit absent/present distinction and every raw field
  value in wire order.

Configured `Vary` names are validated as HTTP header names and deduplicated. Invalid response
`Vary` values fail closed. Per-URL purge keys use a digest too, avoiding punctuation collisions
and platform length limits.

### Origin freshness is authoritative

C2 never invents freshness. Eligibility requires a positive remaining shared lifetime derived
from the origin's cache directives. `private`, `no-store`, `no-cache`, zero freshness, malformed
directives, or already-consumed freshness all bypass storage. The stored max age is capped by the
operator's `creative_opportunities.template_cache_max_age_seconds` safety ceiling and reduced by
`Age`, apparent age from `Date`, and time spent transforming/auctioning before insertion. The
ceiling defaults to 60 seconds for rollback compatibility and is constrained to 1–86,400 seconds.

Requests carrying `Cache-Control: no-cache`, `no-store`, a positive or malformed `max-age`, or
`min-fresh`, `Pragma: no-cache`, range headers, or conditional validators bypass C2 lookup. A
browser reload's valid `max-age=0` is the deliberate exception: TS still builds a new private
response and runs a new per-reader auction, but may reuse a fresh reader-neutral C2 template. C2
does not expose object age to the core layer, so it cannot prove a positive request-side age
constraint is met. Authentication, cookie independence, and diagnostics privacy remain
request-side gates.

Fastly `Surrogate-Control` is interpreted through a deliberately narrow grammar: exactly one
positive `max-age` is required, while optional `stale-while-revalidate` and `stale-if-error`
delta-seconds are validated but never extend C2's fresh lifetime. `private`, `no-store`, and
`no-cache` refuse sharing; duplicate, missing-value, unknown, or malformed directives fail closed.
Freshness follows Fastly's documented edge precedence: `Surrogate-Control: max-age`, then
`Cache-Control: s-maxage`, `Cache-Control: max-age`, then `Expires`. Standard and surrogate
`private`, `no-store`, and `no-cache` directives remain hard refusals even when a higher-priority
field supplies positive freshness. C2 deducts `Age`/apparent age from the selected freshness and
then applies the configured ceiling. Other vendor CDN cache-policy fields remain unsupported and
disqualify the response. This covers the publisher's observed Fastly policy without pretending to
implement every CDN's precedence rules.

Request `no-cache` and `no-store` conservatively bypass both C2 lookup and insertion; the origin
response is delivered through the inline path. On adapters without a shared-template cache, or
when the cache backend fails before reservation, `esi` likewise degrades to the existing inline
path so an optimization outage does not add full-document buffering.

## Response assembly and representation

Templates are stored as identity bytes because marker validation and splitting are textual. The
origin offer remains a supported subset of the reader's `Accept-Encoding`; this is necessary so a
response-gate bypass can return the origin representation without relabeling bytes or sending a
coding the reader refused. A stored response is decoded to identity, and the final assembled
response is encoded for the current client's accepted representation:

- buffered misses assemble first and then encode;
- Fastly hits encode the prefix, request seam, and suffix through one streaming encoder;
- the template key does not vary on `Accept-Encoding`, because the transform normalizes supported
  HTTP content codings to one identity representation before storage.

This normalization assumes the origin uses `Accept-Encoding` only to select an HTTP content
coding, not to change document semantics. A publisher whose origin violates that convention must
leave ESI disabled; otherwise one normalized representation could be shared across semantically
different origin variants.

Missing or repeated markers are optimization failures, not publisher outages. An invalid hit is
purged and served from origin. Publisher-authored copies of the reserved comment are neutralized
while parsing, before TS emits its own marker. A newly transformed document without a normal
body-close seam receives a collision-resistant fallback marker at document end; if a valid seam
still cannot be established, the document is not stored rather than poisoning later hits.

## Privacy and response metadata

The decision that a response must remain private is carried to the final Fastly send. Request
filter effects and operator headers run first; then Trusted Server reapplies `private, no-store`
and removes every CDN-specific cache directive.

Cached policy metadata is strictly decoded:

- only allowlisted names are accepted;
- required fields may appear exactly once;
- repeated policy values are preserved in order;
- invalid metadata is a cache miss, never a partially reconstructed response.

Warm responses preserve repeated CSP/CSP-Report-Only and other per-document security/performance
headers such as COOP, COEP, CORP, HSTS, Origin-Agent-Cluster, reporting headers, and `Link`.
Privacy is stamped after replay so metadata cannot override it.

A nonce-bearing CSP or CSP-Report-Only response is rejected from C2. The nonce is commonly minted
per response; replaying it with a transformed shared document would create an unnecessary and
fragile coupling even if the origin accidentally left public freshness headers in place.

## Scope cleanup

The implementation removes:

- `AssemblyMode::ClientFill` and its harness arm;
- `PlatformTemplateAssembler` and the Fastly assembler registration;
- `esi_assembly.rs` and the `esi` dependency;
- the unused executable `format=fragment` response.

The public string `assembly_mode = "esi"` remains for operator continuity. Documentation describes
it as edge byte-seam assembly and makes its Fastly-only cache acceleration explicit.

## Observability and operations

Every ESI request reports a bounded `X-TS-C2-Cache` state: `hit`, `miss-stored`,
`miss-store-error`, `miss-reserved`, `bypass-request`, `bypass-response`, `unsupported`, `invalid`,
or `backend-error`. Backend read errors are not collapsed into ordinary misses, and no key or
request value is exposed.

Rollback is:

1. set `assembly_mode = "inline"`;
2. remove the new keys before rolling back to a binary whose configuration uses
   `deny_unknown_fields`;
3. purge the `ts-template` surrogate key or wait for the bounded origin-derived TTL.

The local harness works entirely from a temporary Fastly manifest, fails closed when timing data
is missing, verifies cold and warm response bodies, and executes enough of the generated seam
contract to prove populated slots and bids reach the guarded scheduler. CI runs it in ESI mode.

## Verification

Required gates after implementation:

- focused red/green tests for each defect;
- `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, `cargo test-spin`;
- all six target-matched clippy aliases;
- Fastly release WASM build;
- JS tests/build/format under Node 24.12.0;
- documentation format/build;
- cross-adapter parity suite;
- local C2 harness in `esi` and `inline` modes when Viceroy can access the local certificate
  store.
