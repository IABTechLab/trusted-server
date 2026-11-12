# Testing auction orchestration

## Start the local server

Configure at least one reachable provider in `trusted-server.toml`, then start the
Fastly development server:

```bash
fastly compute serve
```

Provider endpoints must use HTTPS, except plain HTTP to `127.0.0.1` or `[::1]`,
which local test bidders use. EdgeZero registers each provider's Fastly backend
at request time, so Viceroy needs no static backend for it. For a deterministic
local bidder, use `scripts/template-cache-local-test.sh`, which serves a stub
bidder over plain HTTP on `127.0.0.1`.

## Example configuration

```toml
[auction]
enabled = true
timeout_ms = 2000
mediator = "adserver_mock"

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
routing = "explicit"

[auction.providers.aps-main]
protocol = "openrtb-2.6"
profile = "aps"
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
profile_config = { account_id = "example-aps-account", debug = false }

[auction.bidders.example-server]
provider = "pbs-main"

[integrations.adserver_mock]
enabled = true
endpoint = "https://mediator.example.com/mediate"
timeout_ms = 500
```

Replace the example endpoints and profile values before running the server.
Omit `mediator` to test local highest-bid selection without mediation.

## Send a routed request

The PBS provider uses explicit routing, so the request must include params for a
bidder listed in `[auction.bidders]`:

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

The first impression routes to `pbs-main` and `aps-main`. The second routes only
to `aps-main` because APS uses `all_eligible` and PBS uses `explicit`.

## Check current logs

Startup logs report plan-backed construction and the provider count:

```text
Building plan-backed auction orchestrator
Auction orchestrator built with 2 bidder providers
```

All bidder requests leave together as one batch. Collection logs how many are
in flight and the remaining auction budget, then the winner count:

```text
Collecting 2 in-flight SSP responses (...ms of the auction budget remaining)
Selected ... winning bids
```

The remaining budget depends on the adapter and how long the requests took to
build. Provider failures are isolated and appear in response metadata under the
configured provider ID.

## Disabled auction

Set:

```toml
[auction]
enabled = false
```

`POST /auction` returns an immediate no-bid response, emits an
`auction_disabled` skipped telemetry event, and performs no provider or mediator
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

The template-cache harness exercises plan compilation, a plain HTTP loopback
bidder, provider dispatch, local highest-bid winner selection, and both ESI and
inline delivery modes:

```bash
./scripts/template-cache-local-test.sh esi
./scripts/template-cache-local-test.sh inline
```
