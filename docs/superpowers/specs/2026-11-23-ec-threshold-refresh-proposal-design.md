# DEC-13 proposal: optional EC threshold refresh

Proposal for DEC-13; off by default; do not merge until DEC-13 is decided. If DEC-13 is decided as never refresh, this PR will be closed.

Part of #1595. Status: implementation approved as a proposal on 2026-11-23, with immediate bootstrap renewal; DEC-13 remains open.

## Baseline and objective

Base this work on #1646, `feat/1574-axum-shared-identity`, inspected at
`6bc15305fdf340f0a38fd79423047f2ecfdf119f`. Merge subsequent base heads into
the proposal branch; never rebase or change #1646. DEC-13 remains open in
the all-adapters identity design. This document proposes an implementation
for comparison with retaining the existing never-refresh policy; it does
not decide DEC-13.

Add optional integer `ec.refresh_after_days`, accepting 1 through 364,
inclusive. Omission disables renewal and marker processing except cleanup on
explicit withdrawal of a carried marker. A day is 86,400 seconds,
not a calendar boundary. Invalid configuration must identify the setting
and supported range during config validation/push and application startup.
The value comes from the typed app config, published with `ts config push`.
Add no serving-process environment reads, clock overrides, secret references,
dependencies, or EdgeZero changes.

When enabled, refresh the same `ts-ec` value with its existing security
attributes and `Max-Age=31536000` only on an eligible publisher document,
with EC consent and a proven live matching record, when a valid marker is
at least N days old or immediately when no valid marker exists. Refresh does not rotate identity or refresh SharedID, its consent
companion, or the EID mirror.

## How to know the cookie's age

Requests carry neither cookie expiry nor proof of receipt of earlier
responses. All alternatives must distinguish an issuance attempt from
browser acceptance. No design can guarantee two response cookies are
accepted together.

| Option                                 | Meaning and compatibility                                                                                                                                                                                                     | Cost and failure behavior                                                                                                                                                                                                                                                                                                                                                                          |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A: optional timestamp in the EC record | Last successful issuance-intent commit for the whole EC, not per-browser receipt. A missing optional field preserves reading old v1 records; omit it while disabled. Older binaries accept but discard the field on rewrites. | Coalesce the timestamp into the existing single page-load update and size cap. Exactly one CAS attempt; only a successful create/update may authorize emission. Failed writes/conflicts mean no refresh; retry on a later navigation. A committed write with a lost response postpones renewal despite the browser retaining its old expiry. Browsers sharing an EC suppress each other's renewal. |
| B: authenticated companion cookie      | Issuance time returned by this browser, bound to this EC and publisher. No EC or record schema change. Old records remain unchanged.                                                                                          | One additional HttpOnly cookie, verification, and request bytes. No refresh-specific record write or CAS. Lost responses leave the browser's previous marker available for a later retry. Missing or invalid markers bootstrap by refreshing the same EC and setting its marker together. Concurrent responses can both refresh and arrive out of order.                                           |
| C: record creation time alone          | Birth of the row, not the last cookie issuance. Current live-row updates preserve `created`.                                                                                                                                  | Once `now - created >= N days`, every subsequent navigation refreshes. Mutating `created` destroys its existing meaning and turns this into Option A. Reject as an ongoing clock.                                                                                                                                                                                                                  |

For A, a missing timestamp cannot reveal historical browser issuance. The
conservative migration is a separate first-observed baseline committed by
CAS, without claiming a refresh occurred; alternatively refresh immediately
and explicitly accept an unknown-age migration exception. Neither choice
recovers the missing history. A must preserve its new field in after-response
and provider updates, and must not advance it after response headers are sent.

**Approved proposal: B**, because the requested threshold concerns the current
browser cookie, rather than the latest record write. A returning valid EC
with no valid marker bootstraps by refreshing that same EC and issuing its
marker in the same eligible, consented response. Compared with marker-only initialization, this response would already
be cookie-bearing and private, so adding EC renewal costs no additional
cacheability and avoids premature cookie expiry. Compared with disabled
behavior, bootstrap does add a private response.

**Record-retention limit:** this extends browser lifetime only. It does not
update or extend the backing record TTL; a later expired record can still
cause legitimate orphan rotation. That limit is part of the approved scope.

## Proposed companion protocol

Name: `ts-ec-issued-at`. Attributes match `ts-ec`: publisher EC cookie domain,
`Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=31536000`. It contains no new
random identifier or provider data.

