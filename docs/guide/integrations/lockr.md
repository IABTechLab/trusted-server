# Lockr Integration

**Category**: Identity
**Status**: Production
**Type**: Identity Management & Privacy Vault

## Overview

The Lockr integration enables first-party identity resolution and data management through Lockr's identity vault platform. This integration provides identity synchronization, with downstream forwarding subject to available consent signals.

## What is Lockr?

Lockr is an identity resolution and privacy platform that helps publishers manage user identities across fragmented environments (cookieless browsers, multiple devices, etc.) subject to user consent.

**Key Capabilities**:

- Identity graph management
- Consent-based data sharing
- Secure identity vault
- Cross-device user recognition
- Publisher-owned identity infrastructure

## How It Works

```
┌──────────────────────────────────────────────────┐
│  User Visit                                      │
│  ↓                                               │
│  Trusted Server generates EC ID                  │
│  ↓                                               │
│  Lockr Integration maps to Lockr Identity        │
│  ↓                                               │
│  Identity synchronized (with consent)            │
│  ↓                                               │
│  Lockr ID added to bid requests (user.ext.eids) │
└──────────────────────────────────────────────────┘
```

## Configuration

Add Lockr configuration to `trusted-server.toml`:

```toml
[integrations.lockr]
enabled = true
api_endpoint = "https://api.lockr.io"
organization_id = "your-org-id"
project_id = "your-project-id"
```

### Configuration Options

| Field             | Type    | Required | Description                                   |
| ----------------- | ------- | -------- | --------------------------------------------- |
| `enabled`         | boolean | No       | Enable/disable integration (default: `false`) |
| `api_endpoint`    | string  | Yes      | Lockr API endpoint URL                        |
| `organization_id` | string  | Yes      | Your Lockr organization ID                    |
| `project_id`      | string  | Yes      | Your Lockr project ID                         |

### Environment Variables

```bash
TRUSTED_SERVER__INTEGRATIONS__LOCKR__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__LOCKR__API_ENDPOINT=https://api.lockr.io
TRUSTED_SERVER__INTEGRATIONS__LOCKR__ORGANIZATION_ID=your-org-id
TRUSTED_SERVER__INTEGRATIONS__LOCKR__PROJECT_ID=your-project-id
```

## Features

### Identity Synchronization

Lockr integration automatically syncs Trusted Server EC IDs with Lockr's identity vault:

1. User visits site (Trusted Server generates EC ID)
2. If GDPR consent granted, sync with Lockr
3. Lockr returns unified identity
4. Identity available for bid requests and analytics

### Privacy Vault

User data is stored securely in Lockr's privacy vault:

- Encrypted at rest
- Consent-based access
- User right to erasure
- Data portability support

### Extended ID (EID) Support

The Lockr SDK writes its ID5 EID into Prebid's first-party data with
`pbjs.setConfig({ ortb2: { user: { ext: { eids } } } })`. Prebid includes it in
the `trustedServer` bidder request, and the adapter forwards it to `/auction`
with its EID-level fields, for example:

```json
{
  "source": "id5-sync.com",
  "inserter": "inserter.example.com",
  "mm": 2,
  "uids": [{ "id": "fictional-id5-id", "atype": 1 }]
}
```

The SDK writes the EID after its own network call, so the first auction on a
page may run before it is available. Later auctions include it, and requests
without an EID body get it from the `ts-eids` cookie and KV.

To store the ID in the EC identity graph, register the source:

```toml
[[ec.partners]]
name = "ID5 via Lockr"
source_domain = "id5-sync.com"
bidstream_enabled = true
```

