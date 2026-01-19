# Didomi Integration

**Category**: CMP (Consent Management Platform)
**Status**: Production
**Type**: Reverse Proxy for Consent Management

## Overview

The Didomi integration enables first-party serving of Didomi's consent management platform (CMP) through Trusted Server. By proxying Didomi's SDK and API through your domain, you maintain first-party context while ensuring GDPR/TCF 2.2 compliance.

## What is Didomi?

Didomi is a Consent Management Platform that helps publishers comply with GDPR, CCPA, and other privacy regulations by managing user consent for data collection and processing.

**Key Capabilities**:
- TCF 2.2 (Transparency & Consent Framework) compliance
- Custom consent notices and preferences
- Vendor management
- Consent analytics and reporting
- Multi-regulation support (GDPR, CCPA, LGPD)

## How It Works

```
┌──────────────────────────────────────────────────┐
│  Browser Request                                 │
│  GET /integrations/didomi/consent/loader.js      │
│  ↓                                               │
│  Trusted Server (First-Party Domain)             │
│  ↓                                               │
│  Proxy to Didomi SDK Origin                      │
│  https://sdk.privacy-center.org/loader.js        │
│  ↓                                               │
│  Return SDK (appears first-party to browser)     │
└──────────────────────────────────────────────────┘
```

**Benefits**:
- Didomi SDK loads from your domain (not `privacy-center.org`)
- First-party cookies for consent storage
- Improved tracking prevention compatibility
- Better page load performance

## Configuration

Add Didomi configuration to `trusted-server.toml`:

```toml
[integrations.didomi]
enabled = true
sdk_origin = "https://sdk.privacy-center.org"
api_origin = "https://api.privacy-center.org"
```

### Configuration Options

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `enabled` | boolean | No | `false` | Enable/disable integration |
| `sdk_origin` | string | Yes | `https://sdk.privacy-center.org` | Didomi SDK backend URL |
| `api_origin` | string | Yes | `https://api.privacy-center.org` | Didomi API backend URL |

### Environment Variables

```bash
TRUSTED_SERVER__INTEGRATIONS__DIDOMI__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__DIDOMI__SDK_ORIGIN=https://sdk.privacy-center.org
TRUSTED_SERVER__INTEGRATIONS__DIDOMI__API_ORIGIN=https://api.privacy-center.org
```

## Endpoints

### SDK Proxy

**Pattern**: `/integrations/didomi/consent/*` (except `/api/*`)

Proxies Didomi SDK resources through first-party domain.

**Example**:
```
Original: https://sdk.privacy-center.org/24cd1234/loader.js
Proxied:  https://your-domain.com/integrations/didomi/consent/24cd1234/loader.js
```

**Headers Forwarded**:
- `User-Agent`
- `Accept`
- `Accept-Language`
- `Accept-Encoding`
- `Referer`
- `Origin`
- `Authorization`

**Geo Headers** (SDK only):
- `X-Geo-Country` ← `FastlyGeo-CountryCode`
- `X-Geo-Region` ← `FastlyGeo-Region`
- `CloudFront-Viewer-Country` ← `FastlyGeo-CountryCode`

**CORS Headers** (added to SDK responses):
```http
Access-Control-Allow-Origin: *
Access-Control-Allow-Headers: Content-Type, Authorization, X-Requested-With
Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS
```

### API Proxy

**Pattern**: `/integrations/didomi/consent/api/*`

Proxies Didomi API requests (consent events, user preferences, etc.).

**Example**:
```
Original: https://api.privacy-center.org/v1/events
Proxied:  https://your-domain.com/integrations/didomi/consent/api/v1/events
```

**Methods**: GET, POST, PUT, DELETE, OPTIONS

**Note**: API requests do NOT receive CORS headers (handled by Didomi API).

## Integration with Trusted Server

### Consent Validation

Didomi consent status is checked before:
- Generating synthetic IDs
- Syncing with identity partners (Lockr)
- Activating tracking pixels
- Sharing data with third parties

```rust
// Example consent check
if !has_didomi_consent(&request, Purpose::Tracking) {
    return reject_tracking();
}
```

### TCF 2.2 Support

Didomi integration supports IAB's Transparency & Consent Framework 2.2:
- TCF consent strings (TC strings)
- Vendor consent validation
- Purpose consent enforcement
- Special feature consent

## Use Cases

### 1. First-Party Consent Management

**Problem**: Third-party consent scripts blocked by tracking prevention.

**Solution**: Serve Didomi SDK from your domain via Trusted Server proxy.

**Benefit**: Consent notice loads reliably, compliance maintained.

### 2. Regional Consent Enforcement

**Problem**: Different consent requirements per region (GDPR, CCPA, LGPD).

