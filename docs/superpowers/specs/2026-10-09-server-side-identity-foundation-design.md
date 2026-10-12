# Server-side identity foundation

Status: Architecture implemented in the draft change, awaiting code review.
See the [implementation plan and verification](../plans/2026-10-09-server-side-identity-foundation-and-lockr.md).
Provider activation and metadata-compatible deployment remain unverified.
The default-off `ts-eids-s` capture handoff in section 8.2 is implemented in the
draft change. Its local checks are recorded separately in the implementation
plan; deployed acceptance remains gated.

First consumer: [Lockr proxy-side identity capture](2026-10-08-lockr-identity-capture-design.md).

Related: [KV EID request snapshot and EC recovery](2026-07-10-kv-eid-request-snapshot-ec-recovery-design.md)
defines the snapshot, root-creation and withdrawal rules retained here.
[The existing LiveRamp design](2026-08-21-liveramp-integration-design.md)
describes the browser path. A separate future spec must approve any native
resolution protocol and identifier intake.

## 1. Purpose

Give integrations a standard way to contribute server-side identities without
making each integration implement storage, consent handling and auction wiring.

The ownership rule is:

> Integrations acquire and interpret identities. A shared identity service
> coordinates lifecycle work. EC owns persistence, consent and auction delivery.

Capture and active resolution are different capabilities. They share identity
outcomes and mutation policy, not a single provider HTTP protocol.

```mermaid
flowchart TD
    A[Browser IDs through auction body] --> D[Shared identity service]
    B[Integration captures IDs from proxied traffic] --> D
    C[Future integration resolves or refreshes IDs] --> D
    D --> E[EC identity graph in KV]
    E --> F[Server-side auctions read usable IDs]
```

This spec owns the shared contracts. The Lockr spec owns the first provider
implementation. A later LiveRamp spec can use the resolution contract without
changing the Lockr protocol.

## 2. Starting system and gaps (pre-implementation baseline)

- `integrations/registry.rs` registers optional proxies, request filters,
  rewriters and head injectors, but no identity capabilities.
- `ec/registry.rs` builds the source policy and partner API registry from
  `[[ec.partners]]`. An EID source is the key into the identity graph.
- `/auction` uses body EIDs for the current auction. Its EC finalization
  currently persists request cookies, not those body EIDs.
- `ec/pull_sync.rs` implements a specific bearer-authenticated protocol:
  send `ec_id`, receive `{"uid":"..."}`, and fill missing sources only.
  It does not refresh existing UIDs; its refresh interval is currently unused.
- `KvPartnerId` stores only `uid`. It cannot express provider expiry, the
  authoritative writer, or the version a refresh result was based on.

Reuse the existing snapshot, conditional-write and post-send mechanisms.
Do not mistake the legacy pull-sync wire protocol for a universal resolver.

The shared identity service belongs in core EC code and uses `KvIdentityGraph`;
it is not another store or vendor registry. The integration registry exposes
capabilities to that service. Platform adapters only supply execution timing
and services, including the point at which a response has been sent.

## 3. Scope and invariants

The first delivery covers integration capability registration, common outcomes,
source ownership, expiry-aware records, browser-body ingestion, bounded capture
and the Fastly post-send capture path. Resolution and refresh contracts are
specified now; a production active resolver is implemented only with an
approved provider spec.

Invariants:

1. `ts-ec` remains the pointer to an existing EC row. An optional, short-lived
   `ts-eids-s` cookie can carry authenticated capture observations across
   requests; it is not a replacement identity root or a long-lived EID store.
2. Integration capture and `/auction` enrichment never create or recover a
   root. Existing eligible publisher-navigation creation and recovery remain.
3. Missing and unreadable rows are distinct. Neither authorizes enrichment.
4. A tombstone is never revived by identity acquisition.
5. Browser input is not an authoritative partner update.
6. A provider timeout or configuration failure does not revoke consent or
   erase a still-valid ID.
7. Known-expired or invalidated records are not exposed to bidders.
8. Stored state and response extensions identify the full EC ID, not just its
   IP-derived hash prefix. A rotated identity cannot reuse the old snapshot.
