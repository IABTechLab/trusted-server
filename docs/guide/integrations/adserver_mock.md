# Ad Server Mock

`adserver_mock` is a development ad server for the auction orchestrator. It
is not a demand source and does not expose an integration proxy route.

Select it with `[ad-server] module = "adserver_mock"` and give it an
`[ad-server.adserver_mock]` table. The orchestrator first gathers responses from
the configured demand sources, then passes successful bids to the ad
server's HTTP endpoint for final selection.

The ad server mock supports banner bids. It omits bids without a decoded
numeric price, applies the optional CPM floor, and restores render and
accounting fields from the original demand bids after the ad server response.
Duplicate demand source/slot/bidder identities are last-write-wins and generate
a warning because accounting restoration may become ambiguous.

`context_query_params` maps auction-context keys to endpoint query
parameters. List values become comma-separated strings and URL construction
percent-encodes names and values. Use this only for explicitly reviewed context
fields; it is not a generic request-forwarding mechanism.

The default endpoint is a loopback Mocktioneer address, and no ad server runs
until `[ad-server] module` names one. Configure an explicit endpoint for
shared environments. See [Auction Orchestration](/guide/auction-orchestration)
and the [Ad server](/guide/configuration#ad-server) section of the
configuration reference.
