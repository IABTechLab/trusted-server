# DEC-13 proposal: configurable EC lifetime and sliding expiration

Proposal only; off by default. Do not merge until DEC-13 is formally recorded as
decided and issue #1595's returning-reader no-Set-Cookie acceptance rule is adjusted.

Part of #1595. The team has selected same-value renewal on each eligible visit;
the formal decision record remains open. This document describes the proposed
implementation, not authorization to merge or deploy it.

## Configuration and compatibility

Two typed application settings, published through the existing `ts config push`
flow, control the feature:

```toml
[ec]
max_age_days = 365
sliding_refresh = false
```

`max_age_days` is an integer from 1 through 400 inclusive. It defaults to 365;
365 days means exactly 31,536,000 seconds, preserving the existing cookie header.
All positive EC issuance uses this lifetime, including creation, proven-orphan
replacement and optional renewal. Withdrawal still emits Max-Age=0.

Reject invalid values on TOML/JSON configuration load and typed publication with
an error-stack configuration error identifying `ec.max_age_days` and its range.
The upper bound matches Chrome's documented 400-day cap for Max-Age and Expires.
[Chrome's expiration limit](https://developer.chrome.com/blog/cookie-max-age-expires) <!-- allow-domain: developer.chrome.com -->
This authoritative browser reference justifies the validation bound; it does
not promise that browsers retain cookies for the entire requested lifetime.

Both defaults are omitted when serializing configuration. With no new settings,
responses, storage operations and cookie attributes match #1646 at
`aa315593fdf7b801ace5b5a12581995958eb74f2`, the base head merged into this
proposal. That includes WB-1's core-generated identity expiries on any finalized
response and their private, non-storable cache policy. The switch defaults to false. Explicitly
configuring another lifetime changes positive issuance even with renewal disabled;
it does not cause returning cookies to be reissued. No runtime environment reads,
extra secret, storage schema, dependency or EdgeZero change is introduced.
Older binaries reject unknown EC fields, so deploy the new binary to every
service version that reads a config store before pushing config that sets either
field, and remove the new settings and republish before rolling back. The pinned
EdgeZero environment overlay overrides only keys already present in the TOML, so
`TRUSTED_SERVER__EC__*` overrides for these fields take effect only once the key
is written there. Disabling renewal does not shorten cookies already held by
a browser; a changed lifetime applies on the next positive issuance or renewal.

## Supported platforms

Fastly and Axum already issue EC cookies through the shared identity lifecycle.
They gain optional same-value renewal. Cloudflare and Spin do not issue EC
cookies in #1646: they bind no identity record store, and their publisher paths
only read incoming identity. Their responses remain identical to #1646 for both
switch values. Adding issuance or renewal to those platforms is a separate
follow-up and may require platform storage work outside this repository.

Their unsupported EC endpoint remains a private empty 501 for GET/POST and 405
for unsupported methods, with no identity work beyond #1646's existing request
preparation, which can still return its private 400. No new eligibility path is
introduced there.

## Eligibility and output

When enabled, the existing publisher lifecycle must first authorize the visit.
Reuse #1646's classification and final-response gate exactly:

- `Sec-Fetch-Dest: document` wins; the existing Accept fallback remains unchanged.
- There is no new GET-only or successful-status-only restriction.
- Exclude the five existing redirect statuses: 301, 302, 303, 307 and 308.
- Accept HTML/XHTML documents or a 304, regardless of its Content-Type.
- Configured asset routes are GET/HEAD routes and never renew. A POST to the same
  prefix uses the publisher fallback and follows its document rules.
- Named API/local routes, including EC POST, never renew a returning cookie even
  when document-looking headers are supplied.
- The client must be allowed identity creation. #1646 gives a client it refuses
  (a known non-browser) no navigation permit, so its held EC is never renewed
  and renewal adds no record I/O for it.
- Request consent must allow EC and must not explicitly withdraw consent.
- Require a valid incoming browser EC equal to the final active EC. Ignore the
  incoming `x-ts-ec` header, as #1646 does. Do not add a header-agreement rule.
- Creation and rotation retain their existing issuance path and do not receive
  a second EC Set-Cookie from renewal.

Renewal emits the same EC value with its full configured lifetime and the existing
Domain, Path, Secure, HttpOnly and SameSite attributes. It adds no identity header,
new identifier or companion cookie. Companion-cookie equality rules stay unchanged.

## Record state and retention

The user-approved rule adds no reads or writes solely for renewal:

| Observed state                                             | Renewal of the held EC                                       |
| ---------------------------------------------------------- | ------------------------------------------------------------ |
| Unread                                                     | Allowed without reading the record                           |
| Storage unavailable / no bound store                       | Allowed on Fastly/Axum document paths                        |
| Known missing                                              | Suppressed; existing proven-orphan recovery remains separate |
| Known revoked                                              | Suppressed                                                   |
| Known read failure                                         | Suppressed                                                   |
| Navigation write failed                                    | Suppressed                                                   |
| Matching consented record, successful or unnecessary write | Allowed                                                      |

The asymmetry between the unavailable-store and read-failure rows is intended:
with no bound or openable store nothing is observed, so the record stays unread,
while a read that was attempted and failed is an observation that suppresses
renewal.

Existing record lookup, conditional-write, orphan recovery and after-response
rules remain unchanged. Renewal does not extend record TTL or retention. Record
expiry may still cause legitimate orphan rotation on a later observation.

**Limitation:** a record revoked out of band while unread on this request still
has its cookie renewed until a later request reads and observes that revocation.
No-new-reads policy deliberately accepts that visibility limit. It does not permit
renewal after an explicit withdrawal carried on the current request.

## Companion inventory

- SharedID and its configured-name `_cst` consent cookie retain their separate
  one-year lifetime. They represent separate identity/consent state and are not
  renewed merely because the EC is renewed.
- `__Host-ts-eids-mirror` retains its independent one-year browser lifetime and
  its existing per-EID expiry and unchanged-value suppression. Split its constant
  from the EC lifetime; there is no approved requirement to couple the two.
- The client EID cookie retains its existing one-day policy.
- Existing withdrawal deletions and retirement cleanup of `ts-ec-pull-complete`
  remain unchanged.

## Shared-cache consequences

Every response that gains Set-Cookie is private and non-storable in shared caches
under `enforce_set_cookie_cache_privacy`. Preserve stricter policies and remove
edge-cache directives. The shared core finalization applies this guard on every
adapter; Fastly's terminal guard and the other adapters' privacy middleware
reapply it after later header changes.

Some returning-reader pages are already private today: assembled/ad-synthesized
HTML uses `no-store, private`, and inactive ad-stack documents can retain a short
private browser policy. Other responses can retain a shared policy when no cookie
or personalization rule applies. Renewal makes those otherwise shareable final
responses private too, and on inactive ad-stack documents it replaces the
generated `private, max-age=60` browser allowance with `private, max-age=0`. It
does not newly change every page's cacheability.
For example, the base inactive-ad-stack non-OK HTML path preserves a public
origin policy when no other privacy rule applies. This is code-supported
cacheability, not a measurement of any deployed shared cache.

The template cookie deny list prohibits `ts-ec` and the configured SharedID name
as explicit cache-key dimensions. Keep its configured-name handling and cookie
independence/bypass rules. It is not a blanket prohibition on storing a neutral,
cookie-free raw template. Final personalized or cookie-bearing responses must
never become shared-cache entries.

Because there is no method or success-status restriction, renewing HEAD, non-GET
and HTML error documents also carry Set-Cookie and become private.

**Limitation:** an origin `Set-Cookie: ts-ec=...` on an eligible document passes
through, as it already does for creation in #1646, so renewal can append a second
`ts-ec` header to it. Stripping origin identity cookies from eligible responses is
a lifecycle change for #1646, not this proposal.

This changes the meaning of #1595's returning-reader acceptance rule when enabled:
returning readers now intentionally receive Set-Cookie on eligible visits. The
issue and DEC-13 decision must explicitly acknowledge this before merge.

## Verification and rollout

Test lifetime defaults, omission, custom values, invalid bounds and all positive
issuance entry points. Test every observed record state separately, the exact
method/status/document matrix, API/asset exclusions, withdrawal, no extra record
operations and private renewal responses. With renewal disabled, check that
origin headers and cookies match the expected #1646 matrix, and run #1646's own
lifecycle and WB-1 tests unchanged against this branch. Verify routed Fastly
behavior through the router plus the entry point's finalization and terminal
helpers, and Axum through its native routes. Verify Cloudflare/Spin unchanged for
both switch values on EC GET/POST.

Run targeted core and adapter tests, pinned isolated Viceroy 0.21.0, touched-target
clippy, Rust formatting, changed-document formatting and the runtime environment
guard. Hosted CI and independent full-PR review precede handoff. Do not run the
full CI matrix locally. Safari/ITP, deployed platform and real-CMP qualification
are not established by local unit/runtime fixtures.

## Review log

- Round 1 (Claude Code, fresh session, after merging #1646 at `aa315593f`): no
  blocking findings. Corrected the cookie-privacy section (core finalization
  applies the guard on every adapter; renewal replaces the inactive ad-stack
  `max-age=60` allowance), stated the unavailable-store versus read-failure
  asymmetry, narrowed the Cloudflare/Spin EC route wording to #1646's existing
  request preparation, and reworded the verification claims to match the actual
  tests. Tightened the creation and rotation tests to assert exactly one EC
  Set-Cookie.
- Round 2 (Claude Code, Sonnet, fresh session): no blocking findings. Added the
  forward-rollout order and the environment-overlay caveat, noted that renewing
  HEAD, non-GET and error documents become private, and recorded origin `ts-ec`
  pass-through on eligible documents as a limitation inherited from #1646. Its
  claim of a missing Cloudflare parity test was wrong: the test is in the
  Cloudflare `tests/routes.rs`. Did not restate #1646's consent-default
  semantics, which this proposal does not change.
