# Amazon Publisher Services (APS) Integration

**Category**: Demand Wrapper
**Status**: Planned for 2026
**Type**: Header Bidding / Transparent Ad Marketplace

## Overview

The Amazon Publisher Services (APS) integration will enable direct integration with Amazon's Transparent Ad Marketplace (TAM) and A9 bidding engine.

## What is APS?

Amazon Publisher Services provides publishers with access to Amazon's advertising demand through:

- **Transparent Ad Marketplace (TAM)**: Server-to-server header bidding
- **A9 Bidding**: Amazon's proprietary bidding algorithm
- **Unified Ad Marketplace (UAM)**: Client-side header bidding wrapper

## Planned Features

- **TAM Integration**: Server-side A9 bid requests
- **First-Party Context**: All requests through publisher domain
- **Privacy-Safe IDs**: Synthetic ID integration for user recognition
- **Dynamic Pricing**: Real-time floor price optimization
- **Creative Proxying**: First-party creative delivery
- **Reporting Integration**: Performance metrics and analytics

## Expected Configuration

```toml
[integrations.aps]
enabled = true
publisher_id = "your-amazon-publisher-id"
endpoint = "https://aax.amazon-adsystem.com"
timeout_ms = 1000
price_floors = true
```

## Use Cases

### Amazon Demand Access

Tap into Amazon's unique shopping and behavioral data for better targeting and higher CPMs.

### Server-Side TAM

Move APS bidding server-side to reduce client-side latency and improve page performance.

### Hybrid Wrapper

Combine APS with Prebid for comprehensive demand access.

## Status

This integration is in the planning phase. Development will begin in Q2 2026.

**Target Release**: Q2 2026

## Comparison: APS vs Prebid

| Feature          | APS                  | Prebid       |
| ---------------- | -------------------- | ------------ |
| Demand Source    | Amazon exclusive     | 300+ bidders |
| Integration      | Proprietary          | Open-source  |
| Data Advantage   | Amazon shopping data | Neutral      |
| Setup Complexity | Medium               | Low          |

## Get Involved

Want to contribute to the APS integration? Check the [GitHub Issues](https://github.com/IABTechLab/trusted-server/issues) for details.

## Next Steps

- Review [Roadmap](/roadmap) for development timeline
- Check [Prebid Integration](/guide/integrations/prebid) for current header bidding
- Learn about [Ad Serving](/guide/ad-serving) concepts