Value: `v1.i.<unix-seconds>.<base64url-hmac>`, with kind `i` for actual
EC issuance, including bootstrap renewal. Use canonical decimal Unix
seconds, full HMAC-SHA256, and unpadded URL-safe base64. Bound input before
decoding (80 bytes maximum); the cookie name/value pair is at most 96 bytes.
Authenticate an unambiguous framed encoding of a fixed protocol/domain
separator, publisher domain, full EC value, kind, and timestamp, with the
existing EC passphrase. Verify using the MAC library's verification API.
Never expose the passphrase or raw cookie values in logs.

**Key-derivation follow-up:** v1 keeps the existing passphrase as its HMAC
key and authenticates a purpose-separated message. Deriving a distinct marker
key with audited HKDF-SHA256 and a fixed purpose label is deferred: the workspace
currently has no HKDF dependency, and changing key derivation needs a versioned
marker protocol and explicit migration policy. A follow-up should specify the
salt/info bytes, add independent vectors, and decide between old-v1 verification
and one-time bootstrap after the new version. No derivation or silent protocol
change is included in this review fix.

The authenticated bytes are the ASCII string
`trusted-server/ec-cookie-issued-at/v1` followed by one zero byte, then four
length-prefixed fields in order: scope, full EC, kind, timestamp. Each field
is encoded as its byte length in unsigned 32-bit big-endian form followed
by its bytes. Scope is the validated `publisher.domain` with ASCII letters
lowercased, preserving every other byte; it comes from settings, not request
headers. Full EC preserves its validated suffix case. Kind is one ASCII byte
(`i`); timestamp is its canonical decimal ASCII representation with
no leading zero except `0`. The signing key is the existing passphrase's
bytes. Do not change publisher normalization or outbound cookie-domain
formatting. Include a fixed cross-language MAC test vector in implementation
tests and use the same byte protocol for controlled browser seeding.

Enabled instances capture at most the first eight candidates across all Cookie
fields before routing consumes the request, counting malformed or oversized
copies toward that limit. If any of those candidates authenticates for this EC and
publisher and is recent, skip renewal, even when another copy is invalid,
oversized or due. Duplicate names alone must not force cookie writes. If no
candidate verifies as recent, missing, malformed, oversized, wrong-version,
bad-MAC, wrong-EC/domain or future values have unknown age and cause bootstrap;
valid due values cause renewal. Refresh the unchanged EC and issue a fresh
signed marker together, subject to every document, consent and live-record
gate below. A recent valid copy beyond the first eight is ignored and extra
duplicates can therefore cause additional private renewals. Never infer age
from record creation or update time.

Disabled instances do not capture or verify marker values. Unsupported EC routes
first probe for marker-name presence only: without a marker, return the bare 501
before geo lookup, identity/consent parsing, EC context info logging or body reads. With a
carried marker, accepted explicit withdrawal can expire it without authenticating
its value. Other disabled identity paths scan for its name only on explicit
withdrawal. Missing or unresolved consent does not trigger cleanup.

Use `checked_current_timestamp`, checked subtraction, and seconds arithmetic.
With unavailable/pre-epoch time, emit no refresh or initialization. Future
markers are unknown age and therefore bootstrap when a usable current time
exists. Wall clocks need to be reasonably synchronized; no production clock
override is added. Missing/rejected/replayed markers can increase renewal
frequency; there is no strict once-per-period server-side guarantee.

## Lifecycle and eligibility

Do not infer document eligibility solely from `NavigationPermit` or
`EidSyncSource::Navigation`: the EC POST route uses both. Capture a distinct
publisher-document authorization in `IdentityApp::prepare_navigation`, after
request filters pass. Confirm it using #1646's final-response rule in
`ec/app/lifecycle.rs`: HTML/XHTML, or any 304 regardless of Content-Type, excluding
301/302/303/307/308. Preserve its method-independent eligibility and admitted
nonredirect error documents. EC API requests never receive returning-cookie
refresh authorization, even if they carry document-looking headers.

Decide after the existing page-load operation, from the final active EC and
record snapshot. Require all of the following for refresh:

- The setting is present and the response is an authorized publisher document.
- Current request consent permits EC, with no explicit withdrawal.
- A valid incoming `ts-ec` exactly matches the final active EC; it was not
  generated or rotated on this request. Header-only and header/cookie-mismatch
  identities cannot qualify.
- A matching `EcKvSnapshot::Present` holds a consented live record. A generic
  successful page-load return flag does not prove this: existing code can
  return success without reading a row.
- Any page-load conditional write attempted on this navigation succeeded.
  Read failures, missing rows, tombstones, unsupported storage and failed CAS
  suppress refresh. Existing orphan recovery stays separate.

