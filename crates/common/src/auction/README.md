# Auction Orchestration System

A flexible, extensible framework for managing multi-provider header bidding auctions with support for parallel execution and mediation.

## Overview

The auction orchestration system allows you to:
- Run multiple auction providers (Prebid, Amazon APS, Google GAM, etc.) in parallel or sequentially
- Implement mediation strategies where a primary ad server (like GAM) makes the final decision
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
  │  Prebid  │      │ Amazon   │     │  Google  │
  │ Provider │      │   APS    │     │   GAM    │
  └──────────┘      └──────────┘     └──────────┘
```

## Key Concepts

### Auction Provider
Implements the `AuctionProvider` trait to integrate with a specific SSP/ad exchange.

### Auction Flow
A named configuration that defines:
- Which providers participate
- Execution strategy (parallel, waterfall, etc.)
- Timeout settings
- Optional mediator

### Orchestrator
Manages the execution of an auction flow, coordinates providers, and collects results.

## Auction Strategies

### 1. Parallel + Mediation (Recommended)
**Use case:** Header bidding with GAM as primary ad server

```toml
[auction]
enabled = true
strategy = "parallel_mediation"
bidders = ["prebid", "aps"]
mediator = "gam"
timeout_ms = 2000
```

**Flow:**
1. Prebid and APS run in parallel
2. Both return their bids simultaneously
3. Bids are sent to GAM for final mediation
4. GAM competes its own inventory and returns winning creative

### 2. Parallel Only
**Use case:** Client-side auction, no mediation

```toml
[auction]
enabled = true
strategy = "parallel_only"
bidders = ["prebid", "aps"]
timeout_ms = 2000
```

**Flow:**
1. All bidders run in parallel
2. Highest bid wins
3. No mediation server involved

### 3. Waterfall
**Use case:** Sequential fallback when parallel isn't needed

```toml
[auction]
enabled = true
strategy = "waterfall"
bidders = ["prebid", "aps"]
timeout_ms = 2000
```

**Flow:**
1. Try Prebid first
2. If Prebid returns no bids, try APS
3. Return first successful bid

## Configuration

### Configuration

All auction settings are configured directly under `[auction]`:

```toml
[auction]
enabled = true                      # Enable/disable auction orchestration
strategy = "parallel_mediation"     # Auction strategy
bidders = ["prebid", "aps"]        # List of bidder providers
mediator = "gam"                    # Optional mediator (only for parallel_mediation)
timeout_ms = 2000                   # Overall auction timeout
```

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

[integrations.gam]
enabled = true
mock = true
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

APS and GAM providers currently run in mock mode for testing the orchestration pattern:

- **APS Mock**: Returns synthetic bids with Amazon branding
- **GAM Mock**: Acts as mediator, optionally injects house ads, simulates mediation logic

Set `mock = false` when real implementations are ready.

### Example Test Flow

```rust
let orchestrator = AuctionOrchestrator::new(config);
orchestrator.register_provider(Arc::new(PrebidAuctionProvider::new(prebid_config)));
orchestrator.register_provider(Arc::new(ApsAuctionProvider::new(aps_config)));
orchestrator.register_provider(Arc::new(GamAuctionProvider::new(gam_config)));

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

See the main project [README](../../../../README.md) or [integration guide](../../../../docs/integration_guide.md).
