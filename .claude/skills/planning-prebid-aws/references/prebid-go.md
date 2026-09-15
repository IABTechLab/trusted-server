# Prebid Go requirements

Resolve these requirements before claiming the infrastructure can serve the selected inventory. Go is fixed; select an approved release, image source, digest, and compatible CPU architecture.

## Caller and bidders

Identify the caller as browser Prebid.js, backend, or edge service. Capture a sanitized baseline containing eligible inventory, enabled bidders, account/placement parameters, timeouts, privacy signals, and host-specific endpoints.

For each bidder, confirm server-side authorization, host credentials, supported formats, approved endpoints, adapter support in the pinned release, and source-IP allowlists. Existing provider credentials and account IDs may not transfer to a self-hosted PBS.

Distinguish publisher request parameters from host-level adapter secrets. Map each secret to a real field or environment binding supported by the pinned adapter. A generic API-key environment variable does not configure arbitrary bidders. Ask for secret identifiers and required keys, never credential values.

For browsers, resolve CORS/OPTIONS, cookie scope, user-sync and callback URLs, consent-dependent endpoints, and identity behavior across regions. For backend/edge callers, define trusted forwarding hops and preserve device IP, privacy signals, request identifiers, and the caller deadline. Verify outgoing bidder requests use the intended device context rather than the proxy's identity.

## Configuration

PBS Go supports environment variables and configuration files. Resolve one nonsecret configuration file and verify precedence, search paths, required fields, and environment-name mapping against the selected release. Use a parser for structured overrides; PBS is not an arbitrary multi-file YAML merger.

Account for external URL, listener, enabled adapters, bidder endpoints, auction timeout, privacy defaults, account policy, user sync, stored requests, cache, metrics, and logs. Privacy choices need an approved policy owner; do not weaken them to make a smoke test pass.

Preserve required upstream static assets when assembling or mounting the runtime. Check startup output and adapter debug paths with dummy values for unintended disclosure, even if PBS documents secret redaction.

## Stored requests, accounts, and cache

Ask whether eligible auctions reference stored-request IDs or depend on dynamically updated account settings. For a small fixed set, local versioned definitions may suffice. Otherwise reproduce the update and lookup behavior or explicitly restrict eligibility. Validate every referenced ID used in fixtures.

Video and AMP can introduce Prebid Cache requirements. Determine the actual request and response behavior rather than assuming all formats share the same dependencies. Specify write location, public retrieval URL, expiration, and failure behavior.

With independent regional caches, a key written in East must remain retrievable from the URL returned in that auction. A shared latency-routed lookup hostname can send retrieval to the wrong cache. Choose region-specific retrieval URLs with accepted availability limits, or a storage/routing design that guarantees retrieval through the approved failure scenario. Do not substitute a database product for a verified cache integration.

## Pilot allocation and rollback

Load this section when comparing with an existing provider or migrating traffic.

- Define eligibility and the sampling unit, such as session or auction. Preserve cohort membership as allocation increases; zero allocation overrides old assignments for new auctions.
- Select a complete provider configuration together, including auction/sync endpoints, account IDs, and request transformations. Preserve unrelated client-side bidders.
- Keep experiment allocation in the caller or its remotely refreshed configuration. DNS regional routing is not precise per-auction allocation and is not the kill switch.
- Specify configuration refresh, maximum staleness, failure behavior, and the observed time to stop new experimental requests. Keep healthy endpoints available while in-flight work and cached configurations drain.
- Send each live auction through its selected provider. Do not duplicate bidder auctions for comparison or add unconditional sequential fallback after a partial auction or timeout.

Agree numeric transport-error, timeout, latency, and business thresholds, with baseline, sample size, observation window, and response owner before live rollout. Measure assigned and observed traffic share, caller p50/p95/p99 latency, bidder failures/no-bids, identity match where relevant, and revenue using a consistent denominator. Record region, release, and cohort without full-payload logging by default.

A legitimate no-bid is not an infrastructure failure. `/status` is one health signal, not proof of bidder permission, valid demand, or business equivalence. Plan auction-level smoke checks and delayed business evaluation separately.

## Sources

Use these entry points, then inspect the corresponding tag or commit for the chosen PBS Go release. Upstream `master` is navigation, not a reproducible configuration contract.

- [Go configuration guide](https://github.com/prebid/prebid-server/blob/master/docs/developers/configuration.md)
- [Go configuration definitions](https://github.com/prebid/prebid-server/blob/master/config/config.go)
- [Go stored requests](https://docs.prebid.org/prebid-server/features/pbs-storedreqs-go.html)
- [Prebid.js PBS integration](https://docs.prebid.org/dev-docs/modules/prebidServer.html)
- [PBS status endpoint](https://docs.prebid.org/prebid-server/endpoints/pbs-endpoint-status.html)
- [PBS overview and cache dependencies](https://docs.prebid.org/prebid-server/overview/prebid-server-overview.html)