9. Logs and telemetry never contain UIDs, envelopes, hashes submitted for
   resolution, consent strings, or query URLs carrying those values.
10. Fastly capture persistence and future active resolution run after send.
    Browser-body persistence keeps existing EC finalization timing.

Non-goals: LiveRamp API implementation, hashed-identifier collection or storage,
cross-browser linking, provider-specific authentication, audience segments,
multiple stored UIDs per source, arbitrary provider metadata in KV, durable
background queues, and guaranteed inclusion in the first auction.

## 4. Registration and configuration

### 4.1 Optional integration capabilities

Extend the existing integration registration, not a separately maintained list
of identity vendors:

```rust
IntegrationRegistration::builder("example_identity")
    .with_identity_capture(capture)
    .with_identity_resolver(resolver)
    .build()
```

The sketch shows optional capabilities, not a requirement to implement both.
A capture-only module needs no resolver or dummy HTTP endpoint. A resolver-only
module needs no browser SDK or proxy route. Each capability declares the finite
set of source namespaces it can produce and its consent requirements.

The initial implementation registers capture. Reserve the resolver contract
without enabling an unused production fetch loop.

### 4.2 Integration IDs and source namespaces

An integration ID identifies the acquisition module. An EID `source` identifies
the partner namespace stored in KV. They are not interchangeable: one broker
can issue several sources, and multiple modules can issue the same source.

Keep provider protocol settings under `[integrations.<module>]`. Keep generic
source policy, bidstream permission and existing partner API authentication
under `[[ec.partners]]`.

Add an optional, vendor-neutral source policy field:

```toml
[[ec.partners]]
name = "Example identity source"
source_domain = "ids.example.com"
openrtb_atype = 3
bidstream_enabled = true
identity_owner = "example_identity"
```

`identity_owner` selects the one integration allowed to replace or invalidate
that source through managed acquisition. Validate that the configured module
has an identity descriptor claiming the source. Reject ambiguous ownership;
do not resolve it by registration order. A configured module can disable its
acquisition capability without losing its ownership descriptor or invalidating
the kill switch configuration.

Without an owner, browser and legacy partner paths retain their existing source
policy. Managed integration updates require an explicit owner. Browser input
can still fill an absent source conservatively, but cannot replace a stored
record or confer provider authority.

For an owned source, legacy push and pull paths must not bypass owner checks.
Disable conflicting authoritative paths or route an explicitly authorized
writer through the same policy. Do not silently allow a generic pull to replace
an integration-owned envelope. Changing the owner is an operator migration,
not an automatic fallback when a module fails.

## 5. Acquisition contracts and common outcomes

### 5.1 Capture

A capture capability declares exact proxy response paths and interprets a
bounded decoded body. The framework supplies request-local EC and consent
context, method, status and response metadata. The module knows its provider
schema; the framework owns buffering, bounds and response preservation.

If the provider needs consent values from a POST body, the integration extracts
them from its existing forwarding buffer and supplies normalized signals.
It must not consume the body twice or let a second parser modify forwarding.

Only framework-associated upstream responses can originate authoritative capture
updates. The shared service can seal a validated observation into the capture
handoff defined in section 8.2. Later requests must authenticate and revalidate
that handoff before staging its restricted retry. An arbitrary browser body,
unsigned cookie or inbound diagnostic header cannot select this trust level or
impersonate a module.

### 5.2 Resolution and refresh

The future resolver contract separates planning, HTTP execution and parsing:

1. Given permitted inputs, current record and consent, the module plans no
   work, reports missing inputs, or builds a resolution/refresh request.
2. The shared coordinator enforces eligibility, due time, consent, allowlists,
   rate limits, timeout and response limits using platform HTTP services.
3. The module interprets the provider response into common identity outcomes.
4. EC applies those outcomes against the record version used to plan the work.

Inputs must be approved by the provider spec and represented as named types.
No generic request header becomes a trusted hashed identifier by convention.
The framework does not grant modules access to all publisher cookies or
credentials; disclosure is limited to declared, permitted inputs.

### 5.3 Outcomes

