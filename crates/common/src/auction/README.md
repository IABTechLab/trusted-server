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
│  - request_bids()                                        │
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
│  2. Route Matching (crates/fastly/src/main.rs:84)                    │
│     - Pattern: (Method::POST, "/auction")                            │
│     - Handler: handle_auction(settings, &orchestrator, &storage, req)│
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
│     - Create/retrieve synthetic ID (persistent)                      │
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
│     - Transform AuctionRequest → Provider format (e.g., APS TAM)     │
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
│      - Rewrite creative HTML for first-party proxy                   │
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
- Transforms it to their specific format (e.g., APS TAM, OpenRTB)
- Makes HTTP request to their endpoint
- Parses the response
- Returns `AuctionResponse` with `Bid[]`

For example, APS provider:
```rust
// Transform AuctionRequest → ApsBidRequest
let aps_request = ApsBidRequest {
    pub_id: "5128",
    slots: vec![
        ApsSlot {
            slot_id: "header-banner",
            sizes: vec![[728, 90], [970, 250]],
            slot_name: Some("header-banner"),
        }
    ],
    page_url: Some("https://example.com"),
    ua: Some("Mozilla/5.0..."),
    timeout: Some(800),
};

// HTTP POST to http://localhost:6767/e/dtb/bid
// Parse response → AuctionResponse
```

#### 4. Response Assembly
The orchestrator collects all bids and creates an OpenRTB response:

