# GDPR Compliance

Understanding GDPR compliance and consent management in Trusted Server.

## Overview

Trusted Server enforces GDPR compliance at the edge, ensuring all tracking and data collection activities require explicit user consent.

## Consent Management

### Consent Validation

All requests are validated for proper GDPR consent before any tracking occurs:

```rust
// Placeholder example
if !validate_gdpr_consent(&request) {
    return reject_tracking();
}
```

### Consent Sources

Trusted Server supports multiple consent frameworks:

- TCF (Transparency & Consent Framework)
- Custom consent signals
- First-party consent cookies

## Implementation

### Checking Consent

```javascript
// Placeholder example
const hasConsent = await trustedServer.checkConsent({
  purposes: ['storage', 'personalization'],
  vendors: [vendor_id],
})
```

### Consent Storage

Consent signals are:

- Validated on every request
- Not persisted without explicit consent
- Respected across all operations

## Privacy Controls

### User Rights

Trusted Server supports:

- Right to access
- Right to erasure
- Right to data portability
- Right to object

### Data Minimization

Only essential data is collected:

- Synthetic IDs (with consent)
- Minimal request metadata
- No PII storage

## Configuration

Configure GDPR settings in `trusted-server.toml`:

```toml
[gdpr]
require_consent = true
tcf_version = "2.2"
default_action = "reject"
```

## Compliance Features

- Consent checks before ID generation
- Automatic rejection without consent
- Audit logging for compliance
- Regional enforcement rules

## Best Practices

1. Always require explicit consent
2. Respect user withdrawal of consent
3. Document consent mechanisms
4. Regular compliance audits
5. Keep consent records

## Next Steps

- Configure [Ad Serving](/guide/ad-serving)
- Review [Architecture](/guide/architecture)