| Outcome                | Meaning and shared action                                                                                                                   |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `Issued`               | Validated source and UID, optional provider expiry; apply the appropriate update policy. A refresh may keep the UID while extending expiry. |
| `NoChange`             | Nothing new was observed; do not manufacture a write.                                                                                       |
| `NoMatch`              | Resolution produced no ID; it does not invalidate an existing ID or withdraw consent.                                                       |
| `Invalidated`          | The provider rejected a particular existing record; make that record unusable only if its expected revision still matches.                  |
| `ConsentWithdrawn`     | Explicit withdrawal with a provider-spec-defined scope; never inferred from a generic empty response.                                       |
| `RetryableFailure`     | Temporary network/provider failure; preserve valid state and apply bounded retry policy.                                                    |
| `ConfigurationFailure` | Approval, credentials or request configuration is wrong; preserve valid state and report a redacted operational error.                      |

Capture decode/schema errors skip capture without changing the proxy response.
Module output is validated again at the shared trust boundary against source
claims, configured ownership, UID caps and allowed metadata. The service assigns
the writer identity and update class; neither comes from browser JSON.

Per-source invalidation is not a global EC withdrawal. A provider spec must
explicitly justify a global withdrawal outcome. The shared finalizer gives
withdrawal precedence over enrichment and suppresses duplicate tombstones when
normal request consent finalization has already handled it.

## 6. Stored records and freshness

### 6.1 Proposed record extension

Keep one UID per source, and add optional lifecycle fields to `KvPartnerId`:

| Field            | Purpose                                                                           |
| ---------------- | --------------------------------------------------------------------------------- |
| `uid`            | Existing opaque identifier.                                                       |
| `expires_at`     | Provider expiry in Unix seconds, when known.                                      |
| `writer`         | Framework-assigned integration, browser or legacy writer identity.                |
| `revision`       | Opaque version changed on every material record mutation.                         |
| `updated_at`     | Server time of a material persisted change, not proof of provider issuance order. |
| `invalidated_at` | Marks a rejected record unusable while retaining its revision.                    |

The first delivery adds expiry, writer and revision support. The table also
reserves the semantics of update timestamps and per-source invalidation; do not
add unused runtime state or scheduling merely to fill out the future contract.
Those fields, refresh timing and attempt/backoff state are implemented when a
consumer needs them. Acquisition timestamps are not a substitute for
provider-owned ordering.

Known provider expiry is preserved through KV and auction resolution. Reject an
already-expired issued token. Do not clear a known expiry merely because a
same-UID observation omitted it. A same-UID authoritative observation with a
changed, supported expiry is a material update; unchanged UID and lifecycle
metadata require no write. Do not write merely to update an observation clock.

Legacy UID-only records and browser inputs have unknown expiry and retain their
existing row-lifetime behavior. Do not invent a historical expiry or falsely
label them as provider-resolved. This compatibility choice does not guarantee
that legacy IDs are still valid; tightening unknown-expiry policy is a separate
operator decision.

The record extension does not store OpenRTB provenance, UID `ext`, or extra UIDs.
Writer identity identifies the acquisition path. An integration can use that
persisted writer and source to reconstruct fixed provenance defined by its spec;
do not infer provenance from the current configured owner or source alone.
The first mapping is Lockr ID5, with `inserter = "Lockr-For-Publishers"`,
`matcher = ""` and `mm = 2`. Other sources and browser, legacy or unknown writers
remain without reconstructed provenance. No additional storage fields are needed.

### 6.2 Mutation classes

Browser enrichment keeps the current conservative policy: fill absent sources,
skip unchanged values, preserve different values, and perform at most one CAS
write followed by at most one conflict read with no retry write. Browser input
cannot set expiry, writer, revision or invalidation metadata. Expired and
invalidated records count as existing records, not empty slots that the browser
can revive.

Authoritative acquisition can replace an owned source and update supported
lifecycle metadata. It retains bounded CAS refresh and re-merge, preserving
unrelated sources and rejecting a missing or withdrawn root.

