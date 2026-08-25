# Auction Orchestration System

A flexible, extensible framework for managing multi-provider header bidding auctions with support for parallel execution and mediation.

## Overview

The auction orchestration system allows you to:
- Run multiple auction providers (Prebid, Amazon APS, etc.) in parallel or sequentially
- Implement mediation strategies where a primary ad server makes the final decision
- Configure different auction flows for different scenarios
- Easily add new auction providers

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                  Auction Orchestrator                   │
│  - Manages auction workflow & sequencing                │
│  - Combines bids from multiple sources                  │
│  - Applies business logic                               │
└─────────────────────────────────────────────────────────┘
                          │
                          │ uses
                          ▼
┌─────────────────────────────────────────────────────────┐
│              AuctionProvider Trait                       │
│  - request_bids() async                                  │
│  - parse_response()                                      │
│  - provider_name()                                       │
│  - timeout_ms()                                          │
│  - is_enabled()                                          │
└─────────────────────────────────────────────────────────┘
                          │
        ┌─────────────────┼─────────────────┐
        │                 │                 │
        ▼                 ▼                 ▼
  ┌──────────┐      ┌──────────┐     ┌──────────┐
  │  Prebid  │      │ Amazon   │     │ AdServer │
  │ Provider │      │   APS    │     │   Mock   │
  └──────────┘      └──────────┘     └──────────┘
```

## Request Flow

When a request arrives at the `/auction` endpoint, it goes through the following steps:

```
┌──────────────────────────────────────────────────────────────────────┐
│  1. HTTP POST /auction                                               │
│     - Body: AdRequest (Prebid.js/tsjs format)                        │
│     - Headers: User-Agent, cookies, etc.                             │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  2. Route Matching (crates/trusted-server-adapter-fastly/src/main.rs)│
│     - Pattern: (Method::POST, "/auction")                            │
│     - Handler: handle_auction(settings, &orchestrator,               │
│       &runtime_services, req)                                        │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  3. Parse Request Body (mod.rs:149)                                  │
│     - Deserialize JSON → AdRequest struct                            │
│     - Extract ad units with media types                              │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  4. Generate User IDs (mod.rs:206-214)                               │
│     - Create/retrieve EC ID (persistent)                             │
│     - Generate fresh ID (per-request)                                │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  5. Transform Request Format (mod.rs:216-240)                        │
│     - AdRequest → AuctionRequest                                     │
│     - AdUnit.code → AdSlot.id                                        │
│     - mediaTypes.banner.sizes → AdFormat[]                           │
│     - Build PublisherInfo, UserInfo, DeviceInfo                      │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  6. Use Provided Orchestrator (mod.rs:150)                           │
│     - Reused across requests from startup construction               │
│     - Contains all registered providers (APS, Prebid, etc.)          │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  7. Create Auction Context (mod.rs:172-176)                          │
│     - Attach settings                                                │
│     - Attach original request                                        │
│     - Set timeout from config                                        │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  8. Run Auction Strategy (orchestrator.rs:42)                        │
│     ┌────────────────────────────────────────────────────────────┐   │
│     │  Strategy: parallel_only                                   │   │
│     │  1. Launch all bidders concurrently                        │   │
│     │  2. Wait for all responses                                 │   │
│     │  3. Select highest bid per slot                            │   │
│     └────────────────────────────────────────────────────────────┘   │
│     ┌────────────────────────────────────────────────────────────┐   │
│     │  Strategy: parallel_mediation                              │   │
│     │  1. Launch all bidders concurrently                        │   │
│     │  2. Collect all bids                                       │   │
│     │  3. Send to mediator for final decision                    │   │
│     └────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  9. Each Provider Processes Request                                  │
│     - Transform AuctionRequest → Provider OpenRTB request            │
│     - Send HTTP request to provider endpoint                         │
│     - Parse provider response                                        │
│     - Transform → AuctionResponse with Bid[]                         │
│     - Return to orchestrator                                         │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  10. Select Winning Bids (orchestrator.rs:363-385)                   │
│      - For each slot, find highest CPM bid                           │
│      - Create HashMap<slot_id, Bid>                                  │
│      - Log winning selections                                        │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  11. Transform to OpenRTB Response (mod.rs:274-322)                  │
│      - Build seatbid array (one per winning bid)                     │
│      - Sanitize creative HTML when enabled (opt-in)                  │
│      - Rewrite creative HTML when enabled (default)                  │
│      - Add orchestrator metadata (timing, strategy, bid count)       │
└──────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────────┐
│  12. Return HTTP Response                                            │
│      - Status: 200 OK                                                │
│      - Content-Type: application/json                                │
│      - Body: OpenRTB BidResponse                                     │
└──────────────────────────────────────────────────────────────────────┘
```

### Step-by-Step Breakdown

#### 1. Request Arrival
Client (browser, Prebid.js, tsjs) sends a POST request to `/auction` with ad unit definitions:

```json
{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": {
        "banner": {
          "sizes": [[728, 90], [970, 250]]
        }
      }
    }
  ]
}
```

#### 2. Format Transformation
The system transforms the Prebid.js format into an internal `AuctionRequest`:

```rust
// From: AdUnit with sizes [[728, 90], [970, 250]]
// To:   AdSlot with formats
AdSlot {
    id: "header-banner",
    formats: vec![
        AdFormat { width: 728, height: 90, media_type: Banner },
        AdFormat { width: 970, height: 250, media_type: Banner },
    ],
    floor_price: None,
    targeting: HashMap::new(),
}
```

#### 3. Provider Execution
Each registered provider (APS, Prebid, etc.) receives the `AuctionRequest` and:
- Transforms it to the provider's OpenRTB request format
- Makes HTTP request to their endpoint
- Parses the response
- Returns `AuctionResponse` with `Bid[]`

For example, APS provider:
```rust
// Transform AuctionRequest → APS OpenRTB request
// - ext.account = configured account_id
// - ext.sdk = { source: "prebid", version: "2.2.0" }
// - banner slots become secure impressions with matching formats/floors
// - existing consent, identity, device, and geo privacy gates apply

