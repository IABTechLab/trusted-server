# Server Side Cookies (SSC)

Trusted Server's SSC module maintains user recognition across all browsers through first-party identifiers.

## What are Server Side Cookies?

Server Side Cookies (SSC) are privacy-safe identifiers generated on a first site visit using HMAC-based hashing that allow tracking with user consent while protecting user privacy. Trusted Server derives a deterministic HMAC base from the client IP address and appends a short random suffix to reduce collision risk. They are passed in requests on subsequent visits and activity.

Trusted Server surfaces the current SSC ID via response headers and a first-party cookie. For the exact header and cookie names, see the [API Reference](/guide/api-reference).

## How They Work

### HMAC-Based Generation

SSC IDs use HMAC (Hash-based Message Authentication Code) to generate a deterministic base from the client IP address, then append a short random suffix.

**Format**: `64-hex-hmac`.`6-alphanumeric-suffix`

**IP normalization**: IPv6 addresses are normalized to a /64 prefix before hashing.

## Configuration

Configure SSC secrets in `trusted-server.toml`. See the full [Configuration Reference](/guide/configuration) for the `ssc` section and environment variable overrides.

## Privacy Considerations

- IDs are only generated with explicit user consent
- No personally identifiable information (PII) is included
- The hash input is the client IP address only
- IDs can be rotated on schedule

## Best Practices

1. Always verify GDPR consent before generating IDs
2. Rotate secret keys periodically
3. Monitor ID collision rates

## Next Steps

- Learn about [GDPR Compliance](/guide/gdpr-compliance)
- Configure [Ad Serving](/guide/ad-serving)
- Learn about [Collective Sync](/guide/collective-sync) for cross-publisher data sharing details and diagrams