For future resolver updates and invalidation, carry the revision used to plan
the call. If another writer changed that record, discard the stale result;
do not delete or overwrite the replacement. Comparing UID alone is insufficient
because a same-UID refresh can change expiry. Source invalidation retains a
non-deliverable record marker so a stale browser submission cannot refill it.
DSR deletion and global withdrawal still remove identity payload through their
existing authoritative paths.

Capture-only updates without a provider sequence retain observation/CAS ordering.
They do not promise chronological provider freshness across overlapping SDK
requests. A provider requiring stronger ordering must supply an ordering
contract in its integration spec, not rely on edge wall clocks.

Authenticated handoff replay is a separate, fill-absent-only mutation class.
It retains the capture writer and known expiry but cannot replace an existing
source or alter its lifecycle metadata. Recheck source absence, root consent,
provider expiry and the original handoff deadline at mutation time; a conflict
that introduces the source makes the replay a no-op.
Do not send replay through browser enrichment, which discards lifecycle metadata,
or unrestricted authoritative replacement, which could replay an older token.

### 6.3 Auction reads

Resolve only live, consented, bidstream-enabled records whose known expiry has
not passed and which are not invalidated. Retain expired records as lifecycle
state; auction omission does not require a hot-path cleanup write.

When browser EIDs are merged with KV, a browser UID matching a known-expired or
invalidated record must also be omitted for that source. Otherwise request-body
precedence would bypass the usability rule. A different valid browser UID can
still be used in its current auction under existing consent rules without
becoming an authoritative replacement in KV.

Body-supplied OpenRTB provenance remains current-request metadata. Add optional
`inserter`, `matcher` and `mm` to `Eid` parsing and merging; an empty `matcher`
is a supplied value. Later KV-derived EIDs use source registry `atype` and the
persisted acquisition writer. For usable Lockr-written `id5-sync.com` records,
reconstruct the fixed provenance in section 6.1 through Lockr's mapping, even
when new capture is disabled. Do not label all records for an owned source.

Explicit body fields take precedence over reconstructed fields, including empty
`matcher` and zero-valued `mm`. If browser input adds a different UID for the
same source, suppress reconstructed provenance for the resulting mixed-origin
EID because its top-level fields apply to every UID. Preserve explicit body
provenance and the existing UID merge rules. The first delivery preserves
existing caps and consent gates across all auction paths.

When the optional handoff is enabled, resolve eligible captured IDs under
section 8.2 before the existing browser merge. Handoff entries require a live,
consenting root and an absent stored source. Stored records retain precedence,
including expired or invalidated records that block replay. Handoff delivery
does not relax current-request browser consent gates.

## 7. Consent and execution timing

The shared service evaluates global EC consent and each module's additional
requirements. A global `consent.ok` flag alone does not prove vendor or purpose
permission. Future outbound disclosure rechecks the live consenting row just
before dispatch and respects current request privacy signals. Mutation rechecks
root consent through conditional-write semantics.

```mermaid
flowchart TD
    A[Request has existing EC and consent context] --> B[Integration captures identity outcomes]
    B --> C[Shared service stages permitted effects]
    C --> D[Finalization handles withdrawal and optional capture handoff cookie]
    D --> E[Response sent to browser]
    E --> F[Shared service applies captured updates]
    F --> G[Persisted snapshot passed to later identity work]
    G --> H[Future due resolver work or existing eligible pull sync]
```

For browser-body ingestion, stage updates before auction dispatch and carry them
through EC finalization, including handled provider-error responses. Preserve
current auction behavior when persistence fails. Denied identity use stages no
updates. Browser persistence does not require a successful bid or a winning bid.

Fastly removes cloneable identity effects before response conversion drops
extensions. Shared effects carry the full EC ID, trusted acquisition origin and
snapshot; the adapter does not parse provider payloads. Run captured mutations
after send and before legacy pull sync. Pass only persisted outcome snapshots
forward, including changed generations and failed states.

Capture routes can have `NotRead` snapshots. Their first required lookup occurs
after send. A reusable snapshot avoids that lookup, but unusable generations,
stale misses and CAS conflicts can require refresh reads. No zero-read claim is
made. No-token observations queue no new direct capture mutation. A valid
incoming handoff can independently stage restricted replay under section 8.2.