// HTTP POST to https://aps.example.com/e/pb/bid
// Parse decoded-price response → AuctionResponse with a typed renderer
```

#### 4. Response Assembly
The orchestrator collects all bids and creates an OpenRTB response:

```json
{
  "id": "auction-response",
  "seatbid": [
    {
      "seat": "aps",
      "bid": [
        {
          "id": "fictional-selected-bid-id",
          "impid": "header-banner",
          "price": 2.5,
          "w": 728,
          "h": 90,
          "ext": {
            "trusted_server": {
              "renderer": {
                "type": "aps",
                "version": 1,
                "accountId": "example-account",
                "bidId": "fictional-selected-bid-id",
                "tagType": "iframe",
                "creativeUrl": "https://creative.example/render",
                "aaxResponse": "<base64 minimized one-bid envelope>",
                "width": 728,
                "height": 90
              }
            }
          }
        }
      ]
    }
  ],
  "ext": {
    "orchestrator": {
      "strategy": "parallel_only",
      "bidders": 1,
      "total_bids": 1,
      "time_ms": 5
    }
  }
}
```

With `[auction].sanitize_creatives = true` (opt-in, default `false`),
executable markup is stripped with its inner content before delivery. With
`[auction].rewrite_creatives = true` (the default), each auction delivery path
rewrites eligible URLs through the first-party proxy (`/first-party/proxy`) and
removes bidder `<base>` elements. The `POST /auction` response also injects the
creative runtime; the publisher SSAT inline path uses absolute first-party URLs
without injecting that bundle. With both disabled, the creative ships exactly
as the bidder returned it. In every mode, creatives over the 1 MiB cap are
rejected.

## Route Registration & Endpoints

### Auction-Related Routes

The trusted-server handles several types of routes defined in `crates/trusted-server-adapter-fastly/src/main.rs`:

| Route                     | Method | Handler                        | Purpose                                          | Line |
|---------------------------|--------|--------------------------------|--------------------------------------------------|------|
| `/auction`                | POST   | `handle_auction()`             | Main auction endpoint (Prebid.js/tsjs format)    | 84   |
| `/first-party/proxy`      | GET    | `handle_first_party_proxy()`   | Proxy creatives through first-party domain       | 84   |
| `/first-party/click`      | GET    | `handle_first_party_click()`   | Track clicks on ads                              | 85   |
| `/first-party/sign`       | GET/POST | `handle_first_party_proxy_sign()` | Generate signed URLs for creatives            | 86   |
| `/first-party/proxy-rebuild` | GET/POST | `handle_first_party_proxy_rebuild()` | Re-sign mutated click URLs (GET 302s for the opaque-origin click guard) | 89   |
| `/static/tsjs=*`          | GET    | `handle_tsjs_dynamic()`        | Serve tsjs library (Prebid.js alternative)       | 66   |
| `/.well-known/ts.jwks.json` | GET  | `handle_jwks_endpoint()`       | Public key distribution for request signing      | 71   |
| `/verify-signature`       | POST   | `handle_verify_signature()`    | Verify signed requests                           | 74   |
| `/_ts/admin/keys/rotate`      | POST   | `handle_rotate_key()`          | Rotate signing keys (admin only)                 | 77   |
| `/_ts/admin/keys/deactivate`  | POST   | `handle_deactivate_key()`      | Deactivate signing keys (admin only)             | 78   |
| `/integrations/*`         | *      | Integration Registry           | Provider-specific endpoints (Prebid, etc.)       | 92   |
| `*` (fallback)            | *      | `handle_publisher_request()`   | Proxy to publisher origin                        | 108  |

### How Routing Works

#### 1. Main Router (main.rs)
The Fastly Compute entrypoint uses pattern matching on `(Method, path)` tuples:

```rust
let result = match (method, path.as_str()) {
    // Auction endpoint
    (Method::POST, "/auction") => {
        handle_auction(&settings, &orchestrator, &runtime_services, req).await
    },
    
    // First-party endpoints
    (Method::GET, "/first-party/proxy") => handle_first_party_proxy(&settings, req).await,
    
    // Integration registry (dynamic routes)
    (m, path) if integration_registry.has_route(&m, path) => {
        integration_registry.handle_proxy(&m, path, &settings, req).await
    },
    
    // Fallback to publisher origin
    _ => handle_publisher_request(&settings, &integration_registry, &runtime_services, req),
}
```

#### 2. Integration Registry (Dynamic Routes)
Some integrations register their own routes dynamically. For example, Prebid registers `/integrations/prebid/auction`:

```rust
// In integrations/prebid.rs
impl Integration for PrebidIntegration {
    fn routes(&self) -> Vec<IntegrationRoute> {
        vec![
            IntegrationRoute {
                path: "/integrations/prebid/auction",
                method: Method::POST,
                handler: handle_prebid_auction,
            }
        ]
    }
}
```

The integration registry checks if a route matches any registered integration routes before falling back to the publisher origin.

#### 3. Route Priority
Routes are matched in this order:
1. **Exact top-level routes** (`/auction`, `/first-party/proxy`, etc.)
2. **Admin routes** (`/_ts/admin/*`)
3. **Integration routes** (`/integrations/*`)
4. **Fallback to publisher origin** (all other paths)

This ensures auction and first-party endpoints take precedence over publisher content.

### Auction Endpoint Deep Dive

The `/auction` endpoint is the primary entry point for auctions:

**Input Format (Prebid.js compatible):**
```json
{
  "adUnits": [
    {
      "code": "div-id",
      "mediaTypes": {
        "banner": {
          "sizes": [[300, 250], [728, 90]]
        }
      }
    }
  ],
  "config": { /* optional Prebid.js config */ }
}
```

**Output Format (OpenRTB 2.x):**
```json
{
  "id": "auction-response",
  "seatbid": [
    {
      "seat": "bidder-name",
      "bid": [
        {
          "id": "bid-id",
          "impid": "div-id",
          "price": 2.5,
          "adm": "<creative-html>",
          "w": 300,
          "h": 250
        }
      ]
    }
  ],
  "ext": {
    "orchestrator": {
      "strategy": "parallel_only",
      "bidders": 2,
      "total_bids": 3,
      "time_ms": 150
    }
  }
}
```

**Key Transformations:**
- `adUnits[].code` → `seatbid[].bid[].impid` (slot identifier)
- `mediaTypes.banner.sizes` → evaluated by providers, winning size in `bid.w` and `bid.h`
- Creative HTML: `[auction].sanitize_creatives = true` (opt-in) strips executable markup; `[auction].rewrite_creatives = true` (default) rewrites eligible URLs to `/first-party/proxy` in both delivery paths (with creative runtime injection on `POST /auction` only); with both disabled the creative ships as the bidder returned it
- Multiple bids per slot become separate `seatbid` entries
- Orchestrator metadata added in `ext.orchestrator`

## Key Concepts

### Auction Provider
Implements the `AuctionProvider` trait to integrate with a specific SSP/ad exchange.

### Auction Flow
A named configuration that defines:
- Which providers participate
- Execution strategy (parallel mediation or parallel only)
- Timeout settings
- Optional mediator

### Orchestrator
Manages the execution of an auction flow, coordinates providers, and collects results.

## Auction Strategies

### 1. Parallel + Mediation

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
profile_config = { account_id = "example-aps-account" }
```

