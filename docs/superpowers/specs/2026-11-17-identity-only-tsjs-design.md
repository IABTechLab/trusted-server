# Identity-only tsjs mode

Issue #1594 implements WB-6 of the server-side identity design. Stack on
#1489, which supplies the mirror cookie; it must land first.

## Contract

Add `integrations.prebid.mode`, accepting `auction` (default) and `identity`.
Unknown values fail typed configuration parsing. Both modes use the existing
external Prebid bundle, managed User ID configuration and consent handling.
Existing configurations retain the auction shim.

In identity mode, leave publisher bidders, ad units, GPT refresh and targeting
alone, including publisher auction timeout/debug settings. Register no Trusted
Server bidder. Capture Prebid User ID module EIDs into
`ts-eids` automatically when native IDs become available, using the existing
sanitization, size limit, expiry and empty-result clearing. Auctions remain an
additional capture opportunity, not a prerequisite. Do not change `getUserIdsAsEids` or capture
server mirror values back into this cookie.

Before each publisher auction, read `__Host-ts-eids-mirror`, the unpadded
base64url UTF-8 JSON envelope `{v:1,eids:[...]}` supplied by #1489. Validate its
version, 3,584-byte encoded size cap, source and UID shapes. `exp`, when
present, must be a finite nonnegative integer of Unix seconds later than now;
missing `exp` remains valid to match the writer contract. Omit invalid UIDs and strip the
mirror-only `exp` field. Preserve OpenRTB EID and UID metadata. Missing, invalid,
unsupported or empty mirrors are a no-op. Re-read every auction to honor cookie
replacement, removal and expiry without persisting mirror values in Prebid's
configuration. Server mirror construction remains responsible for ownership,
partner opt-in and consent eligibility; Prebid's activity controls remain on the
normal request path. This mode does not grant consent or contact the EC route.

Merge mirror values into request-local `ortb2.user.ext.eids`, preserving other
first-party data and publisher EIDs. Let Prebid merge configured global first-party data through its existing
array-union behavior; consult global EIDs only to avoid duplicate mirror UIDs. Deduplicate mirror additions by source, UID `id` and `atype`, retaining
publisher metadata on conflict and leaving existing publisher duplicates alone.
Preserve multiple different IDs for a source. Native User ID
modules continue to contribute through Prebid's normal enrichment path.

Recognize both publisher `user.eids` and `user.ext.eids`, which Prebid normalizes
into `user.ext.eids`, when avoiding duplicate mirror additions.

The wrapper preserves request options, callback receiver/arguments, return value
(including the original promise identity),
exceptions and default-auction behavior when options are omitted (a generated
options object supplies capture). Never mutate publisher request/global data. Installation is idempotent. Common managed User
ID setup precedes mode selection; auction-specific state and hooks follow it.

## Automatic native capture

Identity-only pages may initialize native User ID modules without ever calling
`requestBids`. Prebid 10.26 has no supported ID-update event, and its
`getUserIdsAsync` promise can wait for auction completion when `auctionDelay` is
zero. Configuration callbacks cannot establish that asynchronous IDs or consent
have settled. Waiting only for that promise or for an auction misses this path.

Sample the unmodified, consent-filtered native getter immediately after the
publisher queue runs, then once per second while this document is active.
Visibility changes request an immediate fresh read; browsers may throttle
background timers. Stop on `pagehide`, resume once on BFCache `pageshow`, and
remove listeners on permanent navigation. Never start auctions, force ID
refreshes, change Prebid's synchronization timing, or patch its getter.

Write only when the encoded native cookie differs from the browser's current
value. An empty, missing, invalid or failing getter clears stale capture once;
a repeated failure warns once until recovery. Resume and cookie expiry/removal
must recheck actual browser state, rather than a retained in-memory ID result.
Mirrored IDs remain outside the native getter and capture path. Auction mode
retains its existing callback-based capture and cookie refresh behavior.

Add no-auction, delayed-ID/configuration, consent denial/restoration, failure
recovery, unchanged-write, missing-cookie, visibility, BFCache and repeated
installation controls. Run a real generated Prebid bundle with `auctionDelay: 0`, without calling
`requestBids`; an available native ID must reach `ts-eids` automatically while
`getUserIdsAsync` remains pending. Denial must clear an already captured cookie
and restoration must recapture it. Permanent navigation removes all capture
listeners, repeated BFCache restoration leaves one timer, and auction mode
installs no sampler while retaining its unchanged-result cookie refresh.

## Alternatives and scope

A separate identity integration would duplicate bundle loading and managed ID
consent setup. Patching Prebid's EID getter would echo mirrored IDs into browser
ingest. A request-local wrapper shares the established integration and reaches
Prebid's actual bidder consumer without global mirror state.

The shared Rust configuration/injection and browser bundle apply to every adapter.
Fastly and Axum are the issue's MVP runtime targets. No adapter-specific identity
provider behavior, mirror writer, EC route or server auction semantics change.
#1599 owns consent-triggered EC fetching. Parent mirror lifetime corrections
remain obligations of #1489; this reader honors stated expiries and cannot
reconstruct unstated expiries.

## Verification

- Rust tests cover default, explicit and invalid modes and injected browser JSON.
- Unit tests cover positive mirror merge, metadata, malformed/expired input,
  replacement/removal, capture without echo, callback semantics, idempotence,
  publisher EIDs and default auction preservation.
- Run the generated external Prebid bundle and tsjs in a real browser. A controlled
  client bidder must receive a distinctive mirrored ID and native ID with no
  Trusted Server bidder or auction network request. Repeat after mirror removal;
  compare auction mode as a discriminating control. Deny `transmitEids`
  for the fixture bidder and verify Prebid redacts the mirror before delivery.
- Exercise normal server injection/static assets through Fastly and Axum where
  available, with disposable config and controlled origins. Record unavailable
  targets as blocked rather than substituting helper tests.
- Run repository CI gates, affected runtime regressions and formatting/lint.

## Review log

- Round 1 (Codex): corrected the description of Prebid array merging after
  checking the installed consumer. Defined UID deduplication and publisher
  metadata precedence, and added a real activity-redaction control.
- Round 2 (Codex): no blocking findings. Specified the exact size and expiry
  rules, default-options behavior and publisher-data immutability checks.

- Automatic-capture round 1: independent review found no blocking or optional
  findings after checking the native getter and current consent activity rules.
- Automatic-capture round 2: no blocking findings. Accepted optional controls
  proving capture while the async helper remains pending, withdrawal clearing an
  existing cookie, permanent navigation cleanup, a single BFCache timer and
  unchanged auction-mode cookie refresh behavior.
- Automatic-capture implementation review: contained cookie-access failures so
  initial capture and publisher callbacks remain safe when browser policy blocks
  cookies. Getter/setter failure and recovery regressions pass. Independent
  re-review found no remaining blocking or optional findings.