Best-effort after-send work adds execution time, not Fastly response latency.
There is no durable queue, guaranteed retry or first-auction availability.
The optional handoff allows another qualifying request to attempt persistence
within a fixed deadline, without holding the capture response for a KV write.
Replayed capture persistence also runs after send on Fastly, before legacy pull.
Other adapters use the shared service only where EC KV support exists. Document
before-send behavior where no post-send execution hook exists; do not claim
persistence on unsupported adapters.

## 8. Browser-body migration and capture handoff

### 8.1 Retire the redundant browser EID cookie

Use validated `/auction` body EIDs both for current-request forwarding and for
registered-source browser enrichment. Expose the existing EID-to-update helper
without round-tripping through a fake cookie. Reuse the auction snapshot in
finalization instead of performing a second unconditional lookup.

Remove the `ts-eids` writer and server readers only after body ingestion is
verified. Final behavior:

- `/auction` gets current-request client EIDs from the body, with no `ts-eids`
  fallback. Unknown browser sources remain usable only in that current auction.
- Publisher and SPA page-bid auctions read registered IDs from KV. All auction
  paths may also use eligible `ts-eids-s` observations under section 8.2.
- The separate `sharedId` cookie retains its existing ingestion behavior;
  combine browser inputs into one finalization mutation attempt.
- Orphan recovery no longer seeds IDs from a `ts-eids` cookie.
- Late browser IDs need another auction submission for persistence. No new
  page-exit beacon or browser EID cookie fallback is introduced. `ts-eids-s`
  contains validated server capture only; it does not collect late browser IDs.

Normal `/auction` callers already submit available browser EIDs in the body.
The old `ts-eids` reader used body-or-cookie fallback, not missing-source merge.
Its current-auction role was therefore largely redundant. It could also ingest
IDs discovered after request construction on a later navigation. Restoring that
behavior remains a separate benefit decision; this handoff does not restore it.

Retire `x-ts-eids` and `x-ts-eids-truncated` emission, checking external diagnostic
tooling first. Keep both names on the inbound internal-header strip list.
Do not remove EC or consent headers as part of this change.

A transition JavaScript release expires the old host-only `Path=/` cookie;
its existing max-age is one day. New servers ignore it even if cached older
bundles still write it. Remove the cleanup shim once the transition artifacts
have aged out. Do not add unconditional server cookie cleanup that changes
cache privacy on every response.

### 8.2 Short-lived authenticated capture handoff

Reintroduce `ts-eids-s` as an optional transient handoff, not the original
unsigned source/UID array. It has two material benefits for a newly captured
source: auction delivery before capture persistence becomes visible, and a
later persistence attempt after a failed post-send write. It does not duplicate
browser-body intake or replace KV as the identity graph.

The shared identity service owns encoding, validation, auction resolution,
retry staging and cleanup. Integrations provide observations and approved
source/writer mappings, not private cookie readers or writers.

#### Envelope and lifetime

- Use a versioned authenticated envelope with a key identifier, publisher
  binding, full EC ID, absolute handoff deadline and a bounded source-keyed
  list. Each entry carries the acquisition module, UID, its original absolute
  deadline and optional provider expiry in Unix seconds. Do not include raw
  consent strings or credentials.
- Protect the complete envelope with a publisher-scoped signing key and a
  reviewed encoding and verification scheme. Base64 is encoding, not integrity
  protection; `HttpOnly` does not authenticate an incoming cookie. Never fall
  back to unsigned acceptance when a key or signature is unavailable.
- Use `Path=/; Secure; HttpOnly; SameSite=Lax` and the configured EC cookie
  domain. Default entry lifetime is five minutes, never longer than its known
  provider expiry. Set the envelope deadline to the latest retained entry
  deadline and browser `Max-Age` to the remaining envelope lifetime. Filter each
  entry by its own authenticated deadline; `Max-Age` alone cannot establish expiry.
- Cap the complete encoded value, including authentication overhead, at 2,048
  bytes. Retain the existing UID and source caps. Reject an entry that cannot
  fit; never emit a partial envelope. Any multi-source truncation is
  deterministic and removes the earliest-expiring entries first.
