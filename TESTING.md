# Testing the Auction Orchestration System

## Quick Test Summary

The auction orchestration system has been integrated into the existing Prebid endpoints. You can test it right away using the Fastly local server!

## How to Test

### 1. Start the Local Server

```bash
fastly compute serve
```

### 2. Test with Existing Endpoint

The `/auction` endpoint now uses the orchestrator when `auction.enabled = true` in config.

**Test Request:**
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
        }
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

### 3. What You'll See

**With Orchestrator Enabled** (`auction.enabled = true`):
- Logs showing: `"Using auction orchestrator"`
- Parallel execution of APS (mocked) and Prebid (real)
- GAM mediation (mocked) selecting winning bids
- Final response with winning creatives

**With Orchestrator Disabled** (`auction.enabled = false`):
- Logs showing: `"Using legacy Prebid flow"`
- Direct Prebid Server call (backward compatible)

##Configuration

Edit `trusted-server.toml` to customize the auction:

```toml
# Enable/disable orchestrator
[auction]
enabled = true
providers = ["prebid", "aps"]
mediator = "adserver_mock"  # If set: mediation, if omitted: highest bid wins
timeout_ms = 2000

# Mock provider configs
[integrations.aps]
enabled = true
mock = true
mock_price = 2.50

[integrations.adserver_mock]
enabled = true
endpoint = "http://localhost:6767/adserver/mediate"
timeout_ms = 500
```

## Test Scenarios

### Scenario 1: Parallel + Mediation (Default)
**Config:**
```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
mediator = "adserver_mock"  # Mediator configured = parallel mediation strategy
```

**Expected Flow:**
1. Prebid queries real SSPs
2. APS returns mock bids ($2.50 CPM)
3. AdServer Mock mediates between all bids
4. Winning creative returned

### Scenario 2: Parallel Only (No Mediation)
**Config:**
```toml
[auction]
enabled = true
providers = ["prebid", "aps"]
# No mediator = parallel only strategy
```

**Expected Flow:**
1. Prebid and APS run in parallel
2. Highest bid wins automatically
3. No mediation

### Scenario 3: Legacy Mode (Backward Compatible)
**Config:**
```toml
[auction]
enabled = false
```

**Expected Flow:**
- Original Prebid-only behavior
- No orchestration overhead

## Debugging

### Check Logs
The orchestrator logs extensively:
```
INFO: Using auction orchestrator
INFO: Running auction with strategy: parallel_mediation
INFO: Running 2 bidders in parallel
INFO: Requesting bids from: prebid
INFO: Prebid returned 2 bids (time: 120ms)
INFO: Requesting bids from: aps
INFO: APS (MOCK): returning 2 bids in 80ms
INFO: GAM mediation: slot 'header-banner' won by 'amazon-aps' at $2.50 CPM
```

### Verify Provider Registration
Look for these log messages on startup:
```
INFO: Registering auction provider: prebid
INFO: Registering auction provider: aps
INFO: Registering auction provider: adserver_mock
```

### Common Issues

**Issue:** `"Provider 'aps' not registered"`
**Fix:** Make sure `[integrations.aps]` is configured in `trusted-server.toml`

**Issue:** `"No providers configured"`
**Fix:** Make sure `providers = ["prebid", "aps"]` is set in `[auction]`

**Issue:** Tests fail with WASM errors
**Explanation:** Async tests don't work in WASM test environment. Integration tests via HTTP work fine!

## Next Steps

1. **Test with real Prebid Server** - Verify Prebid bids work correctly
2. **Implement real APS** - Replace mock with actual Amazon TAM API calls
3. **Implement real GAM** - Add Google Ad Manager API integration
4. **Add metrics** - Track bid rates, win rates, latency per provider

## Mock Provider Behavior

### APS (Amazon)
- Returns bids for all slots
- Default mock price: $2.50 CPM
- Always returns 2 bids
- Response time: ~80ms (simulated)

### AdServer Mock
- Acts as mediator by calling mocktioneer's mediation endpoint
- Selects winning bids based on highest CPM
- Response time varies based on mocktioneer instance

### Prebid
- **Real implementation** - makes actual HTTP calls
- Queries configured SSPs
- Returns real bids from real bidders
- Response time: varies (network dependent)
