# Trusted Didomi SDK geography

## Problem and scope

Issue #1151 tracks the legacy SDK fallback that copies caller-controlled
`FastlyGeo-*` headers into Didomi geo headers. PR #1127 protects canonical notice
loaders only. Fix the remaining SDK paths without changing the API URL contract
or extending loader canonicalization to unrelated assets.

## Approved design

- Resolve SDK geography from `RuntimeServices.geo()` using trusted client info.
  Normalize it with the existing complete country/region validator. Never derive
  upstream geography from inbound headers.
- Eligible notice loaders with `geo_query_parameters = true` retain their
  canonical redirect, complete-geo requirement, private failure responses, and
  canonical URL cache identity.
- Other SDK requests forward the normalized trusted pair when available, or omit
  all three derived geo headers on unavailable, failed, incomplete, or invalid
  geo. They continue to proxy without adding geo query parameters.
- Only canonical notice loaders retain default outbound caching and upstream
  cache metadata. All other SDK requests bypass the outbound cache and enforce
  terminal `no-store, private` responses, including removal of validators and
  independent CDN cache directives. Keep SDK CORS behavior.
- API requests retain cache bypass, private responses, and no geo lookup/headers.

## Tradeoffs and compatibility

Omitting every fallback geo header would lose usable platform geo. Extending
canonicalization to every SDK asset would broaden the URL contract. Reusing the
existing validator and privacy controls is the smallest safe change.

Deployments relying on inbound headers must move to trusted platform geo.
Country-only platforms omit the full header group. Noncanonical SDK traffic loses
cache reuse and may increase origin traffic. Operators must purge previously
cached SDK responses and validate notice selection and origin capacity. Canonical
loader geo lookup remains fail-closed; this change does not solve geo availability.

## Acceptance and verification

Regression tests cover loaders and other SDK assets with geo mode enabled and
disabled, spoofed source and destination geo headers, normalized platform geo,
missing/invalid/error geo, POST loader requests, cache bypass and stripped cache
metadata, preserved canonical caching, and unchanged API isolation. Existing
conditional request headers remain excluded from upstream forwarding.

Run focused native Didomi tests first, observing new tests fail before the fix.
Then run target-matched adapter tests/lints and repository CI gates. Independent
subagents review the complete diff against `main` for correctness and security.

## Approval

The user approved the recommended design in chat on 2026-10-19.

## Review log

- Independent design review: approved with no blocking gaps.
- Independent final security and correctness reviews against `main`: no findings
  requiring changes; reviewers confirmed the implementation matches this design.