If refresh otherwise qualifies and the snapshot is unread, enabled instances
may read it once. Reuse a matching readable snapshot when available. Do not
change the disabled read/write path. Never create/recover a record solely
for refresh or add a second navigation write.

| State after the gates                                | Cookie output added by this feature                                                                                                                                    |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Setting absent                                       | No renewal, value parsing, record reads, serialization changes, or new cache-key restrictions; explicit withdrawal still deletes a carried marker                      |
| Known age below N days                               | None                                                                                                                                                                   |
| Known age equal to or above N days                   | Same `ts-ec`, full existing Max-Age, plus fresh `i` companion                                                                                                          |
| Unknown age with usable clock                        | Same `ts-ec`, full existing Max-Age, plus a fresh `i` companion                                                                                                        |
| Existing creation/rotation successfully issues an EC | Add an `i` companion alongside that EC; this also applies to genuine EC creation through the explicit EC endpoint, and is initialization, not returning-cookie refresh |
| Explicit withdrawal                                  | No renewal; expire a carried companion even with the setting absent, without an EC cookie or working storage                                                           |
| Merely missing/unresolved consent                    | No renewal; preserve existing EC handling                                                                                                                              |

Do not add `x-ts-ec` or new identity response headers during refresh. Append
the existing EC attributes unchanged. Prepare/validate both enabled-issuance
header values before adding them so companion construction failure cannot
produce a renewal without its new marker. Creation/rotation must retain
the existing successful EC output if companion construction fails; missing
metadata then follows the bootstrap renewal rule later.

Include the companion in enabled identity-cookie filtering. Ineligible
responses cannot leak positive origin-supplied companion cookies. Reconstruct
only required withdrawal deletion after filtering, including marker-only
withdrawal on a non-document, the EC GET/POST routes and configured Fastly asset
bypasses. EC requests without record storage retain their private empty 501
response and perform only best-effort marker cleanup when a marker is carried
or refresh is enabled, with no record access,
creation, SharedID write-back or provider calls. A POST must pass the existing
same-origin and snapshot validation before its effective consent can authorize
cleanup; rejected events retain the original cookie-free 501.
When disabled, marker classification applies only to explicit
withdrawal cleanup; every other disabled response retains its existing handling.

## Storage, concurrency, privacy and cache consequences

B changes no record format, row timestamp, row TTL or conditional-write
contract. Ordinary page-load and after-response writes keep existing behavior.
The current live record TTL is one year and only actual writes extend it;
cookie renewal alone does not. A later record expiry can legitimately cause
#1646's proven-orphan rotation. Extending record retention would require a
separate, explicit scope decision and a CAS-protected update coalesced with
the navigation's one write, not an EdgeZero change hidden here.

No cookie design proves atomicity with concurrent withdrawal or browser
receipt. Reject observed revoked snapshots and failed navigation writes;
never resurrect a row. Concurrent due requests can both renew, and an older
response can replace a newer marker. A copied/replayed valid old marker can
cause extra renewals; signing prevents alteration and cross-EC substitution,
not replay. Strict server-enforced once-per-period renewal is not promised.
Passphrase rotation invalidates old markers and causes bootstrap renewals.

With sequential accepted cookies, one renewal response occurs on the first
eligible visit at least N days after issuance. Creation has no extra
uncacheable response beyond its existing EC write. Migration adds one
private bootstrap response per browser in the normal case. Missing,
rejected, replayed or concurrent cookies can increase this frequency.
The marker adds request bytes on every matching request, including assets,
but assets cannot renew it.

Every renewal and deletion uses existing Set-Cookie privacy guards:
private cache policy, removal of edge-cache directives, and Fastly's terminal
guard after late identity finalization. Never store companion headers in
shared templates. While enabled, reject the companion in template-cache key
cookies and reject a configured SharedID name/companion collision with it.
Keep the existing configured SharedID deny-list behavior unchanged. While
disabled, do not introduce these new validation restrictions.

No cookie output does not imply public cacheability. Existing inactive-ad
responses can remain `private, max-age=60`, and assembled per-reader HTML
remains private; a cookie-free raw template may independently be reusable.

## Safari and rollout