- Each entry must retain its original absolute deadline when cookies merge or
  entries are removed. Reading, retrying, clearing another entry or receiving
  `NoChange` must not extend that deadline. An actually new validated capture
  can get a new deadline. A matching UID and effective expiry retain the pending
  deadline; omission of expiry retains the same UID's known pending expiry.
  That pending expiry may only fill an absent source, never alter a stored record.
  Enforce both entry and envelope deadlines.

The shared finalizer can set the cookie from already validated capture without
adding a pre-send KV lookup or write. A `NotRead` snapshot remains permitted at
issuance; the cookie itself does not prove that a live root exists. Handoff
creation failure leaves the original response and normal deferred capture
persistence intact. Responses carrying the cookie remain private and uncacheable.
Upstream body bytes and encoding/length headers remain unchanged, but the response
is no longer header-identical because it adds `Set-Cookie` and cache privacy.

#### Eligibility, precedence and replay

On a later request, authenticate within the encoded size cap and revalidate the
full EC ID, publisher, clock, entry expiry, current acquisition consent, module
claims, enabled capture and configured ownership. Use the normal request-local
KV snapshot. Missing, unreadable or withdrawn roots authorize neither handoff
auction delivery nor handoff persistence. No cookie creates or recovers a root.
An unusable clock cannot authorize a time-bounded handoff.

Eligibility reflects the row observed in this request, not a strongly consistent
or globally current read. A stale live snapshot that still lacks the source can
deliver a handoff while another POP has newer stored state or a withdrawal.
This retains the foundation's existing KV read-consistency limit. Current request
consent still gates delivery; post-send replay checks the authoritative generation
and cannot overwrite the newer record or revive its tombstone. No cross-POP
instant-withdrawal guarantee is made.

For each registered source:

| Stored source state                                          | Handoff action                                                                                                                  |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| Absent on a live, consenting root                            | Allow the unexpired captured UID for a bidstream-enabled source and stage a fill-absent-only retry.                             |
| Expired or invalidated record                                | Retire the handoff without delivery or retry for that source; the retained record is not an empty slot.                         |
| Usable record with the same UID, writer and effective expiry | Acknowledge the handoff and remove it; use the stored record.                                                                   |
| Any other existing record                                    | Use the stored record if usable and retire the conflicting handoff; do not replace the UID, extend expiry or change its writer. |

Source/UID equality alone cannot acknowledge a handoff with different expiry or
writer metadata. When capture omitted expiry, a stored same-UID known expiry is
retained rather than erased. A matching browser UID does not turn handoff
metadata into browser authority. Provenance reconstruction uses only the
validated acquisition module and its approved source mapping; mixed-origin
browser merges follow section 6.3.

This conservative first version accelerates absent-source capture only. A refresh
of an existing source still depends on the direct authoritative capture write.
It does not make an older cookie beat a newer record observed in the snapshot. Supporting immediate
replacement or expiry extension from replay requires a separate revision-bound
contract; edge timestamps and signatures do not prove provider ordering.

The shared service owns retry. A new direct capture observation supersedes any
incoming handoff for the same source; do not replay the older entry alongside it.
Combine remaining eligible handoff entries into at most one replay batch per
qualifying request, using bounded CAS and preserving unrelated sources. On
conflict, refresh and recheck source absence; do not blindly retry replacement. Fastly applies the replay after send, in the shared capture executor
before pull sync. No immediate recursive retry, private integration dispatcher,
provider refetch or durable queue is introduced.

Remove acknowledged, conflicting, expired or no-longer-authorized entries on
responses that already have a validated handoff to reconcile. Clear the cookie
when none remain. Withdrawal, EC rotation or publisher/owner mismatch invalidate
the affected handoff. Do not emit unconditional cleanup cookies on unrelated
responses. A post-send success cannot clear the cookie in an already-sent response;
cleanup waits for a later request that observes the persisted result. A failed
write can retain the handoff only until its original deadline.

A later SDK response with no new tokens may therefore repair a previously missed
capture by carrying an existing valid handoff. No new token means no new direct
capture effect; it does not mean no independently validated replay. Retry remains
best-effort and requires another qualifying request before expiry.

