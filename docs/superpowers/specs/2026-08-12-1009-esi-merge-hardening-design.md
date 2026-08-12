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
short operator safety ceiling and reduced by `Age`.

Requests carrying `Cache-Control: no-cache`/`max-age=0`, `Pragma: no-cache`, range headers, or
conditional validators bypass C2 lookup. Authentication, cookie independence, and diagnostics
privacy remain request-side gates.

`no-cache` forces a fresh origin read but may replace C2 with the newly validated response;
request `no-store` forbids both lookup and insertion. On adapters without a shared-template cache,
or when the cache backend fails before reservation, `esi` degrades to the existing inline path so
an optimization outage does not add full-document buffering.

## Response assembly and representation

Templates are stored as identity bytes because marker validation and splitting are textual. ESI
misses offer a fixed, supported compressed `Accept-Encoding` set to the origin, independent of the
reader, so every reader selects the same upstream representation before it is decoded. The final
assembled response is encoded for the current client's accepted representation:

- buffered misses assemble first and then encode;
- Fastly hits encode the prefix, request seam, and suffix through one streaming encoder;
- the template key does not vary on `Accept-Encoding`, because the stored representation is
  always identity and the upstream offer is canonical.

Missing or repeated markers are optimization failures, not publisher outages. An invalid hit is
discarded and replaced through the transactional miss path. A newly transformed document without
a normal body-close seam receives a collision-resistant fallback marker at document end; if a
valid seam still cannot be established, the document is not stored and is delivered safely rather
than turning an origin 200 into a Trusted Server 5xx.

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

A nonce-bearing CSP is not automatically rejected. If an origin explicitly marks the matching
HTML and CSP response shareable, sharing that exact header/body pair is already part of the
origin's cache contract. C2 must not extend its freshness beyond that contract.

## Scope cleanup

The implementation removes:

- `AssemblyMode::ClientFill` and its harness arm;
- `PlatformTemplateAssembler` and the Fastly assembler registration;
- `esi_assembly.rs` and the `esi` dependency;
- the unused executable `format=fragment` response.

The public string `assembly_mode = "esi"` remains for operator continuity. Documentation describes
it as edge byte-seam assembly and makes its Fastly-only cache acceleration explicit.

## Observability and operations

Every request reports a distinct C2 state: bypass, hit, cold miss/reservation, invalid entry,
backend error, store, or cancelled reservation. Backend errors are not collapsed into ordinary
misses. A response header suitable for canary inspection is added without exposing key material.

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
