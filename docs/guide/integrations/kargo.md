# Kargo SSP Integration

**Category**: SSP (Supply-Side Platform)
**Status**: In Development
**Type**: Direct SSP Integration

## Overview

The Kargo integration provides direct access to Kargo's supply-side platform for premium programmatic demand.

## What is Kargo?

Kargo is a mobile-first SSP specializing in high-impact, brand-safe advertising with:

- Premium brand demand
- Advanced creative formats
- Viewability optimization
- First-party data activation

## Planned Features

- **Direct Kargo Bidding**: Native Kargo bid adapter
- **First-Party Creative Rendering**: All creatives served through publisher domain
- **Enhanced Viewability Tracking**: Real-time viewability metrics
- **Brand Safety**: Pre-bid brand safety validation
- **Custom Creative Formats**: Support for Kargo's rich media units

## Expected Configuration

```toml
[integrations.kargo]
enabled = true
publisher_id = "your-kargo-publisher-id"
endpoint = "https://krk.kargo.com"
timeout_ms = 1000
```

## Use Cases

### Premium Brand Campaigns

Access to Kargo's premium brand advertisers for higher CPMs.

### Mobile Optimization

Kargo's mobile-first approach optimizes for mobile inventory.

### High-Impact Formats

Leverage Kargo's proprietary creative formats for better engagement.

## Status

This integration is currently being developed on a separate branch. Check with the team for the latest status.

**Expected Release**: Q1-Q2 2026

## Get Involved

Interested in beta testing the Kargo integration? Reach out via [GitHub Discussions](https://github.com/IABTechLab/trusted-server/discussions).

## Next Steps

- Review [Roadmap](/roadmap) for SSP integration plans
- Check [Prebid Integration](/guide/integrations/prebid) for current demand access
- Explore [Ad Serving](/guide/ad-serving) for general concepts
