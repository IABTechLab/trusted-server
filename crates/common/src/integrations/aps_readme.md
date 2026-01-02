# Amazon Publisher Services (APS) Integration

Server-side bidding integration for Amazon's Transparent Ad Marketplace (TAM).

## Overview

The APS integration enables publishers to request bids from Amazon's demand sources server-side, providing:

- **Privacy-first bidding**: No client-side ID tracking or third-party cookies required
- **Reduced latency**: Server-side auction reduces page load impact
- **Unified auction**: Integrates seamlessly with other bidders (Prebid, GAM)
- **Performance**: Async bidding with configurable timeouts

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

### Parallel Bidding (Recommended)

Run APS alongside other bidders:

```toml
[auction]
enabled = true
strategy = "parallel_only"
bidders = ["aps", "prebid"]
timeout_ms = 2000
```

**Benefits:**
- Maximum fill rate
- Best CPM selection
- All bidders compete equally

### Waterfall

Try APS first, fallback to others:

```toml
[auction]
enabled = true
strategy = "waterfall"
bidders = ["aps", "prebid"]  # Order matters
timeout_ms = 2000
```

**Benefits:**
- Reduced latency (stops on first bid)
- Priority to APS demand

### With Mediation

Use GAM to mediate all bids:

```toml
[auction]
enabled = true
strategy = "parallel_mediation"
bidders = ["aps", "prebid"]
mediator = "gam"
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

## Backend Configuration

APS requires a Fastly backend configured for `aax.amazon-adsystem.com`.

### Via Fastly UI

1. Go to your service → Origins
2. Add backend:
   - **Name**: `aax.amazon-adsystem.com`
   - **Address**: `aax.amazon-adsystem.com`
   - **Port**: 443
   - **Enable TLS**: Yes

### Via `fastly.toml`

```toml
[[backends]]
name = "aax.amazon-adsystem.com"
address = "aax.amazon-adsystem.com"
port = 443
use_ssl = true
ssl_cert_hostname = "aax.amazon-adsystem.com"
ssl_sni_hostname = "aax.amazon-adsystem.com"
```

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

### Mock Provider

For testing without live APS credentials:

```toml
[integrations.aps_mock]
enabled = true
timeout_ms = 800
bid_price = 2.50
fill_rate = 1.0  # Always bid
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

## Integration with Client-Side

While this implementation focuses on server-side bidding, you can optionally add client-side components later:

1. **ID Resolution**: Integrate third-party ID vendors client-side
2. **Analytics**: Load `aps_csm.js` for viewability tracking
3. **Config Loading**: Fetch `/configs/{pub_id}` for advanced settings

See the original network flow documentation for client-side patterns.

## Troubleshooting

### No Bids Returned

**Possible causes:**
- Invalid `pub_id` configuration
- Timeout too short (increase `timeout_ms`)
- Backend not configured correctly
- APS account not active

**Check:**
```bash
# View logs
fastly compute serve

# Verify backend
fastly backend list --service-id=YOUR_SERVICE_ID
```

### Parse Errors

**Symptoms:**
```
Failed to parse APS response JSON
```

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

For issues or questions:
1. Check logs with `fastly compute serve`
2. Verify configuration in `trusted-server.toml`
3. Run unit tests: `cargo test aps`
4. Review this documentation

---

**Status**: ✅ Production Ready (Server-Side Bidding)

**Last Updated**: 2025-12-23
