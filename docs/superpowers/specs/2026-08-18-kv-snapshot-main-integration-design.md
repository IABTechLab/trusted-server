# KV Snapshot Main Integration Design

**Date:** 2026-08-18  
**Status:** Approved for implementation

## Goal

Bring PR #885 onto current `main` without regressing the true-streaming SSAT
publisher path, and verify that its request-scoped EC KV snapshot and orphaned
cookie recovery remain complete after the intervening auction, telemetry,
privacy, and adapter changes.

## Findings

PR #885 remains necessary. Current `main` still performs a synchronous
identity-graph read while decorating an SSAT auction, repeats EC graph reads in
later request phases, and does not recover a valid `ts-ec` cookie whose KV root
expired.

The branch predates true publisher streaming. Its origin-first scheduling uses
`PlatformHttpClient::send_async`, while current Fastly code rejects
`stream_response` on that path. A mechanical merge would therefore either
buffer publisher responses again or restore the KV-before-origin latency.

PR #1013 also retains the synchronous KV read before its C2 lookup. A warm ESI
template hit therefore has an additional ordering concern that cannot be fixed
inside a `main`-based PR without importing #1013. This integration will verify
the combined branches and report that concern separately.

## Design

### Preserve current `main`

Merge `origin/main` into the published feature branch. Resolve conflicts by
retaining current streaming response conversion, auction telemetry, configured
publisher-domain attribution, inline creative handling, conditional/range
stripping, cache bypass, DataDome suppression, and final response privacy.

The EC additions remain request-scoped:

1. Build or receive the active EC ID.
2. Start the publisher origin request before reading the identity graph when
   the platform can provide concurrent fan-out without sacrificing response
   streaming.
3. Load one `EcKvSnapshot` and use it to decorate the auction.
4. Dispatch the auction and await the already-running origin request.
5. Carry the updated snapshot through response finalization and post-send pull
   sync.

### Async origin response streaming

Extend the platform HTTP contract narrowly so a single pending origin request
can preserve a streamed response when it is awaited. Fastly will support this
path; adapters that cannot combine pending sends and streamed responses retain
the eager fallback.

The capability must be explicit. The publisher handler may use origin-first
scheduling only when the client advertises concurrent fan-out and pending
streamed-response support. Otherwise it loads the snapshot, dispatches the
auction, and uses the existing streamed `send` path.

Fastly's auction fan-out continues using the existing multi-request `select`
behavior. Stream-preserving pending metadata is limited to the single-request
`wait` path so it cannot become ambiguously associated after `select` reorders
pending requests.

### Recovery and privacy

Orphan recovery remains limited to a consent-granted, real-browser publisher
navigation after the origin request has successfully started. A missing row is
confirmed with a second read before rotation; failed reads do not rotate.

Generated IDs are persisted with add-only semantics before a cookie is emitted.
Consent withdrawal tombstones existing roots only, including both the incoming
cookie ID and a different active ID when applicable. Finalization updates the
request snapshot, and pull sync performs one request-wide bulk CAS from that
state.

## Error Handling

- Origin-start failure returns the existing proxy error and performs no EC KV
  preload or auction dispatch.
- KV read failure becomes a non-authoritative failed snapshot; auctions proceed
  without server-side EIDs and recovery does not rotate.
- Pending streamed-response setup failure follows the existing proxy error path.
- Platforms without combined pending/streaming support use the eager fallback.
- All failures continue using `error-stack` and existing logging conventions.

## Testing

Tests will be written before production changes and must prove:

- a streaming-capable concurrent client starts origin before the EC KV read;
- the pending origin response remains an `EdgeBody::Stream`;
- cache bypass, request rewriting, conditional/range removal, and DataDome
  behavior survive the reordered path;
- eager adapters preload before auction dispatch and still stream via `send`;
- the snapshot is reused by auction, finalize, cookie ingestion, withdrawal,
  and pull sync;
- missing, failed, tombstoned, newly created, and transiently missing snapshots
  retain the existing fail-closed behavior;
- current SSAT telemetry and publisher-domain attribution remain intact.

After focused red/green tests, run repository formatting, all adapter test
aliases, all target-matched Clippy aliases, and a temporary merge check against
`1009-esi-cacheable-root-spec`. No #1013 code will be committed to PR #885.