Providers run in parallel, then the separately registered mediator chooses from
decoded-price bids.

### 2. Parallel Only

Omit `mediator` from the same map-shaped configuration. The orchestrator selects
the highest decoded CPM per slot and applies floors locally.

## Configuration

`[auction.providers.<provider-id>]` is the only bidder-provider inventory.
`[auction.bidders.<bidder-id>]` maps a client-visible bidder to exactly one
provider. The mediator is selected separately by `[auction].mediator`.

```toml
[auction]
enabled = true
timeout_ms = 2000

[auction.providers.pbs-main]
protocol = "openrtb-2.6"
profile = "prebid-server"
endpoint = "https://prebid.example.com/openrtb2/auction"
timeout_ms = 900
routing = "explicit"

[auction.providers.pbs-main.profile_config]
debug = false
test_mode = false
consent_forwarding = "both"

[auction.providers.pbs-main.notifications]
suppress_all = false
suppress_seats = ["example-seat"]

[auction.bidders.example-server]
provider = "pbs-main"
```

Provider IDs own backend correlation and response identity. The configured
profile supplies typed OpenRTB behavior. Common endpoint, timeout, routing, and
notification policy do not belong to browser integration configuration.

## Adding a Provider

A standards-compatible OpenRTB 2.6 endpoint does not require a Rust provider
implementation. Add an `[auction.providers.<id>]` table, select the `standard`
profile, and route bidder codes through `[auction.bidders.<code>]`. Endpoint,
timeout, routing, and notification behavior are compiled into the shared
`AuctionPlan` at startup.

