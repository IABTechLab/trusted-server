# Permutive Integration

**Category**: Data
**Status**: Production
**Type**: Audience Data Platform

## Overview

The Permutive integration enables first-party audience segmentation and data collection by proxying Permutive's SDK and API endpoints through your domain.

## What is Permutive?

Permutive is a real-time data platform that helps publishers build and activate audience segments for advertising without relying on third-party cookies.

## Configuration

```toml
[integrations.permutive]
enabled = true
organization_id = "your-org-id"
workspace_id = "your-workspace-id"
project_id = "your-project-id"
api_endpoint = "https://api.permutive.com"
secure_signals_endpoint = "https://secure-signals.permutive.app"
cache_ttl_seconds = 3600
rewrite_sdk = true
```

## Endpoints

- `GET /integrations/permutive/sdk` - SDK serving
- `GET/POST /integrations/permutive/api/*` - API proxy
- `GET/POST /integrations/permutive/secure-signal/*` - Secure Signals (GAM integration)
- `GET/POST /integrations/permutive/events/*` - Event tracking
- `GET/POST /integrations/permutive/sync/*` - ID synchronization
- `GET /integrations/permutive/cdn/*` - CDN proxy

## Features

- **Real-time segmentation**: Build audience cohorts in real-time
- **First-party data**: All data collection through your domain
- **Secure Signals**: Integrate with Google Ad Manager
- **SDK caching**: Performance optimization (1 hour TTL)
- **Privacy compliance**: Consent-based activation

## Use Cases

### Publisher Audience Monetization

Collect first-party data, build segments, activate in programmatic auctions to increase CPMs.

### Contextual Targeting

Combine page context with user behavior for privacy-safe targeting.

### Cross-Site Insights

Aggregate audience data across your property portfolio.

## Implementation

See [crates/common/src/integrations/permutive.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/permutive.rs) for implementation details.

## Next Steps

- Review [Integrations Overview](/guide/integrations-overview) for comparison
- Check [Configuration Reference](/guide/configuration) for options
- Learn about [First-Party Proxy](/guide/first-party-proxy) architecture
