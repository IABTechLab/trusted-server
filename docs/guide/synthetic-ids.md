# Synthetic IDs

Trusted Server's Synthetic ID module maintains user recognition across all browsers through first-party identifiers.

## What are Synthetic IDs?

Synthetic IDs are privacy-safe identifiers generated on a first site visit using HMAC-based templates that allow tracking with user consent while protecting user privacy. Trusted Server derives a deterministic HMAC base from the template inputs and appends a short random suffix to reduce collision risk. They are passed in requests on subsequent visits and activity.

Trusted Server surfaces the current synthetic ID via response headers and a first-party cookie. For the exact header and cookie names, see the [API Reference](/guide/api-reference).

## How They Work

### HMAC-Based Generation

Synthetic IDs use HMAC (Hash-based Message Authentication Code) to generate a deterministic base from a configurable template, then append a short random suffix.

**Format**: `64-hex-hmac`.`6-alphanumeric-suffix`

**IP normalization**: IPv6 addresses are normalized to a /64 prefix before templating.

## Configuration

Configure synthetic ID templates and secrets in `trusted-server.toml`. See the full [Configuration Reference](/guide/configuration) for the `synthetic` section and environment variable overrides.

## Privacy Considerations

- IDs are only generated with explicit user consent
- No personally identifiable information (PII) is included
- Templates are configurable per-deployment
- IDs can be rotated on schedule

## Best Practices

1. Always verify GDPR consent before generating IDs
2. Rotate secret keys periodically
3. Use appropriate template complexity for your use case
4. Monitor ID collision rates

## Next Steps

- Learn about [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)
- Learn about [Collective Sync](/guide/collective-sync) for cross-publisher data sharing details and diagrams
