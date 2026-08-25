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
- Parallel execution of APS OpenRTB and Prebid Server
- Optional mock-adserver mediation selecting winning bids
- Final response with winning creatives

**With Auction Execution Disabled** (`auction.enabled = false`):
- Logs showing: `"/auction: auction is disabled; returning no-bid response"`
- Immediate no-bid response with no provider or mediator dispatch

## Configuration

Edit `trusted-server.toml` to customize the auction:

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

## Test Scenarios

### Scenario 1: Parallel + Mediation (Default)
**Config:**
```toml
[auction]
enabled = true
mediator = "adserver_mock"  # Providers come from [auction.providers.*] maps
```

**Expected Flow:**
1. Prebid queries its configured bidders through Prebid Server
2. APS sends an OpenRTB request for eligible banner impressions
3. AdServer Mock mediates the provider responses
4. The winning creative or typed APS renderer is returned

### Scenario 2: Parallel Only (No Mediation)
**Config:**
```toml
[auction]
enabled = true
# Configured [auction.providers.*] run without a mediator
```

**Expected Flow:**
1. Prebid and APS run in parallel
2. Highest bid wins automatically
3. No mediation

### Scenario 3: Auction Disabled

**Config:**

```toml
[auction]
enabled = false
```

**Expected Flow:** no auction provider dispatch.

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
INFO: APS requests bids for 2 impressions
INFO: APS returns 2 accepted bids in 80ms
INFO: GAM mediation: slot 'header-banner' won by 'aps' at $2.50 CPM
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
**Fix:** Make sure an `[auction.providers.<id>]` entry selects `profile = "aps"`

**Issue:** `"No providers configured"`
**Fix:** Make sure map-shaped `[auction.providers.<id>]` entries are configured

**Issue:** Tests fail with WASM errors
**Explanation:** Async tests don't work in WASM test environment. Integration tests via HTTP work fine!

## Next Steps

1. **Verify Prebid Server demand** - Confirm configured bidders return expected test bids
2. **Verify APS eligibility** - Confirm the test account, inventory identity, and `/e/pb/bid` endpoint are authorized
3. **Exercise renderer security** - Run the APS browser integration suite for iframe and script creatives
4. **Add metrics** - Track bid rates, win rates, latency, and aggregate drop reasons per provider

## Provider Behavior

### APS (Amazon)
- Sends real OpenRTB requests for eligible banner slots
- Safely drops malformed, unsupported, or unrenderable bids and reports aggregate reasons
- Reduces multiple APS candidates to one winner per impression
- Returns typed renderer descriptors rather than exposing `adm` outside the sandbox
- Automated tests intercept upstream traffic and use fictional response fixtures

### AdServer Mock
- Acts as mediator by calling mocktioneer's mediation endpoint
- Selects winning bids based on highest CPM
- Response time varies based on mocktioneer instance

### Prebid
- **Real implementation** - makes actual HTTP calls
- Queries configured SSPs
- Returns real bids from real bidders
- Response time: varies (network dependent)
