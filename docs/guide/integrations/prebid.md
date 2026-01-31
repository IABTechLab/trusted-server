# Prebid Integration

**Category**: Demand Wrapper
**Status**: Production
**Type**: Header Bidding

## Overview

The Prebid integration enables server-side header bidding through Prebid Server while maintaining first-party context and privacy compliance.

## What is Prebid?

Prebid is the leading open-source header bidding solution that allows publishers to offer ad inventory to multiple demand sources simultaneously, maximizing revenue through competition.

## Configuration

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "rubicon", "appnexus"]
auto_configure = true
debug = false
```

## Features

### Server-Side Header Bidding

Move header bidding to the server for:

- Faster page loads (reduce browser JavaScript)
- Better mobile performance
- Reduced client-side latency
- Improved user experience

### OpenRTB 2.x Support

Full OpenRTB protocol conversion:

- Converts ad units to OpenRTB `imp` objects
- Injects publisher domain and page URL
- Adds synthetic ID for privacy-safe tracking
- Supports banner, video, and native formats

### Synthetic ID Injection

Automatically injects privacy-preserving synthetic ID into bid requests for user recognition without cookies.

### Request Signing

Optional Ed25519 request signing for bid request authentication and fraud prevention.

## Endpoints

### GET /first-party/ad

Server-side ad rendering for single ad slot.

**Query Parameters**:

- `slot` - Ad unit code
- `w` - Width in pixels
- `h` - Height in pixels

**Response**: Complete HTML creative with first-party proxying.

### POST /third-party/ad

Client-side auction endpoint for TSJS library.

**Request Body**: Ad units configuration
**Response**: OpenRTB bid response with creatives

### GET /prebid.js (Optional)

Empty script override to prevent client-side Prebid.js loading when using server-side bidding.

## Use Cases

### Pure Server-Side Header Bidding

Replace client-side Prebid.js entirely with server-side auctions for maximum performance.

### Hybrid Client + Server

Use server-side for primary demand, client-side for niche bidders.

### Mobile-First Monetization

Optimize mobile ad serving with reduced JavaScript overhead.

## Implementation

See [crates/common/src/integrations/prebid.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/prebid.rs) for full implementation.

### OpenRTB Request Construction

Located in `build_openrtb_from_ts()` function (line 335):

- Converts ad units to impressions
- Adds site metadata
- Injects bidder parameters
- Generates unique request ID

## Best Practices

1. **Configure Timeouts**: Set `timeout_ms` based on your latency requirements
2. **Select Bidders**: Enable only bidders you have direct relationships with
3. **Monitor Performance**: Track bid response times and fill rates
4. **Test Thoroughly**: Validate bid requests in debug mode before production

## Next Steps

- Review [Ad Serving Guide](/guide/ad-serving) for general concepts
- Check [OpenRTB Support](/roadmap) on the roadmap for enhancements
- Explore [Request Signing](/guide/request-signing) for authentication
- Learn about [Synthetic IDs](/guide/synthetic-ids) for privacy-safe tracking
