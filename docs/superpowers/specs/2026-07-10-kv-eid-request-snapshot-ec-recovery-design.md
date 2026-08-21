# KV EID Request Snapshot and EC Recovery Design

## Objective

Remove redundant EC identity-graph reads from the publisher-navigation hot path,
overlap Fastly origin latency with KV lookup and auction dispatch, and recover
from an orphaned `ts-ec` cookie without allowing an untrusted cookie to create an
arbitrary identity-graph root.

## Confirmed Problems

An eligible Fastly publisher navigation currently performs a synchronous KV
lookup while decorating the auction before the publisher origin starts. EC
finalization can perform a second pre-send read-modify-write lookup while
ingesting `ts-eids` or `sharedId`. Post-send pull sync performs another lookup
for eligibility and then one read-modify-write lookup per successful partner.

When a request contains a valid-looking EC cookie but its KV row is missing,
generation is skipped because an EC ID is already active. Browser EID ingestion
and pull sync both reject the missing root, leaving the request in a permanent
degraded state until the cookie expires.

The cookie and live KV entry currently both have a one-year lifetime and neither
is refreshed on ordinary returning visits. The concrete correctness defect is
therefore missing-row recovery, not a general long-term mismatch between two
rolling expiration policies.

## Scope

This change covers the publisher-navigation EC path on Fastly, shared core EC
snapshot and mutation primitives, response finalization, and pull sync. It does
not change batch-sync root-creation policy, introduce sampled sliding TTL
refresh, change the EC cookie wire format, or refactor unrelated adapters.

GitHub issue #880 separately owns avoiding pull-sync reads when no partners are
enabled or when a browser-side completeness marker proves the current partner
set is complete. This design does not add that early no-partner optimization,
the completeness marker, partner-set fingerprinting, or marker validation. Its
pull-sync changes consume the single request-scoped snapshot owned by #851 and
leave a clear pre-dispatch boundary where #880 can independently skip work
without introducing another EC cache.

## Request-Scoped KV Snapshot

Introduce a snapshot type tied to one EC ID. It must distinguish:

- lookup not attempted;
- authoritative missing row;
- present row with `KvEntry` and an optional usable generation;
- lookup failure.

The failed state is a marker rather than a clone of `Report`, because the state
must travel through cloneable response extensions. The original error is logged
at the lookup boundary. Consumers degrade without retrying a failed lookup on
the hot path.

The EC ID association prevents a snapshot from being reused after orphan
recovery rotates the active ID. A present snapshot with a generation supplies
the initial CAS input to the next update. The current Fastly insert API reports
only written or precondition failed; it does not return the generation produced
by a successful write. Consequently, a successful mutation retains the updated
in-memory entry but marks its generation unavailable. The next writer can still
use that entry for read-only decisions, but must perform one refresh lookup
before another CAS. A CAS precondition failure likewise causes a fresh lookup,
merge, and bounded retry.

## Origin and Auction Scheduling

Before consuming the downstream publisher request, build a bodyless auction
request snapshot containing the original method, URI, version, and headers.
This is an in-process compatibility view, not an outbound request: providers
continue to receive the same client-facing request shape through
`AuctionContext` and must not observe the origin-rewritten URI or Host header.
The existing provider-specific outbound allowlist remains authoritative. For
example, Prebid copies only selected browser headers and applies its configured
consent-cookie forwarding policy; internal and hop-by-hop headers are not newly
forwarded by this snapshot. Consent-denied auctions do not dispatch at all.

When an EC-capable publisher request needs a snapshot and
`PlatformHttpClient::supports_concurrent_fanout()` is true:

1. Build the base auction request and client-request snapshot.
2. Rewrite and start the publisher-origin request with `send_async`.
3. Read the EC KV row once while the origin is in flight.
4. If auction-eligible, decorate and dispatch the auction using the
   client-request snapshot.
5. Await the pending origin request.

