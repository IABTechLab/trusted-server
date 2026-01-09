# APS Integration Quick Start

Get started with server-side APS bidding in 5 minutes.

## Prerequisites

- Fastly Compute service configured
- APS publisher account with valid `pub_id`
- Rust toolchain installed (see main README)

## Step 1: Configure Fastly Backend

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

## Step 2: Enable APS in Configuration

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

## Step 3: Deploy or Test Locally

**Deploy to Fastly:**
```bash
cargo build --release --target wasm32-wasip1
fastly compute publish
```

**Test Locally:**
```bash
fastly compute serve
```

## Step 4: Verify It's Working

Check the logs for APS activity:

```bash
# You should see:
INFO APS: requesting bids for 2 slots (pub_id: 5128)
DEBUG APS: sending bid request: {...}
DEBUG APS: received response: {...}
INFO APS returned 2 bids in 150ms
```

## Example Auction Configurations

### APS Only

```toml
[auction]
enabled = true
strategy = "parallel_only"
bidders = ["aps"]
```

### APS + Prebid (Parallel)

Best for maximum revenue:

```toml
[auction]
enabled = true
strategy = "parallel_only"
bidders = ["aps", "prebid"]
timeout_ms = 2000

[integrations.aps]
enabled = true
pub_id = "5128"
timeout_ms = 800

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com"
timeout_ms = 1000
```

### APS + Prebid + GAM Mediation

Let GAM decide winners:

```toml
[auction]
enabled = true
strategy = "parallel_mediation"
bidders = ["aps", "prebid"]
mediator = "gam"
```

## Testing Without Live Credentials

Use the mock provider for development:

```toml
[integrations.aps_mock]
enabled = true
bid_price = 2.50
fill_rate = 1.0  # Always return bids
```

Run tests:
```bash
cargo test -p trusted-server-common aps
```

## Troubleshooting

### Problem: No bids returned

**Check:**
1. Verify `pub_id` is correct
2. Check backend is configured: `fastly backend list`
3. Increase timeout: `timeout_ms = 1200`
4. View logs with `fastly compute serve`

### Problem: "Backend not found" error

**Solution:**
Add backend to Fastly (see Step 1)

### Problem: Parse errors

**Check:**
- Enable debug logging
- Verify endpoint URL is correct
- Check APS API for changes

## Next Steps

- **Add more bidders**: Configure Prebid, GAM alongside APS
- **Optimize timeouts**: Balance latency vs fill rate
- **Monitor performance**: Track bid rates, CPMs, latency
- **Add targeting**: Pass user segments, geo data (future enhancement)

## Resources

- **Full Documentation**: See `crates/common/src/integrations/aps_readme.md`
- **Code**: `crates/common/src/integrations/aps.rs`
- **Tests**: Run `cargo test aps` to see examples
- **Configuration**: `trusted-server.toml`

## Support

Questions? Issues?
1. Check logs: `fastly compute serve`
2. Run tests: `cargo test aps`
3. Review full docs in `aps_readme.md`

---

**You're all set!** APS server-side bidding is now enabled.