Add Rust code only when an endpoint needs behavior that the existing
`standard`, `prebid-server`, or `aps` profiles cannot express. New profile work
belongs in `profile.rs` and `openrtb.rs`: define and validate typed profile
configuration, register the profile with the central profile registry, and add
request/response golden tests. Production provider registration is plan-backed;
`AuctionOrchestrator::register_provider` exists only in the legacy test parity
harness and is not an application extension API.

See the maintained [auction orchestration guide](../../../../docs/guide/auction-orchestration.md)
and [integration guide](../../../../docs/guide/integration-guide.md) for complete
configuration and validation examples.

## Testing

Compile test settings with `compile_auction_plan`, construct the orchestrator and
integration registry from the same `Arc<AuctionPlan>`, and exercise requests
through the normal adapter or auction endpoint. Profile tests should cover typed
configuration validation, exact OpenRTB request output, response admission,
provider-local failures, routing, and target capability validation. Legacy
provider constructors and manual registration are retained only for parity tests.

## Performance Considerations

- **Parallel Execution**: Providers are launched concurrently via `select()` over `PendingRequest`s; responses are processed as they become ready within the auction deadline
- **Timeouts**: Each provider has independent timeout; global timeout enforced at flow level
- **Error Handling**: Provider failures don't fail the entire auction; partial results are returned

## Related Files

- `src/auction/mod.rs` - Plan compilation and module exports
- `src/auction/plan.rs` - Typed provider plan and target validation
- `src/auction/profile.rs` - Typed OpenRTB profile registry
- `src/auction/routing.rs` - Central bidder-to-provider routing
- `src/auction/openrtb.rs` - Shared request construction and response parsing
- `src/auction/provider.rs` - Plan-backed provider execution
- `src/auction/orchestrator.rs` - Fan-out, deadline, and mediation flow
- `src/auction/types.rs` - Core auction types

## Questions?

See the main project [README](../../../../README.md) or [integration guide](../../../../docs/guide/integration-guide.md).
