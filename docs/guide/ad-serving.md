# Ad Serving

Learn how Trusted Server handles privacy-compliant ad serving.

## Overview

Trusted Server provides edge-based ad serving with built-in GDPR compliance and real-time bidding support.

## Supported Integrations

### Equativ

Primary ad server integration with support for:

- Direct ad requests
- Creative proxying
- Click tracking
- Impression tracking

### Prebid

Real-time bidding integration:

- Header bidding support
- Bid caching
- Timeout management
- Winner selection

## Ad Request Flow

1. Request validation
2. GDPR consent check
3. Synthetic ID generation (if consented)
4. Ad server request
5. Response processing
6. Creative delivery

## Configuration

Configure ad servers in `trusted-server.toml`:

```toml
[ad_servers.equativ]
endpoint = "https://ad-server.example.com"
timeout_ms = 1000
enabled = true

[prebid]
timeout_ms = 1500
cache_ttl = 300
```

## Creative Handling

### Proxy Mode

Creatives can be proxied through Trusted Server for:

- Security scanning
- Content modification
- Click tracking injection
- GDPR compliance

### Direct Mode

Creatives served directly from ad server:

- Lower latency
- Reduced edge load
- Less control over content

## Tracking

### Impression Tracking

```javascript
// Placeholder example
trustedServer.trackImpression({
  adId: 'ad-123',
  syntheticId: 'synthetic-xyz',
  consent: true,
})
```

### Click Tracking

Click tracking with privacy preservation:

- No PII in URLs
- Synthetic ID only (with consent)
- Encrypted parameters

## Performance

### Edge Caching

- Bid responses cached at edge
- Creative assets cached
- Configuration cached
- Reduced origin requests

### Timeouts

Configurable timeouts for:

- Ad server requests
- Prebid auctions
- Creative fetching

## Best Practices

1. Set appropriate timeouts for your use case
2. Enable caching for frequently requested ads
3. Monitor ad server response times
4. Use proxy mode for security-sensitive content
5. Implement fallback ads

## Next Steps

- Review [Architecture](/guide/architecture)
- Configure [Testing](/guide/testing)
