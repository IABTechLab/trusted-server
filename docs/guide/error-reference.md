# Error Reference

Common errors, their causes, and solutions when working with Trusted Server.

## Table of Contents

- [Configuration Errors](#configuration-errors)
- [Runtime Errors](#runtime-errors)
- [Integration Errors](#integration-errors)
- [Request Signing Errors](#request-signing-errors)
- [Build & Deployment Errors](#build--deployment-errors)

---

## Configuration Errors

### Failed to load settings

**Error Message:**
```
Failed to load settings: ParseError
```

**Cause:** Invalid TOML syntax in `trusted-server.toml`

**Solution:**
1. Validate TOML syntax using an online validator
2. Check for missing quotes around strings
3. Ensure array syntax uses square brackets: `["item1", "item2"]`
4. Verify section headers use brackets: `[section]`

**Example Fix:**
```toml
# ❌ Wrong
[publisher]
domain = test-publisher.com  # Missing quotes

# ✅ Correct
[publisher]
domain = "test-publisher.com"
```

---

### Missing required configuration

**Error Message:**
```
Missing required field: publisher.domain
```

**Cause:** Required configuration field not provided

**Solution:** Add the missing field to `trusted-server.toml`:

```toml
[publisher]
domain = "your-publisher-domain.com"
origin_url = "https://origin.your-publisher-domain.com"
proxy_secret = "change-me-to-random-string"
```

**Required Fields:**
- `publisher.domain`
- `publisher.origin_url`
- `publisher.proxy_secret`
- `synthetic.secret_key`

---

### Invalid URL format

**Error Message:**
```
Invalid URL in integrations.prebid.server_url
```

**Cause:** Malformed URL in configuration

**Solution:** Ensure URLs are well-formed with scheme:

```toml
# ❌ Wrong
[integrations.prebid]
server_url = "prebid-server.example.com"

# ✅ Correct
[integrations.prebid]
server_url = "https://prebid-server.example.com"
```

---

### Environment variable override failed

**Error Message:**
```
Failed to parse environment variable: TRUSTED_SERVER__PUBLISHER__DOMAIN
```

**Cause:** Environment variable format doesn't match expected type

**Solution:** Use correct format for the field type:

```bash
# For strings
TRUSTED_SERVER__PUBLISHER__DOMAIN="example.com"

# For numbers
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1000

# For booleans
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true

# For arrays (comma-separated)
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS="appnexus,rubicon"
```

See [Environment Variables Reference](./environment-variables.md) for complete patterns.

---

## Runtime Errors

### Synthetic ID generation failed

**Error Message:**
```
Failed to generate synthetic ID: KV store not available
```

**Cause:** KV store (counter_store or opid_store) not configured in Fastly

**Solution:**
1. Create KV stores in Fastly dashboard
2. Link them to your Compute service
3. Update `trusted-server.toml`:

```toml
[synthetic]
counter_store = "counter_store"  # Must match Fastly KV store name
opid_store = "opid_store"
```

4. For local development, configure in `fastly.toml`:

```toml
[local_server.kv_stores]
    [[local_server.kv_stores.counter_store]]
        key = "placeholder"
        data = "placeholder"
```

---

### Backend not found

**Error Message:**
```
Backend not found: prebid-server
```

**Cause:** Dynamic backend creation failed or backend not configured

**Solution:**

For integrations using dynamic backends (Prebid, Testlight):
- Ensure the integration is enabled
- Verify the URL is accessible from Fastly edge
- Check Fastly service limits (backend count)

For static backends, configure in Fastly dashboard:
1. Go to Origins → Hosts
2. Add backend with name matching configuration
3. Redeploy service

---

### Token validation failed

**Error Message:**
```
Invalid tstoken signature
```

**Cause:** URL was modified after signing, or proxy_secret mismatch

**Solution:**
1. Verify `publisher.proxy_secret` matches across environments
2. Don't manually modify signed URLs
3. Use `/first-party/sign` endpoint to generate new signatures
4. Check for URL encoding issues

**Debug:**
```bash
# Test URL signing
curl "https://edge.example.com/first-party/sign?url=https://external.com/pixel.gif"
```

---

### Request timeout

**Error Message:**
```
Upstream request timeout after 1000ms
```

**Cause:** Upstream service (Prebid Server, Permutive, etc.) didn't respond in time

**Solution:**
1. Increase timeout in configuration:

```toml
[integrations.prebid]
timeout_ms = 2000  # Increase from default 1000ms
```

2. Verify upstream service is responsive:
```bash
curl -w "%{time_total}\n" https://upstream-service.example.com
```

3. Check Fastly backend timeout settings

---

### Cookie domain mismatch

**Error Message:**
```
Warning: Cookie not set due to domain mismatch
```

**Cause:** `publisher.cookie_domain` doesn't match request domain

**Solution:**
```toml
[publisher]
domain = "example.com"
cookie_domain = ".example.com"  # Leading dot for subdomains
```

**Rules:**
- Use leading dot (`.example.com`) to cover subdomains
- Cookie domain must be parent of request domain
- Don't include protocol or port

---

## Integration Errors

### Prebid Server error

**Error Message:**
```
Prebid Server returned 400: Invalid OpenRTB request
```

**Cause:** OpenRTB transformation produced invalid request

**Solution:**
1. Enable debug mode:
```toml
[integrations.prebid]
debug = true
```

2. Check logs for request/response details
3. Verify bidders are supported by your Prebid Server
4. Ensure ad unit format is correct:

```javascript
{
  "code": "banner-1",
  "mediaTypes": {
    "banner": {
      "sizes": [[300, 250], [728, 90]]
    }
  }
}
```

---

### Next.js rewriting not working

**Error Message:**
```
Next.js links still pointing to origin domain
```

**Cause:** Rewrite attributes don't match JSON keys in Next.js data

**Solution:**
1. Inspect Next.js data payload in `<script id="__NEXT_DATA__">`
2. Update `rewrite_attributes` to match actual keys:

```toml
[integrations.nextjs]
enabled = true
rewrite_attributes = ["href", "link", "url", "src"]  # Add keys you find
```

3. For streaming payloads, check console for rewrite logs

---

### Permutive SDK not loading

**Error Message:**
```
Failed to fetch Permutive SDK: 404 Not Found
```

**Cause:** Incorrect organization_id or workspace_id

**Solution:**
1. Verify IDs in your Permutive dashboard URL:
   - `https://myorg.edge.permutive.app/workspace-123-web.js`
   - org: `myorg`
   - workspace: `workspace-123`

2. Update configuration:
```toml
[integrations.permutive]
organization_id = "myorg"
workspace_id = "workspace-123"
```

3. Test SDK URL directly:
```bash
curl https://myorg.edge.permutive.app/workspace-123-web.js
```

---

### Integration not proxying requests

**Error Message:**
```
No route matched for /integrations/custom/endpoint
```

**Cause:** Integration not enabled or route not registered

**Solution:**
1. Enable integration:
```toml
[integrations.custom]
enabled = true
```

2. Verify integration is compiled in (check build logs)
3. Restart/redeploy service (integrations loaded at startup)

---

## Request Signing Errors

### Key not found

**Error Message:**
```
Signing key not found: ts-2025-01-A
```

**Cause:** Key ID referenced but not present in Secret Store

**Solution:**
1. Check Config Store `current-kid` value
2. Verify corresponding key exists in Secret Store
3. Run key rotation to generate new key:

```bash
curl -X POST https://edge.example.com/admin/keys/rotate \
  -u admin:password
```

---

### JWKS endpoint empty

**Error Message:**
```
{
  "keys": []
}
```

**Cause:** No active keys in system

**Solution:**
1. Initialize keys using rotation endpoint:
```bash
curl -X POST https://edge.example.com/admin/keys/rotate \
  -u admin:password
```

2. Verify Config Store has `active-kids` entry
3. Check Secret Store contains the key

---

### Signature verification failed

**Error Message:**
```
{
  "verified": false,
  "message": "Invalid signature"
}
```

**Causes & Solutions:**

**Wrong payload encoding:**
```bash
# Ensure base64 encoding (not base64url)
echo -n "Hello World" | base64
```

**Wrong key ID:**
```bash
# Check current key ID
curl https://edge.example.com/.well-known/ts.jwks.json | jq '.keys[].kid'
```

**Signature from wrong key:**
- Verify you're using the current signing key
- Check key hasn't been rotated/deactivated

---

### Config/Secret Store not found

**Error Message:**
```
Config store not found: jwks_store
```

**Cause:** Store not created or not linked to Compute service

**Solution:**
1. Create stores in Fastly dashboard:
   - Settings → Config Stores → Create
   - Settings → Secret Stores → Create

2. Link to Compute service:
   - Service → Configuration → Config Stores
   - Add store with exact name from configuration

3. Update `trusted-server.toml`:
```toml
[request_signing]
enabled = true
config_store_id = "your-config-store-id"  # From Fastly dashboard
secret_store_id = "your-secret-store-id"
```

---

## Build & Deployment Errors

### WASM compilation failed

**Error Message:**
```
error: could not compile `trusted-server-fastly`
```

**Cause:** Rust compilation error or missing dependencies

**Solution:**
1. Update Rust toolchain:
```bash
rustup update
```

2. Install wasm32-wasip1 target:
```bash
rustup target add wasm32-wasip1
```

3. Clean and rebuild:
```bash
cargo clean
cargo build --target wasm32-wasip1 --release
```

---

### Fastly CLI deployment failed

**Error Message:**
```
Error: Service version activation failed
```

**Causes & Solutions:**

**Missing service ID:**
```toml
# Add to fastly.toml
service_id = "YOUR_SERVICE_ID"  # From Fastly dashboard
```

**Missing API token:**
```bash
# Authenticate Fastly CLI
fastly profile create
# Paste API token when prompted
```

**Service validation errors:**
- Check Fastly dashboard for validation messages
- Verify all required backends are configured
- Ensure KV/Config/Secret stores are linked

---

### TSJS build failed

**Error Message:**
```
ERROR: npm run build:custom failed
```

**Cause:** Node.js dependency or build issue

**Solution:**
1. Install Node.js dependencies:
```bash
cd crates/js/lib
npm install
```

2. Test build manually:
```bash
npm run build
```

3. Check for TypeScript errors:
```bash
npm run type-check
```

4. Skip TSJS build temporarily:
```bash
TSJS_SKIP_BUILD=1 cargo build
```

---

### Version mismatch error

**Error Message:**
```
Warning: viceroy version mismatch
```

**Cause:** Local test runtime (viceroy) out of date

**Solution:**
```bash
cargo install viceroy --force
```

---

## Debugging Tips

### Enable Debug Logging

**In configuration:**
```toml
[integrations.prebid]
debug = true

# Or via environment variable
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=true
```

**Check Fastly logs:**
```bash
fastly log-tail
```

---

### Test Endpoints Locally

```bash
# Start local server
fastly compute serve

# Test endpoint
curl http://localhost:7676/ad/render?slot=test&w=300&h=250
```

---

### Validate Configuration

```bash
# Test configuration load
cargo run --bin trusted-server-fastly -- --validate-config

# Or check startup logs
fastly compute serve 2>&1 | grep -i "settings"
```

---

### Inspect KV Store Contents

```bash
# List KV store keys (Fastly CLI)
fastly kv-store list --store-id=your-store-id

# Get specific key
fastly kv-store get --store-id=your-store-id --key=your-key
```

---

## Getting Help

If you encounter an error not listed here:

1. **Check logs** - Most errors include context in log messages
2. **Search GitHub Issues** - Someone may have encountered the same issue
3. **Enable debug mode** - Get detailed request/response logging
4. **Isolate the problem** - Test components individually
5. **Open an issue** - Provide error message, configuration, and steps to reproduce

**Useful Information to Include:**
- Complete error message
- Relevant configuration (redact secrets)
- Rust version (`cargo --version`)
- Fastly CLI version (`fastly version`)
- Steps to reproduce

---

## Common HTTP Status Codes

| Code | Meaning | Common Causes | Quick Fix |
|------|---------|---------------|-----------|
| **400** | Bad Request | Missing parameters, invalid JSON | Check request format |
| **401** | Unauthorized | Missing/invalid auth | Check credentials |
| **403** | Forbidden | Invalid token, disabled feature | Verify token/config |
| **404** | Not Found | Unknown endpoint | Check URL path |
| **500** | Internal Error | Configuration error, bug | Check logs |
| **502** | Bad Gateway | Backend unavailable | Verify backend |
| **504** | Gateway Timeout | Backend slow | Increase timeout |

---

## Next Steps

- Review [Configuration Reference](./configuration-reference.md)
- Check [Environment Variables](./environment-variables.md)
- Explore [API Reference](./api-reference.md)
- Learn about [Testing](./testing.md)