WebKit's documented seven-day script-created-storage rule is not a blanket
cap on first-party HTTP-set HttpOnly cookies. Its detected CNAME/IP-cloaking
defenses can cap HTTP cookies, and other ITP deletion rules still apply.
Neither this timestamp nor a threshold reveals the browser's effective
expiry or guarantees one-year persistence. Safari/ITP is not verified by
the local Chrome/Viceroy tests and remains a deployment/browser qualification.
Source checked 2026-11-23; this one-off browser-policy reference needs no
global domain-allowlist addition.
[WebKit source](https://webkit.org/tracking-prevention/) <!-- allow-domain: webkit.org -->

For either Fastly or Axum, after the proposal is approved for deployment,
an operator may add this to the app-config copy and publish it using the
adapter's existing `ts config push` flow:

```toml
[ec]
refresh_after_days = 30
```

Use the same field for both adapters. Fastly consumes the validated config
store envelope; Axum loads its prepared configuration snapshot at startup,
so publish/update that snapshot and restart Axum. No runtime environment
setting can enable the feature. Existing CLI publication-time overlays
remain tooling behavior, not serving-time environment reads.

Omission must serialize without a new null/default field, preserving old
config payload bytes. An enabled blob is incompatible with older binaries
that deny unknown EC settings: remove the field and republish before rolling
back. Disabling stops renewal and leaves an existing companion until browser
expiry or an explicit withdrawal; withdrawal deletes it even while disabled.
Re-enabling may reuse a still-valid companion only when no intervening
withdrawal removed it. Older binaries that predate the marker cannot perform
this cleanup after rollback and may leave it until browser expiry. The feature
is not a new consent grant or a guarantee of identity continuity.

The opaque signed marker can still link requests while carried, even after the
EC cookie itself disappears. Deleting it on withdrawal reduces that re-linking
risk across disable/re-enable transitions; it does not erase previously logged
requests or guarantee the browser accepts deletion. Missing or unresolved consent
alone must not be interpreted as withdrawal. Copies with a different `Path` or a host-only domain
survive this scoped withdrawal deletion, just as copies of `ts-ec` do.

## Acceptance and verification

1. Absent-setting serialization, response headers/cookies and record I/O
   match #1646 except explicit withdrawal deletes a carried marker. Existing
   returning-reader tests stay green; explicit absent setting tests cover markers present and absent, creation, rotation,
   withdrawal, unknown consent and configured cache-cookie policies.
2. Enabled setting rejects out-of-range values with an actionable field/range
   diagnostic through typed config validation and runtime startup. No serving
   environment read is added; the runtime environment guard passes.
3. Pure clock-explicit tests cover exact threshold, below/above threshold,
   missing/invalid markers and duplicate candidates, bindings, signature failures, future
   time, clock failure and input bounds. No time-based sleeps.
4. Full lifecycle tests cover document HTML/XHTML/304 and allowed methods,
   304 with an explicit non-HTML Content-Type, redirects/error documents,
   API routes with misleading navigation headers,
   assets, header-only/mismatched EC, live/missing/failed/tombstoned records,
   no extra writes, failed CAS, creation, rotation and withdrawal filtering.
5. SharedID equality, configured-name template denial, mirror equality,
   response-privacy and Fastly terminal-guard preservation tests pass.
6. Axum and Fastly/Viceroy 0.21.0 run behind the publisher proxy in fresh
   owned Chrome profiles: unset repeated visits; enabled bootstrap; seeded
   old valid issue marker with N=1; same-value private renewal; immediate
   no-renewal revisit; no-consent and withdrawal. Seed the authenticated
   token with synthetic local secrets through the documented format, using
   CDP HttpOnly-cookie setup; do not add a runtime clock/key override or
   sleep for days. Label this controlled seeding, not elapsed browser time.
7. Create only a draft proposal PR, with the warning at this document's top
   at the top of its body, the comparison, and `Part of #1595`. No automatic
   close keyword, merge, decision-record edit, or publication to other PRs.

Run targeted checks only, then watch every hosted check to completion after
an authorized push. Independently review spec, plan and final PR, resolve all
blocking and optional findings, and retain precise runtime limitations.

## Review log

- 2026-11-23, design round 1: corrected the eligibility description to admit
  all 304 responses regardless of Content-Type; added outer-lifecycle test
  commands, reproducible startup requirements and base-movement verification.
- 2026-11-23, design round 2: specified exact authenticated byte framing and
  the MAC-vector requirement; resolved all remaining design and plan findings.
- 2026-11-23, approved bootstrap amendment: replaced observation-only bootstrap
  with same-response EC-plus-marker renewal, retained browser-only lifetime
  scope, and removed the unused observation-marker protocol mode. Re-review
  resolved all stale bootstrap descriptions.
- 2026-11-23, implementation review: failing regressions demonstrated header
  mismatch, direct EC GET withdrawal and refresh-read mirror interactions.
  The corrections preserve disabled identity selection, delete withdrawn
  markers without requiring storage, and keep refresh-only snapshots local.
  All 15 lifecycle tests pass; source re-review has no remaining findings.