For eager implementations where `send_async` completes the upstream request
before returning, retain dispatch-before-origin ordering. Cloudflare and Spin
must not wait for the complete origin response before starting their auction.
On Fastly, real-browser document navigations with an active EC and configured
graph preload the snapshot even when auctions are disabled, no slots match, or
consent prevents auction dispatch. Their lookup still occurs after the origin
has started, so orphan detection and finalization remain available without
placing the read before origin start. Non-document publisher requests do not
preload and cannot trigger orphan rotation. Routes that do not proxy a
publisher origin leave the snapshot not-read; finalization may perform its
existing lazy lookup for EID ingestion, while orphan recovery remains restricted
to real-browser document navigations and explicit withdrawal remains
route-independent. Non-EC publisher requests retain the ordinary origin send
path.

Normal `generate_if_needed` creation seeds a generation-unavailable present
snapshot with the exact `KvEntry` successfully added to KV. It does not
pre-apply EID cookies in the generation layer. On a document navigation, the
single origin-overlapped preload refreshes that snapshot and obtains the
generation needed by finalize, so ordinary first-generation behavior joins the
same read lifecycle without an extra pre-send lookup.

Origin-start failure returns the existing proxy error without performing KV or
auction work. Auction dispatch failure remains best-effort and does not prevent
the already-started origin response from being returned. Origin failure after
auction dispatch preserves the existing abandoned-auction telemetry behavior
and emits its terminal event exactly once.

## Auction EID Resolution

Auction resolution accepts the request snapshot rather than a KV graph. A
present live entry resolves registered partner IDs. Missing, failed, not-read,
or tombstone state produces no server-side EIDs and does not fail the auction.
Client-provided EIDs continue through the existing merge and consent gate.

## Orphaned Cookie Recovery

An incoming EC ID cannot be authenticated in full. The HMAC prefix is shared by
all IDs behind the same normalized IP, while the suffix is random and unsigned.
Consequently, neither format validation nor HMAC-prefix validation authorizes
recreating the incoming key.

On an authoritative missing snapshot during consent-granted, real-browser
document-navigation EC finalization:

1. Generate a fresh EC ID using the current generation path.
2. Parse, validate, deduplicate, and apply request EID-cookie updates to the new
   in-memory `KvEntry` before persistence.
3. Atomically add that complete entry in one write.
4. Only after the add succeeds, replace the active EC ID and emit the new
   cookie.
5. Return a snapshot associated with the replacement ID and updated entry. Its
   generation is unavailable because `Add` does not return the new token.

KV lookup failure is not a miss and must not rotate the cookie. A present
tombstone must never be revived. Explicit withdrawal runs before recovery and
continues to expire the cookie and write the authoritative tombstone.

If creating the replacement row fails, retain fail-closed behavior: keep the
original active context for withdrawal bookkeeping, return a failed snapshot,
do not emit a replacement cookie, and do not create partner mappings. A random
suffix collision is handled by generating another fresh ID and retrying a small
bounded number of times; no existing key is overwritten or revived.

Batch sync and generic partner upsert APIs continue to reject missing roots.
Only the trusted browser finalization flow can initiate orphan recovery.
Subresources and non-document integration requests never rotate an orphaned
cookie.

## Withdrawal Semantics

The invariant that an unverified incoming ID cannot create a root also applies
to withdrawal tombstones. The current unconditional tombstone overwrite can
create a new key for a forged cookie and must become existing-key-only:

- an authoritative missing row is a no-op after expiring the browser cookie;
- a present row is tombstoned with its generation;
- a CAS conflict rereads and retries against the latest existing row;
- a row that disappears during retry is a no-op;
- a lookup failure expires the browser cookie but performs no KV write.

This preserves withdrawal for real entries without turning arbitrary cookie
values into stored tombstones. Tombstone writes retain their 24-hour TTL and
empty identity payload.

## Finalization Contract

EC finalization accepts the request snapshot and returns an outcome containing
the current EC context and updated snapshot. Returning-user EID ingestion uses
the carried entry and generation for its first CAS attempt. It rereads only on
CAS conflict. After a successful write, it returns the updated entry with no
usable generation because the backend does not expose the new token.

