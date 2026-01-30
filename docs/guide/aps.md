# Amazon Publisher Services (APS) Integration

Server-side bidding integration for Amazon's Transparent Ad Marketplace (TAM).

## Overview

The APS integration enables publishers to request bids from Amazon's demand sources server-side, providing:

- **Privacy-first bidding**: No client-side ID tracking or third-party cookies required
- **Reduced latency**: Server-side auction reduces page load impact
- **Unified auction**: Integrates seamlessly with other bidders (Prebid, GAM)
- **Performance**: Async bidding with configurable timeouts

## Quick Start

Get started with server-side APS bidding in 5 minutes.

### Prerequisites

- Fastly Compute service configured
- APS publisher account with valid `pub_id`
- Rust toolchain installed (see main README)

### Step 1: Configure Fastly Backend

Add APS backend to your Fastly service:

**Option A: Via Fastly UI**
1. Go to your service → Origins
2. Click "Create a Backend"
3. Configure:
   - Name: `aax.amazon-adsystem.com`
   - Address: `aax.amazon-adsystem.com`
   - Port: `443`
   - Enable TLS: ✓

**Option B: Via `fastly.toml`**

Add to your `fastly.toml`:

```toml
[[backends]]
name = "aax.amazon-adsystem.com"
address = "aax.amazon-adsystem.com"
port = 443
use_ssl = true
ssl_cert_hostname = "aax.amazon-adsystem.com"
ssl_sni_hostname = "aax.amazon-adsystem.com"
```

### Step 2: Enable APS in Configuration

Edit `trusted-server.toml`:

```toml
# Enable APS bidding
[integrations.aps]
enabled = true
pub_id = "5128"  # Replace with your APS publisher ID
endpoint = "https://aax.amazon-adsystem.com/e/dtb/bid"
timeout_ms = 800

# Configure auction to use APS
[auction]
enabled = true
strategy = "parallel_only"  # Run APS alongside other bidders
bidders = ["aps"]  # Add other bidders like ["aps", "prebid"]
timeout_ms = 2000
```

### Step 3: Deploy or Test Locally

**Deploy to Fastly:**
```bash
cargo build --release --target wasm32-wasip1
fastly compute publish
```

**Test Locally:**
```bash
fastly compute serve
```

### Step 4: Verify It's Working

Check the logs for APS activity:

```bash
# You should see:
INFO APS: requesting bids for 2 slots (pub_id: 5128)
DEBUG APS: sending bid request: {...}
DEBUG APS: received response: {...}
INFO APS returned 2 bids in 150ms
```

## Architecture

```
Publisher Page Request
        ↓
Auction Orchestrator
        ↓
APS Provider (async)
        ↓
https://aax.amazon-adsystem.com/e/dtb/bid
        ↓
Parse APS Response → Unified Bid Format
        ↓
Auction Winner Selection
```

## Configuration

### Basic Setup

Add to `trusted-server.toml`:

```toml
[integrations.aps]
enabled = true
pub_id = "5128"  # Your APS publisher ID
endpoint = "https://aax.amazon-adsystem.com/e/dtb/bid"
timeout_ms = 800
```

### Environment Variables

Override settings via environment variables:

```bash
TRUSTED_SERVER__INTEGRATIONS__APS__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__APS__PUB_ID=5128
TRUSTED_SERVER__INTEGRATIONS__APS__TIMEOUT_MS=800
```

## Auction Strategies

### APS Only

```toml
[auction]
enabled = true
bidders = ["aps"]
# No mediator = parallel only (highest CPM wins)
```

### APS + Prebid (Parallel)

Best for maximum revenue:

```toml
[auction]
enabled = true
providers = ["aps", "prebid"]
timeout_ms = 2000
# No mediator = all providers compete, highest CPM wins

[integrations.aps]
enabled = true
pub_id = "5128"
timeout_ms = 800

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
```

**Benefits:**
- Maximum fill rate
- Best CPM selection
- All bidders compete equally

### APS + Prebid + Mediation

Let a mediator decide winners:

```toml
[auction]
enabled = true
providers = ["aps", "prebid"]
mediator = "adserver_mock"  # Enables parallel mediation (mediator decides winner)
timeout_ms = 2000
```

## Request Format

The integration transforms unified `AuctionRequest` to APS TAM format:

```json
{
  "pubId": "5128",
  "slots": [
    {
      "slotID": "header-banner",
      "slotName": "header-banner",
      "sizes": [[728, 90], [970, 250]]
    }
  ],
  "pageUrl": "https://example.com/article",
  "ua": "Mozilla/5.0...",
  "timeout": 800
}
```

