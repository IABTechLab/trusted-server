# Google Ad Manager (GAM) Integration

**Category**: Ad Serving
**Status**: Planned for 2026
**Type**: Ad Server

## Overview

The Google Ad Manager (GAM) integration will enable direct integration with Google's ad serving platform, providing first-party ad delivery and reporting.

## Planned Features

- **Direct GAM Integration**: Native support for GAM ad requests
- **Dynamic Ad Slots**: Programmatic slot management
- **Programmatic Guaranteed**: Direct deals and PG support
- **First-Party Reporting**: Ad performance metrics
- **Secure Signals**: Privacy-preserving audience signals
- **Creative Rendering**: First-party creative delivery

## Expected Configuration

```toml
[integrations.gam]
enabled = true
publisher_id = "your-publisher-id"
network_code = "your-network-code"
endpoint = "https://securepubads.g.doubleclick.net"
```

## Use Cases

### Header Bidding + Direct

Combine header bidding (Prebid) with GAM direct campaigns for optimal yield.

### Programmatic Guaranteed

Manage PG deals with deterministic delivery.

### Unified Reporting

Consolidated reporting across all monetization channels.

## Status

This integration is currently in the planning phase. See the [Roadmap](/roadmap) for timeline and progress updates.

**Target Release**: Q1 2026

## Get Involved

Interested in contributing to the GAM integration? Check out the [GitHub issue](https://github.com/IABTechLab/trusted-server/issues) or join the discussion.

## Next Steps

- Review [Roadmap](/roadmap) for latest status
- Check [Prebid Integration](/guide/integrations/prebid) for current header bidding support
- Explore [Ad Serving Guide](/guide/ad-serving) for general ad serving concepts