The capture response must install the cookie before a later request can carry it.
An auction starting earlier can still miss the ID. The handoff does not guarantee
the first auction, immediate refresh of an existing source, delivery during a
root-read failure, or repair after its deadline.

Shared handoff issuance, delivery and retries default off behind explicit
publisher opt-in. The integration capture kill switch also stops handoff delivery
and retry for that module, without making stored KV records unusable. Confirm the
publisher key provisioning and rotation before activation. The implemented
interface is `[ec.identity_handoff]` with `enabled`, `active_key_id` and at most
four `keys`. Each key has a safe identifier and a dedicated app-config secret-store
reference; runtime material must be URL-safe unpadded base64 of 32 random bytes.
Issuance uses the active key and verification accepts retained rotation keys.
The versioned envelope uses domain-separated HMAC-SHA256; the EC passphrase
cannot be reused. Only Fastly requests admitted to its EC/KV finalization path
enable handoff work. Unsupported adapters neither issue nor replay it.

## 9. Bounded transport and failure policy

Capture response inspection has independent 64 KiB limits for original body
bytes and decoded JSON bytes. Decode incrementally into bounded buffers; do not
allocate the full decompressed body before checking its size. Exceeding either
limit, unsupported encoding or decode failure skips capture and preserves the
original body and encoding/length headers. Oversized streaming bodies must
replay the collected original prefix and remaining stream, not fail after a
destructive collector consumes the prefix.

Memory accounting includes both buffers and bounded stream/decoder overhead;
the total budget is not 64 KiB. Future resolver HTTP uses equivalent independent
wire and decoded limits, though it has no browser response to replay.

Retain UID length and source-count caps. Provider request building must use
HTTPS, an explicit destination allowlist and narrowly allowed headers. Do not
forward publisher credentials by default. Disable or revalidate redirects
before any request carrying identity input leaves the approved destination.

Failures are best-effort and redacted. No-match and failed attempts need bounded
negative-cache/backoff policy when an active resolver is introduced; define its
state lifetime and key in that provider delivery. Do not introduce nested,
unbounded retries in modules and the coordinator.

## 10. Resolution lifecycle reserved for later delivery

A future resolver distinguishes missing, usable, refresh-due, expired and
invalidated state. Missing approved inputs means no call. Expired or invalidated
values are not auction-usable, even if refresh or re-resolution cannot run.

Refresh is initially activity-driven: a qualifying request can trigger due work
after send. There is no promise that a task runs while a visitor is inactive.
A provider spec defines resolution inputs, refresh inputs, interval, failure
backoff and whether a particular response invalidates the old record.

Reuse post-send orchestration without changing the legacy protocol's
fill-missing-only behavior for unowned sources. Skip integration-owned sources
in legacy completeness and dispatch decisions. Rate-limit keys must define
whether their identity scope is the full EC ID or the IP-derived EC hash;
the existing generic pull limiter uses the hash and is not a per-cookie limit.

No module can create cross-browser linkage by storing a UID in KV. Chrome and
Safari ordinarily have different full EC IDs. A later linking design must
establish an approved association; a common IP prefix is not proof of a person.

## 11. Compatibility and delivery sequence

The proposed record fields are additive and optional within schema version 1.
Legacy records decode as before. Unsupported schema versions still fail closed.
Do not bump the schema merely to make old records unreadable.

However, older writers would deserialize and reserialize away unknown metadata,
and older readers would ignore expiry. Deploy metadata-preserving readers and
all mutation paths before activating managed writes. This includes browser,
push, pull, withdrawal, recovery and admin-related mutations. Test their
round-trips and equality rules; a UID-only overwrite must not strip expiry.

Activation requires deployment propagation and old invocation drain. Rolling
back to a UID-only binary after activation is unsafe unless managed writes are
stopped and record compatibility is explicitly handled. Disabling acquisition
is safe but does not remove expiry-aware reader requirements.

Delivery stages:

1. Add defaulted record fields, usability checks and metadata-preserving
   mutation plumbing with managed acquisition inactive.
