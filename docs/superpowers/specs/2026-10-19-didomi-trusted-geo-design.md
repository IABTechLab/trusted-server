# Trusted Didomi SDK geography

## Problem and scope

Issue #1151 tracks the legacy SDK fallback that copies caller-controlled
`FastlyGeo-*` headers into Didomi geo headers. PR #1127 protects canonical notice
loaders only. Fix the remaining SDK paths without changing the API URL contract
or extending loader canonicalization to unrelated assets.

## Design

- Resolve SDK geography from `RuntimeServices.geo()` using trusted client info.
  Normalize it with the existing complete country/region validator. Never derive
  upstream geography from inbound headers.
- Eligible notice loaders with `geo_query_parameters = true` retain their
  canonical redirect, complete-geo requirement, private failure responses, and
  canonical URL cache identity.
- Other SDK requests forward the normalized trusted pair when available, or omit
  all three derived geo headers on unavailable, failed, incomplete, or invalid
  geo. They continue to proxy without adding geo query parameters.
- Canonical notice loaders and other SDK GETs except paths ending `/loader.js`
  retain default outbound caching and all origin cache metadata. Noncanonical
  loaders (including root and nested paths) and SDK POSTs bypass the outbound cache
  and enforce terminal `no-store, private`, including removal of validators and
  independent CDN cache directives. Keep SDK CORS behavior.
- API requests retain cache bypass, private responses, and no geo lookup/headers.
- `ts audit generate` leaves detected Didomi disabled and lists it for manual
  review, because the audit cannot pick an adapter-specific geo and
  loader-caching mode.

## Tradeoffs and compatibility

Omitting every fallback geo header would lose usable platform geo. Extending
canonicalization to every SDK asset would broaden the URL contract. Reusing the
existing validator and privacy controls is the smallest safe change.

Deployments relying on inbound headers must move to trusted platform geo.
Country-only platforms omit the full header group. Noncanonical loaders lose cache
reuse and query Didomi on every request, risking automatic throttling and
proxy disablement under Didomi's reverse-proxy requirements. Fastly production
operators should validate geo coverage and enable canonical loader URLs. Other
adapters currently have no compliant loader-caching mode through this proxy and
should use direct SDK delivery. Other SDK GETs follow origin cache policy.
Operators must purge previously cached SDK responses and validate notice selection.
Canonical loader geo lookup remains fail-closed; this change does not solve geo
availability.

## Acceptance and verification

Regression tests cover loaders and other SDK assets with geo mode enabled and
disabled, spoofed source and destination geo headers, normalized platform geo,
missing/invalid/error geo, POST loader/asset requests, root/nested loader cache
bypass and stripped metadata, preserved static/canonical caching, and unchanged
API isolation. Existing conditional request headers remain excluded from upstream
forwarding.

Run focused native Didomi tests first, observing new tests fail before the fix.
Then run target-matched adapter tests/lints and repository CI gates. Independent
subagents review the complete diff against `main` for correctness and security.

## Review references

PR #1414 reviews identified excessive cache bypass for static SDK assets and the
remaining geo-less loader throttling trade-off. This design retains loader bypass
while restoring origin caching for other SDK GETs and documenting the production
constraints.