## Response Format

APS returns bids in this format:

```json
{
  "bids": [
    {
      "slotID": "header-banner",
      "price": 2.50,
      "adm": "<div>Creative HTML</div>",
      "w": 728,
      "h": 90,
      "adomain": ["amazon.com"],
      "bidId": "bid-123",
      "nurl": "https://win-notification.com",
      "targeting": {
        "amzniid": "user-id",
        "amznbid": "2.50"
      }
    }
  ]
}
```

The integration automatically transforms this to unified `Bid` format.

## Testing

### Unit Tests

```bash
cargo test -p trusted-server-common aps::tests
```

### Local Testing

1. Configure APS in `trusted-server.toml`
2. Set up Fastly backend
3. Run locally:
   ```bash
   fastly compute serve
   ```
4. Inspect logs for bid requests/responses

### Testing

For local testing without live APS credentials, configure the integration with test values:

```toml
[integrations.aps]
enabled = true
pub_id = "test-publisher-123"
endpoint = "https://aax.amazon-adsystem.com/e/dtb/bid"
timeout_ms = 800
```

Run unit tests:
```bash
cargo test -p trusted-server-common aps
```

## Monitoring

The integration logs detailed information at different levels:

```rust
// Info: Auction lifecycle
log::info!("APS: requesting bids for 2 slots (pub_id: 5128)");
log::info!("APS returned 2 bids in 150ms");

// Debug: Request/response payloads
log::debug!("APS: sending bid request: {...}");
log::debug!("APS: received response: {...}");

// Warn: Non-success responses
log::warn!("APS returned non-success status: 400");
```

## Troubleshooting

### Problem: No bids returned

**Check:**
1. Verify `pub_id` is correct
2. Check backend is configured: `fastly backend list`
3. Increase timeout: `timeout_ms = 1200`
4. View logs with `fastly compute serve`

**Possible causes:**
- Invalid `pub_id` configuration
- Timeout too short (increase `timeout_ms`)
- Backend not configured correctly
- APS account not active

### Problem: "Backend not found" error

**Solution:**
Add backend to Fastly (see Step 1)

### Problem: Parse errors

**Check:**
- Enable debug logging
- Verify endpoint URL is correct
- Check APS API for changes

**Solutions:**
- Enable debug logging to inspect response
- Verify APS endpoint is correct
- Check for API changes (update structs if needed)

### Timeout Errors

**Symptoms:**
```
APS request failed: Timeout
```

**Solutions:**
- Increase `timeout_ms` in config
- Check backend connectivity
- Verify DNS resolution for `aax.amazon-adsystem.com`

## Performance Tuning

### Timeout Configuration

- **Default**: 800ms (matches APS client-side behavior)
- **Aggressive**: 500ms (reduce latency, may miss some bids)
- **Conservative**: 1200ms (maximize fill rate)

```toml
[integrations.aps]
timeout_ms = 800  # Balance latency vs fill rate
```

### Orchestrator Timeout

Ensure orchestrator timeout exceeds provider timeout:

```toml
[auction]
timeout_ms = 2000  # > sum of all provider timeouts

[integrations.aps]
timeout_ms = 800

[integrations.prebid]
timeout_ms = 1000
```

## Integration with Client-Side

While this implementation focuses on server-side bidding, you can optionally add client-side components later:

1. **ID Resolution**: Integrate third-party ID vendors client-side
2. **Analytics**: Load `aps_csm.js` for viewability tracking
3. **Config Loading**: Fetch `/configs/{pub_id}` for advanced settings

See the original network flow documentation for client-side patterns.

## Future Enhancements

Potential additions for complete APS TAM parity:

1. **Video Support**: Add video slot formats
2. **ID Enrichment**: Server-side ID resolution (LiveRamp, ID5, etc.)
3. **Advanced Targeting**: Pass user segments, geo data
4. **Config API**: Fetch APS configuration from `/configs/{pub_id}`
5. **Analytics**: Integrate with APS measurement endpoints

## Reference

- **APS TAM Docs**: https://aps.amazon.com/aps/transparent-ad-marketplace-api/
- **Integration Code**: `crates/common/src/integrations/aps.rs`
- **Tests**: `crates/common/src/integrations/aps.rs#tests`
- **Example Config**: `trusted-server.toml`

## Support

Questions? Issues?
1. Check logs: `fastly compute serve`
2. Run tests: `cargo test aps`
3. Verify configuration in `trusted-server.toml`
4. Review this documentation

---

**Status**: ✅ Production Ready (Server-Side Bidding)

**Last Updated**: 2025-12-23
