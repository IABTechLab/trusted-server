# Ad Server Mock Mediator

`adserver_mock` is a development mediator for the auction orchestrator. It
is not an ordinary bidder provider and does not expose an integration proxy
route.

Enable `[integrations.adserver_mock]` and set
`auction.mediator = "adserver_mock"`. Startup registers the mediator only
when both conditions hold. The orchestrator first gathers responses from the
configured auction providers, then passes successful bids to the mediator's
HTTP endpoint for final selection.

The mediator supports banner bids. It omits bids without a decoded numeric
price, applies the optional CPM floor, and restores render and accounting
fields from the original provider bids after the mediation response. Duplicate
provider/slot/bidder identities are last-write-wins and generate a warning
because accounting restoration may become ambiguous.

`context_query_params` maps auction-context keys to endpoint query
parameters. List values become comma-separated strings and URL construction
percent-encodes names and values. Use this only for explicitly reviewed context
fields; it is not a generic request-forwarding mechanism.

The default endpoint is a loopback Mocktioneer address and the integration is
disabled by default. Configure an explicit endpoint for shared environments.
See [Auction Orchestration](/guide/auction-orchestration) and the generated
[Configuration Reference](/guide/configuration#ad-server-mock-integration).
