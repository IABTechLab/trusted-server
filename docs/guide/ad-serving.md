# Ad Serving

Trusted Server supports two shipped ad-delivery paths: a direct server-side
auction API and publisher-page processing at the edge. Both paths use the same
validated settings and compiled auction plan.

## Request flow

For ordinary publisher requests, the selected adapter fetches the configured
publisher origin. Eligible HTML responses pass through the shared processing
pipeline, which can rewrite first-party URLs, inject Trusted Server JavaScript,
and place configured ad opportunities. Non-HTML responses remain on the
publisher proxy path.

`POST /auction` accepts the documented auction request shape. When
`[auction].enabled = true`, the compiled `AuctionPlan` selects configured
providers, routes bidder codes, executes supported provider fan-out, applies an
optional mediator, and returns the winning bids. When auctions are disabled,
the endpoint returns a no-bid response without contacting a provider.

## Shipped demand and mediation

Provider instances are declared under `[auction.providers.<id>]`. The shipped
profiles are:

- `standard` for a generic OpenRTB 2.6 endpoint;
- `prebid-server` for Prebid Server request controls; and
- `aps` for the APS OpenRTB contract and typed renderer response.

Browser-visible bidder codes are mapped separately under
`[auction.bidders.<id>]`. `adserver_mock` is the only registered mediator; it
is optional and is configured under `[integrations.adserver_mock]`.

```toml
[auction]
enabled = true

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[auction.bidders.example-bidder]
provider = "pbs-main"
```

Deploy validation compiles this configuration before publication. Adapter
startup adds target-specific provider-count and backend-name checks; see
[Auction orchestration](/guide/auction-orchestration) for the complete schema
and adapter limits.

## Creative delivery

Winning OpenRTB markup can be rewritten through the first-party proxy and can
be sanitized when the corresponding auction settings are enabled. The default
is rewriting enabled and sanitization disabled. APS winners use the typed
renderer contract instead of exposing raw `adm`.

Publisher HTML uses the integration registry to contribute head markup and to
select embedded TSJS modules. The registry does not fetch arbitrary integration
assets at runtime.

## Operational checks

- Validate configuration with `ts config validate` before deployment.
- Use the [auction test runbook](/guide/auction-testing) for focused and
  end-to-end checks.
- Confirm provider authorization and test inventory with each upstream before
  enabling traffic.
- Treat provider timeout values as logical budgets; no adapter currently
  promises an abortable provider-wide wall-clock deadline.

See also [Creative processing](/guide/creative-processing) and the
[Integrations overview](/guide/integrations-overview).