```json
{
  "id": "auction-response",
  "seatbid": [
    {
      "seat": "amazon-aps",
      "bid": [
        {
          "id": "amazon-aps-header-banner",
          "impid": "header-banner",
          "price": 2.5,
          "adm": "<iframe src=\"/first-party/proxy?tsurl=...\">",
          "w": 728,
          "h": 90,
          "crid": "amazon-aps-creative",
          "adomain": ["amazon.com"]
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

Note that creative HTML is rewritten to use the first-party proxy (`/first-party/proxy`) for privacy and security.

## Route Registration & Endpoints

### Auction-Related Routes

The trusted-server handles several types of routes defined in `crates/fastly/src/main.rs`:

| Route                     | Method | Handler                        | Purpose                                          | Line |
|---------------------------|--------|--------------------------------|--------------------------------------------------|------|
| `/auction`                | POST   | `handle_auction()`             | Main auction endpoint (Prebid.js/tsjs format)    | 84   |
| `/first-party/proxy`      | GET    | `handle_first_party_proxy()`   | Proxy creatives through first-party domain       | 84   |
| `/first-party/click`      | GET    | `handle_first_party_click()`   | Track clicks on ads                              | 85   |
| `/first-party/sign`       | GET/POST | `handle_first_party_proxy_sign()` | Generate signed URLs for creatives            | 86   |
| `/first-party/proxy-rebuild` | POST | `handle_first_party_proxy_rebuild()` | Rebuild creative HTML with new settings     | 89   |
| `/static/tsjs=*`          | GET    | `handle_tsjs_dynamic()`        | Serve tsjs library (Prebid.js alternative)       | 66   |
| `/.well-known/ts.jwks.json` | GET  | `handle_jwks_endpoint()`       | Public key distribution for request signing      | 71   |
| `/verify-signature`       | POST   | `handle_verify_signature()`    | Verify signed requests                           | 74   |
| `/admin/keys/rotate`      | POST   | `handle_rotate_key()`          | Rotate signing keys (admin only)                 | 77   |
| `/admin/keys/deactivate`  | POST   | `handle_deactivate_key()`      | Deactivate signing keys (admin only)             | 78   |
| `/integrations/*`         | *      | Integration Registry           | Provider-specific endpoints (Prebid, etc.)       | 92   |
| `*` (fallback)            | *      | `handle_publisher_request()`   | Proxy to publisher origin                        | 108  |

### How Routing Works

#### 1. Main Router (main.rs)
The Fastly Compute entrypoint uses pattern matching on `(Method, path)` tuples:

```rust
let result = match (method, path.as_str()) {
    // Auction endpoint
    (Method::POST, "/auction") => handle_auction(&settings, req).await,
    
    // First-party endpoints
    (Method::GET, "/first-party/proxy") => handle_first_party_proxy(&settings, req).await,
    
    // Integration registry (dynamic routes)
    (m, path) if integration_registry.has_route(&m, path) => {
        integration_registry.handle_proxy(&m, path, &settings, req).await
    },
    
    // Fallback to publisher origin
    _ => handle_publisher_request(&settings, &integration_registry, req),
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
2. **Admin routes** (`/admin/*`)
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
- Creative HTML is rewritten to use `/first-party/proxy` URLs
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

### 1. Parallel + Mediation (Recommended)
**Use case:** Header bidding with ad server mediation

```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
mediator = "adserver_mock"  # Setting mediator enables parallel mediation strategy
timeout_ms = 2000
```

**Flow:**
1. Prebid and APS run in parallel
2. Both return their bids simultaneously
3. Bids are sent to the mediator for final decision
4. Mediator competes house inventory and returns winning creative

### 2. Parallel Only
**Use case:** Client-side auction, no mediation

```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
# No mediator = parallel only strategy (highest CPM wins)
timeout_ms = 2000
```

**Flow:**
1. All providers run in parallel
2. Highest bid wins
3. No mediation server involved

## Configuration

### Configuration

All auction settings are configured directly under `[auction]`:

```toml
[auction]
enabled = true                      # Enable/disable auction orchestration
providers = ["prebid", "aps"]        # List of bidder providers
mediator = "adserver_mock"          # Optional: if set, uses mediation; if omitted, highest bid wins
timeout_ms = 2000                   # Overall auction timeout
```

**Strategy Auto-Detection:**
- When `mediator` is configured → Runs **parallel mediation** (providers in parallel, mediator decides winner)
- When `mediator` is omitted → Runs **parallel only** (providers in parallel, highest CPM wins)

### Provider Configuration

Each provider has its own configuration section:

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000

[integrations.aps]
enabled = true
mock = true  # Set to false for real integration
timeout_ms = 800

[integrations.adserver_mock]
enabled = true
endpoint = "http://localhost:6767/adserver/mediate"
timeout_ms = 500
```

## Adding a New Provider

1. Create a new file in `src/auction/providers/your_provider.rs`

```rust
use async_trait::async_trait;
use crate::auction::provider::AuctionProvider;
use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse};

pub struct YourAuctionProvider {
    config: YourConfig,
}

#[async_trait(?Send)]
impl AuctionProvider for YourAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "your_provider"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        // 1. Transform AuctionRequest to your provider's format
        // 2. Make HTTP request to your provider
        // 3. Parse response
        // 4. Return AuctionResponse with bids
        todo!()
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}
```

2. Register the provider in `src/auction/providers/mod.rs`

3. Configure it in `trusted-server.toml`

## Testing

### Mock Providers

APS and adserver_mock providers are used for testing the orchestration pattern:

- **APS Mock**: Returns synthetic bids with Amazon branding
- **AdServer Mock**: Acts as mediator by calling mocktioneer's mediation endpoint, selects winning bids based on highest CPM

Set `mock = false` in APS config when real APS integration is ready.

### Example Test Flow

```rust
let orchestrator = AuctionOrchestrator::new(config);
orchestrator.register_provider(Arc::new(PrebidAuctionProvider::new(prebid_config)));
orchestrator.register_provider(Arc::new(ApsAuctionProvider::new(aps_config)));

let result = orchestrator.run_auction(&request, &context).await?;

// Check results
assert_eq!(result.winning_bids.len(), 2);
assert!(result.total_time_ms < 2000);
```

## Performance Considerations

- **Parallel Execution**: Currently runs sequentially in Fastly Compute (no tokio runtime), but structured for easy parallelization
- **Timeouts**: Each provider has independent timeout; global timeout enforced at flow level
- **Error Handling**: Provider failures don't fail entire auction; partial results returned

## Related Files

- `src/auction/mod.rs` - Module exports
- `src/auction/types.rs` - Core auction types
- `src/auction/provider.rs` - Provider trait definition
- `src/auction/orchestrator.rs` - Orchestration logic
- `src/auction/config.rs` - Configuration types
- `src/auction/providers/` - Provider implementations

## Questions?

See the main project [README](../../../../README.md) or [integration guide](../../../../docs/guide/integration-guide.md).