Where GDPR applies and Prebid's TCF enforcement is active, Prebid sends the
`trustedServer` adapter no EIDs unless the publisher grants the exception
described in
[TCF and the `trustedServer` adapter](./prebid.md#tcf-and-the-trustedserver-adapter).

## Use Cases

### 1. Cookieless Identity Resolution

**Problem**: Safari and Firefox block third-party cookies, fragmenting user identity.

**Solution**: Lockr provides first-party identity resolution that works across cookieless environments.

**Benefit**: Maintain user recognition and monetization in browsers that restrict third-party cookies.

### 2. Cross-Device User Recognition

**Problem**: Users access content from multiple devices, appearing as different users.

**Solution**: Lockr's identity graph links devices to a unified user profile.

**Benefit**: Better audience targeting and frequency capping across devices.

### 3. Consent-Based Data Sharing

**Problem**: Sharing user data with partners raises GDPR/CCPA compliance risks.

**Solution**: Lockr enforces consent-based access to identity data.

**Benefit**: Data monetization gated on recorded consent.

## Implementation Details

The Lockr integration is implemented in [crates/trusted-server-core/src/integrations/lockr.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/trusted-server-core/src/integrations/lockr.rs).

### Key Components

**Routed Endpoints**:

- Route: `/integrations/lockr/sdk` (GET) serves the Lockr SDK first-party
- Route: `/integrations/lockr/api/*` (GET, POST) proxies Lockr API calls
- Purpose: Keep SDK delivery and identity API traffic on the publisher origin

**ID Mapping**:

- Maps Trusted Server EC ID → Lockr unified ID
- Cached for performance
- Respects consent status

**Consent Validation**:

- Checks consent signals before syncing
- Integrates with CMP (e.g., Didomi)
- Respects user withdrawal of consent

## Best Practices

### 1. Consent First

Always validate consent before initiating Lockr sync:

```rust
if !has_gdpr_consent() {
    return; // Skip Lockr sync
}
```

### 2. Cache Lockr IDs

Cache Lockr identity mappings to reduce API calls:

- TTL: 24 hours recommended
- Invalidate on consent withdrawal
- Use KV store for persistence

### 3. Monitor Sync Rate

Track Lockr sync success/failure rates:

- Alert on elevated failures
- Monitor API latency
- Track consent decline impact

### 4. Test Identity Flow

Validate end-to-end identity flow:

```bash
# 1. Generate EC ID
curl https://edge.example.com/

# 2. Verify Lockr sync (check logs)
# 3. Validate EID in bid request
```

## Troubleshooting

### Lockr Sync Fails

**Symptoms**:

- No Lockr ID in bid requests
- Sync endpoint returns errors

**Solutions**:

- Verify `organization_id` and `project_id` are correct
- Check Lockr API endpoint is reachable
- Ensure GDPR consent is granted
- Review Lockr API credentials

### Missing EID in Bid Requests

**Symptoms**:

- Lockr sync succeeds but EID missing from OpenRTB

**Solutions**:

- Confirm `pbjs.getEvents()` shows a `bidRequested` event for `trustedServer`
  whose `ortb2.user.ext.eids` contains the `id5-sync.com` entry
- Under GDPR, confirm the publisher's Prebid config grants `trustedServer` the
  full TCF exception
- Decode the `/auction` response's `x-ts-eids` header to see what was forwarded

## Performance

### Typical Latency

- Identity sync: 50-100ms
- Cached lookup: <5ms
- API timeout: 500ms (configurable)

### Optimization Tips

- Enable caching for repeat visitors
- Batch sync operations when possible
- Use async/non-blocking sync
- Monitor API quota usage

## Security Considerations

### API Credentials

- Store Lockr credentials in secret management system
- Rotate credentials periodically
- Never commit credentials to git
- Use environment variables in production

### Data Privacy

- Only sync with explicit user consent
- Implement right to erasure
- Audit data access logs
- Encrypt data in transit (HTTPS)

## Next Steps

- Learn about [Edge Cookies](/guide/edge-cookies) for identity generation
- Review [GDPR Compliance](/guide/gdpr-compliance) for consent management
- Explore [Didomi Integration](/guide/integrations/didomi) for CMP integration
- Check [Configuration Reference](/guide/configuration) for advanced options
