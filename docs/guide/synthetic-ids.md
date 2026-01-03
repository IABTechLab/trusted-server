# Synthetic IDs

Learn about privacy-preserving synthetic ID generation in Trusted Server.

## What are Synthetic IDs?

Synthetic IDs are privacy-safe identifiers generated using HMAC-based templates that allow tracking with user consent while protecting user privacy.

## How They Work

### HMAC-Based Generation

Synthetic IDs use HMAC (Hash-based Message Authentication Code) to generate deterministic but privacy-safe identifiers.

```rust
// Example placeholder
synthetic_id = hmac_sha256(secret_key, template_data)
```

### Template System

Templates define how synthetic IDs are constructed from various input sources:

- User consent signals
- Domain information
- Temporal data
- Custom parameters

## Configuration

Configure synthetic ID templates in `trusted-server.toml`:

```toml
[synthetic_ids]
template = "{{domain}}-{{timestamp}}-{{consent_hash}}"
secret_key = "your-secret-key"
```

## Privacy Considerations

- IDs are only generated with explicit user consent
- No personally identifiable information (PII) is included
- Templates are configurable per-deployment
- IDs can be rotated on schedule

## Usage Example

```javascript
// Placeholder example
const syntheticId = await trustedServer.generateSyntheticId({
  domain: 'example.com',
  consent: true,
})
```

## Best Practices

1. Always verify GDPR consent before generating IDs
2. Rotate secret keys periodically
3. Use appropriate template complexity for your use case
4. Monitor ID collision rates

## Next Steps

- Learn about [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)
