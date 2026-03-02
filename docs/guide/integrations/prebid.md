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
debug = false
# test_mode = false

# Script interception patterns (optional - defaults shown below)
script_patterns = ["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]

# Optional per-bidder, per-zone param overrides (shallow merge)
[integrations.prebid.bid_param_zone_overrides.kargo]
header       = {placementId = "_s2sHeaderPlacement"}
in_content   = {placementId = "_s2sContentPlacement"}
```

### Configuration Options

| Field                      | Type          | Default                                                                | Description                                                                                                                                      |
| -------------------------- | ------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `enabled`                  | Boolean       | `true`                                                                 | Enable Prebid integration                                                                                                                        |
| `server_url`               | String        | Required                                                               | Prebid Server endpoint URL                                                                                                                       |
| `timeout_ms`               | Integer       | `1000`                                                                 | Request timeout in milliseconds                                                                                                                  |
| `bidders`                  | Array[String] | `["mocktioneer"]`                                                      | List of enabled bidders                                                                                                                          |
| `bid_param_zone_overrides` | Table         | `{}`                                                                   | Per-bidder, per-zone param overrides; zone values are shallow-merged into bidder params                                                          |
| `debug`                    | Boolean       | `false`                                                                | Enable Prebid debug mode (sets `ext.prebid.debug` and `ext.prebid.returnallbidstatus`; surfaces debug metadata in auction responses)             |
| `test_mode`                | Boolean       | `false`                                                                | Set the OpenRTB `test: 1` flag so bidders treat the auction as non-billable test traffic. Separate from `debug` to avoid suppressing real demand |
| `debug_query_params`       | String        | `None`                                                                 | Extra query params appended for debugging                                                                                                        |
| `script_patterns`          | Array[String] | `["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]` | URL patterns for Prebid script interception                                                                                                      |

## Debug Mode

When `debug = true`, the Prebid integration enables additional diagnostics on both the outgoing OpenRTB request and the incoming response.

### Outgoing request flags

| OpenRTB field                   | Value  | Purpose                                                                                             |
| ------------------------------- | ------ | --------------------------------------------------------------------------------------------------- |
| `ext.prebid.debug`              | `true` | Tells Prebid Server to include `ext.debug` in the response (httpcalls, resolvedrequest)             |
| `ext.prebid.returnallbidstatus` | `true` | Asks Prebid Server to return per-bid status for every bidder, including those that returned no bids |

### Response metadata enrichment

The Prebid provider extracts metadata from the Prebid Server response and attaches it to the `AuctionResponse.metadata` map:

**Always-on fields** (present regardless of `debug`):

| Key                  | Source                   | Description                    |
| -------------------- | ------------------------ | ------------------------------ |
| `responsetimemillis` | `ext.responsetimemillis` | Per-bidder response times (ms) |
| `errors`             | `ext.errors`             | Per-bidder error diagnostics   |
| `warnings`           | `ext.warnings`           | Per-bidder warning diagnostics |

**Debug-only fields** (only when `debug = true`):

| Key         | Source                 | Description                                              |
| ----------- | ---------------------- | -------------------------------------------------------- |
| `debug`     | `ext.debug`            | Prebid Server debug payload (httpcalls, resolvedrequest) |
| `bidstatus` | `ext.prebid.bidstatus` | Per-bid status from every invited bidder                 |

::: warning
Enabling `debug` increases response sizes and adds overhead. Use it in development or when diagnosing auction issues — not in production.
:::

### Test mode vs. debug

`test_mode` and `debug` are independent flags:

- **`debug`** — Enables diagnostic data without affecting bidder behavior. Bidders still treat the auction as live.
- **`test_mode`** — Sets the top-level OpenRTB `test: 1` flag. Bidders treat the request as non-billable test traffic, which can significantly reduce fill rates.

You can combine both to get debug diagnostics on test traffic, or use `debug` alone to inspect live auctions without affecting revenue.

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

### Script Interception

The `script_patterns` configuration controls which Prebid scripts are intercepted and replaced with empty JavaScript. This enables server-side bidding by preventing client-side Prebid.js from loading.

**Pattern Matching**:

- **Suffix matching**: `/prebid.min.js` matches any URL ending with that path
- **Wildcard patterns**: `/static/prebid/*` matches paths under that prefix (filtered by known Prebid script suffixes)
- **Case-insensitive**: All patterns are matched case-insensitively

**Examples**:

```toml
# Default patterns (intercept common Prebid scripts)
script_patterns = ["/prebid.js", "/prebid.min.js"]

# Custom CDN path with wildcard
script_patterns = ["/static/prebid/*", "/assets/js/prebid.min.js"]

# Disable script interception (keep client-side Prebid)
script_patterns = []
```

When a request matches a script pattern, Trusted Server returns an empty JavaScript file with aggressive caching (`max-age=31536000, immutable`).

### Bid Param Zone Overrides

Use `bid_param_zone_overrides` for per-zone, per-bidder param overrides. This is designed for bidders like Kargo that use different server-to-server placement IDs per ad zone.

The JS adapter reads the zone from `mediaTypes.banner.name` on each Prebid ad unit (e.g., `"header"`, `"in_content"`, `"fixed_bottom"`) and sends it alongside the bidder params. The server then uses this zone to look up the correct override. When `mediaTypes.banner.name` is not set, no zone is sent and zone overrides are skipped for that impression.

**Behavior**:

- Overrides are matched by bidder name + zone combination
- Override params are shallow-merged into incoming bidder params (override values win on key conflicts)
- Non-conflicting incoming fields are preserved
- When no zone override matches (unknown zone or missing zone), incoming params are left unchanged

**Example**:

```toml
[integrations.prebid.bid_param_zone_overrides.kargo]
header       = {placementId = "_s2sHeaderPlacement"}
in_content   = {placementId = "_s2sContentPlacement"}
fixed_bottom = {placementId = "_s2sBottomPlacement"}
```

If the incoming request for zone `header` has:

```json
{ "kargo": { "placementId": "client_side_abc" } }
```

the outgoing bidder params become:

```json
{ "kargo": { "placementId": "_s2sHeaderPlacement" } }
```

For an unrecognised zone (e.g., `sidebar`), the incoming params are left unchanged.

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

### GET `<script_patterns>` (Dynamic)

Routes are registered dynamically based on the `script_patterns` configuration. Each pattern creates an endpoint that returns an empty JavaScript file to prevent client-side Prebid.js loading.

Default registered routes:

- `GET /prebid.js`
- `GET /prebid.min.js`
- `GET /prebidjs.js`
- `GET /prebidjs.min.js`

Set `script_patterns = []` to disable these routes entirely.

## Use Cases

### Pure Server-Side Header Bidding

Replace client-side Prebid.js entirely with server-side auctions for maximum performance.

### Hybrid Client + Server

Use server-side for primary demand, client-side for niche bidders.

### Mobile-First Monetization

Optimize mobile ad serving with reduced JavaScript overhead.

## Implementation

See [crates/common/src/integrations/prebid.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/prebid.rs) for full implementation.

### Key Components

- **`PrebidIntegration`**: Handles script interception and HTML attribute rewriting to remove Prebid script references
- **`PrebidAuctionProvider`**: Implements the `AuctionProvider` trait for the auction orchestrator

### OpenRTB Request Construction

The `to_openrtb()` method in `PrebidAuctionProvider` builds OpenRTB requests:

- Converts ad slots to OpenRTB `imp` objects with bidder params
- Adds site metadata with publisher domain and page URL
- Injects synthetic ID in the user object
- Includes device info (user-agent, client IP) and geo when available
- Sets `ext.prebid.debug` and `ext.prebid.returnallbidstatus` when `debug` is enabled
- Sets the top-level `test: 1` flag when `test_mode` is enabled
- Appends `debug_query_params` to page URL when configured
- Applies `bid_param_zone_overrides` to `imp.ext.prebid.bidder` before request dispatch
- Signs requests when request signing is enabled

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
