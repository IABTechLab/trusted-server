# Google Tag Manager Integration

**Category**: Tag Management
**Status**: Production
**Type**: First-Party Tag Gateway

## Overview

The Google Tag Manager (GTM) integration enables Trusted Server to act as a first-party proxy for GTM scripts and analytics beacons. This improves performance, tracking accuracy, and privacy control by serving these assets from your own domain.

## What is the Tag Gateway?

The Tag Gateway intercepts requests for GTM scripts (`gtm.js`) and Google Analytics beacons (`collect`). Instead of the user's browser connecting directly to Google content servers, it connects to your Trusted Server. Trusted Server then fetches the content from Google and serves it back to the user.

**Benefits**:

- **Bypass Ad Blockers**: Serving scripts from a first-party domain can prevent them from being blocked by some ad blockers and privacy extensions.
- **Extended Cookie Life**: First-party cookies set by these scripts are more durable in environments like Safari (ITP).
- **Performance**: Utilize edge caching for scripts.
- **Privacy Control**: Strips client IP addresses before forwarding data to Google.

## Configuration

Add the GTM configuration to `trusted-server.toml`:

```toml
[integrations.google_tag_manager]
enabled = true
container_id = "GTM-XXXXXX"
# upstream_url = "https://www.googletagmanager.com" # Optional override
```

### Configuration Options

| Field          | Type    | Required | Description                                   |
| -------------- | ------- | -------- | --------------------------------------------- |
| `enabled`      | boolean | No       | Enable/disable integration (default: `false`) |
| `container_id` | string  | Yes      | Your GTM Container ID (e.g., `GTM-A1B2C3`)    |
| `upstream_url` | string  | No       | Custom upstream URL (advanced usage)          |

## How It Works

### 1. Script Rewriting

When Trusted Server processes an HTML response, it automatically rewrites GTM script tags:

**Before:**

```html
<script src="https://www.googletagmanager.com/gtm.js?id=GTM-XXXXXX"></script>
```

**After:**

```html
<script src="/integrations/google_tag_manager/gtm.js?id=GTM-XXXXXX"></script>
```

### 2. Script Proxying

When the browser requests `/integrations/google_tag_manager/gtm.js`:

1.  Trusted Server fetches the original script from Google.
2.  It modifies the script content on-the-fly to replace references to `www.google-analytics.com` and `www.googletagmanager.com` with the local proxy path.
3.  It serves the modified script to the browser.

### 3. Beacon Proxying

Analytics data sent by the modified script is directed to:
`/integrations/google_tag_manager/collect` (or `/g/collect`)

Trusted Server forwards these requests to Google's servers, ensuring the data is recorded successfully.

## Manual Verification

You can verify the integration using `curl`:

**Test Script Proxy:**

```bash
curl -v "http://your-server.com/integrations/google_tag_manager/gtm.js?id=GTM-XXXXXX"
```

_Expected_: 200 OK, and the body content should contain rewritten paths.

**Test Beacon:**

```bash
curl -v -X POST "http://your-server.com/integrations/google_tag_manager/g/collect?v=2&tid=G-XXXXXX..."
```

_Expected_: 200/204 OK.

## Implementation Details

See [crates/common/src/integrations/google_tag_manager.rs](https://github.com/IABTechLab/trusted-server/blob/main/crates/common/src/integrations/google_tag_manager.rs).

## Next Steps

- Review [Prebid Integration](/guide/integrations/prebid) for header bidding.
- Check [Configuration Guide](/guide/configuration) for other integration settings.
- Learn more about [Synthetic IDs](/guide/synthetic-ids) which are generated alongside this integration.