**Solution**: Didomi provides region-specific consent flows, Trusted Server forwards geo data.

**Benefit**: Automatic compliance with regional privacy laws.

### 3. Consent-Based Data Activation

**Problem**: Need to enforce consent before activating analytics/advertising.

**Solution**: Check Didomi consent status in Trusted Server before data processing.

**Benefit**: Provable compliance, reduced regulatory risk.

## Implementation

The Didomi integration is implemented in [crates/common/src/integrations/didomi.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/didomi.rs).

### Key Components

**Backend Selection** (line 74-80):
```rust
fn backend_for_path(&self, consent_path: &str) -> DidomiBackend {
    if consent_path.starts_with("/api/") {
        DidomiBackend::Api  // Route to API origin
    } else {
        DidomiBackend::Sdk  // Route to SDK origin
    }
}
```

**Header Forwarding** (line 100-127):
- Forwards standard HTTP headers
- Adds geo headers for SDK requests
- Preserves client IP via `X-Forwarded-For`

**CORS Management** (line 143-153):
- Adds CORS headers to SDK responses
- Skips CORS for API requests (Didomi API handles it)

## Frontend Integration

### Load Didomi SDK

Replace your direct Didomi SDK reference with the proxied version:

```html
<!-- ❌ Old (third-party) -->
<script src="https://sdk.privacy-center.org/24cd1234/loader.js"></script>

<!-- ✅ New (first-party via Trusted Server) -->
<script src="/integrations/didomi/consent/24cd1234/loader.js"></script>
```

### Access Consent Status

Use Didomi's standard JavaScript API:

```javascript
// Wait for Didomi to load
window.didomiOnReady = window.didomiOnReady || [];
window.didomiOnReady.push(function (Didomi) {

  // Check consent for specific purpose
  if (Didomi.getUserStatus().purposes.consent.enabled.includes('cookies')) {
    // User consented to cookies
    initializeAnalytics();
  }

  // Listen for consent changes
  Didomi.on('consent.changed', function() {
    console.log('Consent status changed');
  });
});
```

## Best Practices

### 1. Configure Didomi ID

Ensure your Didomi organization ID is in the SDK path:

```html
<script src="/integrations/didomi/consent/{YOUR_DIDOMI_ID}/loader.js"></script>
```

### 2. Preconnect to Proxy

Add DNS preconnect for faster loading:

```html
<link rel="preconnect" href="https://your-domain.com">
<link rel="dns-prefetch" href="https://your-domain.com">
```

### 3. Cache SDK Responses

Configure caching headers for Didomi SDK:

```http
Cache-Control: public, max-age=3600
```

### 4. Monitor Consent Rate

Track consent acceptance/rejection rates:
- Low acceptance → Review consent notice clarity
- Regional variations → Adjust messaging
- Trend analysis → Optimize user experience

## Troubleshooting

### Didomi SDK Not Loading

**Symptoms**:
- Consent notice doesn't appear
- Console errors about missing Didomi

**Solutions**:
- Verify `/integrations/didomi/consent/` path is correct
- Check `sdk_origin` configuration
- Ensure Didomi ID in script path is valid
- Inspect network tab for 404/403 errors

### CORS Errors

**Symptoms**:
- Browser console shows CORS errors
- SDK requests blocked

**Solutions**:
- Verify integration adds CORS headers for SDK requests
- Check `Access-Control-Allow-Origin` is present
- Ensure requests go through proxy (not directly to Didomi)

### API Requests Failing

**Symptoms**:
- Consent events not recording
- Preference updates failing

**Solutions**:
- Check `/integrations/didomi/consent/api/*` routing
- Verify `api_origin` configuration
- Review Authorization headers are forwarded
- Inspect Didomi API credentials

## Performance

### Typical Latency

- SDK load: 100-200ms (first load)
- Cached SDK: <50ms
- API calls: 50-150ms
- Total overhead: ~20ms (proxy layer)

### Optimization

- Enable HTTP/2 for multiplexing
- Use CDN caching for SDK files
- Implement service worker for offline consent
- Lazy-load consent notice

## Security

### Content Security Policy

Add Didomi to your CSP:

```http
Content-Security-Policy:
  script-src 'self' /integrations/didomi/;
  connect-src 'self' /integrations/didomi/;
  frame-src 'self' /integrations/didomi/;
```

### Data Privacy

- Didomi consent data stays first-party
- No PII shared without consent
- Consent strings stored locally
- User can withdraw consent anytime

## Next Steps

- Review [GDPR Compliance](/guide/gdpr-compliance) for consent enforcement
- Explore [Lockr Integration](/guide/integrations/lockr) for consent-based identity
- Check [Configuration](/guide/configuration) for advanced setup
- Read [First-Party Proxy](/guide/first-party-proxy) for proxy architecture