2. Add common outcomes, source ownership and capture registration. Keep
   authoritative and browser mutation classes distinct.
3. Add browser-body ingestion, verify it, then retire `ts-eids` cookie/header paths.
4. Add bounded capture handling and shared Fastly effect execution, using
   synthetic integration fixtures before enabling Lockr.
5. Implement the separate Lockr consumer, then the optional authenticated
   handoff through the shared service. Enable each only after its ownership,
   provider, key-management and safety gates pass.
6. Implement active resolution and refresh only with its later provider spec.

## 12. Verification and open decisions

Foundation tests must cover:

- Optional capability registration and source-ownership validation, including
  a capture kill switch without invalidating the configured owner.
- Legacy record decoding and every writer preserving lifecycle metadata.
- Browser fill-missing and conflict bounds; rejection of forged authority and
  metadata; no browser resurrection of expired/invalidated state.
- Authoritative replacement and same-UID expiry changes; unchanged metadata
  skips writes; unrelated sources survive CAS conflicts.
- Known expiry boundaries across publisher, `/auction` and SPA auction paths,
  including an expired UID resubmitted in the auction body.
- Writer-aware Lockr ID5 provenance through KV reads and bidder-facing auctions,
  including browser/legacy/unknown writers, configured-owner-only records,
  unrelated sources, matching versus different body UIDs and explicit body
  field precedence. Reconstruction must not write KV or bypass usability gates.
- Revision-conditional invalidation and stale resolver-result rejection,
  with same-UID/new-expiry and concurrent-withdrawal cases.
- Missing versus failed roots, no-create enrichment, snapshot identity
  association, withdrawal precedence and duplicate-tombstone suppression.
- Independent original/decoded size limits, compressed expansion, unsupported
  encodings, decode failure and byte-identical capture pass-through.
- Browser updates surviving handled auction-provider failures; one combined
  browser finalization mutation with `sharedId`; body-only migration behavior.
- Fastly send-before-capture-write ordering and resulting snapshot handoff to
  pull sync; no-token and failed-update paths; supported adapter behavior.
- Redacted errors and no private data in logs, including provider query URLs.
- Handoff authentication, publisher/full-ID binding, key rotation, malformed and
  oversized values, independent entry deadlines and no deadline extension on
  replay, merge, cleanup or `NoChange`.
- Handoff auction eligibility across publisher, `/auction` and SPA paths before
  capture KV visibility; no delivery for missing/failed/withdrawn roots, denied
  consent, expired entries, disabled capture or changed ownership.
- Absent-source-only replay, exact acknowledgement, different-UID and same-UID
  changed-expiry conflicts, retained expired records, duplicate requests and CAS
  races with acquisition or withdrawal. No stale replacement or metadata loss.
- Send-before-replay-write ordering, combined replay bounds, acknowledgement
  cleanup on a later response, no-token request repair and failure retention only
  until the original deadline. Cookie failure must not break direct capture.
- `Set-Cookie` cache privacy, unchanged upstream bytes/encoding, deterministic
  complete-envelope size limits and no cleanup cookies on unrelated responses.

Resolve before implementation activation:

1. Confirm deployment propagation/drain and rollback procedures for additive
   lifecycle metadata. Passing new-reader tests does not prove old-writer safety.
2. Confirm capture ordering expectations when an SDK issues overlapping token
   requests; no provider sequence is assumed by this foundation.
3. Identify external consumers of the diagnostic EID headers before retirement.
4. Provision dedicated publisher signing keys and verify rotation operationally.
   The implemented defaults are five minutes and 2,048 encoded bytes, including
   binding, metadata and signature overhead. Validate provider-approved ID5
   fixtures and cross-POP visibility/withdrawal behavior on the approved route.
   Synthetic local tests do not establish live acceptance or strong KV reads.

Provider expiry, writer and revision storage are implemented foundation decisions. LiveRamp protocol approval, identifier
intake, refresh response semantics and country-specific TTL remain for its
future spec. Arbitrary OpenRTB provenance storage remains outside this first
delivery; fixed Lockr ID5 provenance is reconstructed from the persisted writer.
