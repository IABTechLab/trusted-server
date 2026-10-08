# Testing auction orchestration

## Start the local server

Configure at least one reachable demand source in `trusted-server.toml`, then
start the Fastly development server:

```bash
fastly compute serve
```

Demand endpoints must use HTTPS. Fastly and Viceroy also need a backend that
matches the endpoint's host and TLS settings. For a deterministic local bidder,
use `scripts/template-cache-local-test.sh`, which creates a temporary CA and
registers the matching backend.

## Example configuration

```toml
[auction]
enabled = true
timeout_ms = 2000

[demand]
modules = ["pbs_main", "aps_main"]

[demand.pbs_main]
implementation = "auction.prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[demand.aps_main]
implementation = "auction.aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-aps-account"
debug = false

[auction.bidders.example-server]
module = "pbs_main"

[ad-server]
module = "mock"

[ad-server.mock]
endpoint = "https://adserver.example.com/decide"
timeout_ms = 500
```

Replace the example endpoints and account values before running the server.
Leave `[ad-server]` out to test local highest-bid selection with no ad server.

## Send a routed request

The Prebid Server demand source uses explicit routing, so the request must
include params for a bidder listed in `[auction.bidders]`:

```bash
curl -X POST http://localhost:7676/auction \
  -H "Content-Type: application/json" \
  -d '{
    "adUnits": [
      {
        "code": "header-banner",
        "mediaTypes": {
          "banner": {
            "sizes": [[728, 90], [970, 250]]
          }
        },
        "bids": [
          {
            "bidder": "example-server",
            "params": {
              "placement": "example-header-placement"
            }
          }
        ]
      },
      {
        "code": "sidebar",
        "mediaTypes": {
          "banner": {
            "sizes": [[300, 250], [300, 600]]
          }
        }
      }
    ]
  }'
```

The first impression routes to `pbs_main` and `aps_main`. The second routes only
to `aps_main` because APS uses `all_eligible` and Prebid Server uses `explicit`.

## Check current logs

Startup logs report plan-backed construction and the demand source count:

```text
Building plan-backed auction orchestrator
Auction orchestrator built with 2 demand sources
```

A launched request logs the configured demand source name, predicted backend,
and budget. Collection logs the pending and immediate response counts:

```text
Dispatching bid request to 'pbs_main' (backend: ..., budget: ...ms)
Dispatching bid request to 'aps_main' (backend: ..., budget: ...ms)
Dispatched 2 SSP request(s) with 0 immediate response(s) (timeout: ...ms)
```

Exact backend names and budgets depend on the adapter and remaining auction
deadline. A demand source's failures are isolated and appear in response
metadata under its name.

## Disabled auction

Set:

```toml
[auction]
enabled = false
```

`POST /auction` returns an immediate no-bid response, emits an
`auction_disabled` skipped telemetry event, and performs no demand or ad server
work. The request log is:

```text
/auction: auction is disabled; returning no-bid response
```

## Automated checks

Use the repository aliases instead of bare `cargo test --workspace`:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

For browser integration tests:

```bash
cd crates/trusted-server-js/lib
npx vitest run
```

The template-cache harness exercises plan compilation, HTTPS backend naming,
demand dispatch, local highest-bid winner selection, and both ESI and inline
delivery modes:

```bash
./scripts/template-cache-local-test.sh esi
./scripts/template-cache-local-test.sh inline
```
