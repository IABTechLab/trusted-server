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
`6bc15305fdf340f0a38fd79423047f2ecfdf119f`. The switch defaults to false. Explicitly
configuring another lifetime changes positive issuance even with renewal disabled;
it does not cause returning cookies to be reissued. No runtime environment reads,
extra secret, storage schema, dependency or EdgeZero change is introduced.
Older binaries reject unknown EC fields: remove the new settings and republish
before rolling back. Disabling renewal does not shorten cookies already held by
a browser; a changed lifetime applies on the next positive issuance or renewal.

## Supported platforms

Fastly and Axum already issue EC cookies through the shared identity lifecycle.
They gain optional same-value renewal. Cloudflare and Spin do not issue EC
cookies in #1646: they bind no identity record store, and their publisher paths
only read incoming identity. Their responses remain identical to #1646 for both
switch values. Adding issuance or renewal to those platforms is a separate
follow-up and may require platform storage work outside this repository.

Their unsupported EC endpoint remains a private empty 501 for GET/POST and 405
for unsupported methods, without additional request parsing or identity work.
No new eligibility path is introduced there.

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
edge-cache directives. Fastly must run its terminal guard after late identity
finalization; the other adapters retain their existing shared privacy middleware.

Some returning-reader pages are already private today: assembled/ad-synthesized
HTML uses `no-store, private`, and inactive ad-stack documents can retain a short
private browser policy. Other responses can retain a shared policy when no cookie
or personalization rule applies. Renewal makes those otherwise shareable final
responses private too; it does not newly change every page's cacheability.
For example, the base inactive-ad-stack non-OK HTML path preserves a public
origin policy when no other privacy rule applies. This is code-supported
cacheability, not a measurement of any deployed shared cache.

The template cookie deny list prohibits `ts-ec` and the configured SharedID name
as explicit cache-key dimensions. Keep its configured-name handling and cookie
independence/bypass rules. It is not a blanket prohibition on storing a neutral,
cookie-free raw template. Final personalized or cookie-bearing responses must
never become shared-cache entries.

This changes the meaning of #1595's returning-reader acceptance rule when enabled:
returning readers now intentionally receive Set-Cookie on eligible visits. The
issue and DEC-13 decision must explicitly acknowledge this before merge.

## Verification and rollout

Test lifetime defaults, omission, custom values, invalid bounds and all positive
issuance entry points. Test every observed record state separately, the exact
method/status/document matrix, API/asset exclusions, withdrawal, no extra record
operations and private renewal responses. Compare disabled/default response
fixtures to the base and verify actual routed Fastly/Axum behavior, including
late finalization. Verify Cloudflare/Spin unchanged for both switch values.

Run targeted core and adapter tests, pinned isolated Viceroy 0.21.0, touched-target
clippy, Rust formatting, changed-document formatting and the runtime environment
guard. Hosted CI and independent full-PR review precede handoff. Do not run the
full CI matrix locally. Safari/ITP, deployed platform and real-CMP qualification
are not established by local unit/runtime fixtures.