The updated in-memory entry must include partner IDs written during finalization
so post-send pull sync does not dispatch a partner that was just populated.
Cookie and cache-privacy behavior remains centralized in the existing finalize
and entry-point layers.

Mutation outcomes contain only state known to be persisted. An unchanged merge
returns the original entry and generation. A successful write returns the
persisted updated entry with no generation. A store error or exhausted CAS
retry returns a failed snapshot rather than claiming request-local updates were
stored. Pull sync does not dispatch from failed state.

## Pull Sync

Pull sync receives the finalized snapshot and uses it for eligibility without
an initial KV lookup. Missing, failed, not-read, or tombstone snapshots do not
dispatch pull sync.

Valid partner responses are collected across every HTTP concurrency batch into
one request-wide set of `PartnerIdUpdate` values and merged in one final bulk
CAS operation. Draining a network batch never writes KV. When the finalized snapshot still
has a usable generation, the uncontended case performs one write and no
additional read. When finalization already wrote the row, pull sync uses its
updated entry for eligibility, collects responses, then performs one refresh
lookup to obtain the new generation before its bulk CAS. A conflict rereads the
latest entry, rejects a tombstone, re-merges every collected update, and retries
within the existing bound. Pull sync never creates a missing root.

Rate limiting, URL allowlisting, bearer-token handling, response-size limits,
UID validation, concurrency limits, and best-effort post-send behavior remain
unchanged.

## Adapter Surface

Fastly owns the configured EC KV graph and carries the snapshot through
`EcRequestState` and `EcFinalizeState`. The graph itself is rebuilt at the entry
point as it is today; only cloneable entry data and generation travel in response
extensions.

Axum, Cloudflare, and Spin currently call the shared publisher path without an
EC KV graph. They must continue compiling and retain their existing auction and
origin behavior. Scheduling tests cover both concurrent and eager HTTP-client
implementations so future adapters cannot accidentally regress auction timing.

## Error and Privacy Invariants

- A KV error never becomes an authoritative miss.
- No unverified incoming EC ID can create a KV root.
- A cookie is emitted only after its backing row exists.
- Tombstones are never converted to live entries by enrichment.
- CAS conflicts re-merge rather than overwrite concurrent data.
- Consent-denied requests expose no EC or EID data.
- Post-send failures never change the client response.
- Logs use redacted EC identifiers through the existing `log_id` helper.

## Test Strategy

Use strict red-green-refactor cycles. Add focused unit tests for snapshot state,
snapshot-bound EID resolution, CAS reuse and conflict retry, orphan rotation,
failure versus miss, tombstone protection, and bulk pull updates. Include a
finalize-write-then-pull test proving eligibility uses the updated entry and the
pull writer refreshes the unavailable generation exactly once. Cover successful
pull responses spanning multiple HTTP concurrency batches and prove they still
produce one request-wide KV merge. Add publisher scheduling tests using
recording HTTP/KV collaborators to prove origin starts before the lookup only
for truly concurrent clients and that bidders see the original downstream
request. Cover origin-start failure with no KV/auction work, auction dispatch
failure followed by a successful origin response, and origin completion failure
with exactly one abandoned-auction terminal event. Add withdrawal tests for
present, missing, conflicting, and failed snapshot states, plus mutation-failure
tests proving unpersisted request-local IDs never appear in returned snapshots.

Run targeted core tests after each behavior change, followed by adapter suites,
formatting, and target-matched clippy according to `CLAUDE.md`.

## Success Criteria

- An eligible Fastly navigation starts the publisher origin before its EC KV
  lookup and SSP dispatch.
- Auction, finalize, and pull eligibility share one normal-path KV read. A
  subsequent pull write may require one generation-refresh read when finalize
  already wrote the row.
- Successful pull-sync responses use one bulk write rather than one RMW cycle
  per partner.
- Orphaned cookies recover through a newly generated, KV-backed EC ID.
- KV errors and tombstones remain fail-closed.
- Eager adapters do not delay auction dispatch behind origin completion.
- All target-specific tests, formatting checks, and clippy checks pass.
