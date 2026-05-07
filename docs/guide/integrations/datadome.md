# DataDome Integration

DataDome provides bot protection and fraud prevention for websites. This integration enables first-party delivery of DataDome's JavaScript tag and signal collection through Trusted Server, eliminating the need for DNS/CNAME configuration.

## Overview

The DataDome integration:

- Proxies `tags.js` SDK through your first-party domain
- Rewrites internal DataDome URLs to route through Trusted Server
- Proxies signal collection API (`/js/*`) through first-party context
- Automatically rewrites `<script>` tags in HTML responses

## Benefits

| Traditional Setup          | Trusted Server Approach             |
| -------------------------- | ----------------------------------- |
| Requires DNS CNAME changes | No DNS changes needed               |
| Separate subdomain setup   | Uses existing publisher domain      |
| Direct browser-to-DataDome | All traffic through publisher edge  |
| Ad blockers may interfere  | First-party context avoids blocking |

## Configuration

Add the following to your `trusted-server.toml`:

```toml
[integrations.datadome]
enabled = true
sdk_origin = "https://js.datadome.co"        # SDK script origin (tags.js)
api_origin = "https://api-js.datadome.co"    # Signal collection API origin (/js/*)
cache_ttl_seconds = 3600
rewrite_sdk = true
```

### Configuration Options

| Option              | Type    | Default                      | Description                                               |
| ------------------- | ------- | ---------------------------- | --------------------------------------------------------- |
| `enabled`           | boolean | `false`                      | Enable the DataDome integration                           |
| `sdk_origin`        | string  | `https://js.datadome.co`     | DataDome SDK origin URL (for tags.js)                     |
| `api_origin`        | string  | `https://api-js.datadome.co` | DataDome signal collection API origin URL (for /js/\*)    |
| `cache_ttl_seconds` | integer | `3600`                       | Cache TTL for tags.js (1 hour default)                    |
| `rewrite_sdk`       | boolean | `true`                       | Rewrite DataDome script URLs in HTML to first-party paths |

## Usage

### Publisher Page Setup

Update your page to load DataDome through Trusted Server:

```html
<script>
  window.ddjskey = 'YOUR_DATADOME_JS_KEY'
  window.ddoptions = {}
</script>
<script src="/integrations/datadome/tags.js" async></script>
```

If `rewrite_sdk` is enabled, Trusted Server will automatically rewrite any existing DataDome script tags in your HTML:

```html
<!-- Original -->
<script src="https://js.datadome.co/tags.js" async></script>

<!-- Becomes -->
<script
  src="https://your-domain.com/integrations/datadome/tags.js"
  async
></script>
```

## Endpoints

The integration exposes the following routes:

| Method     | Path                             | Description           |
| ---------- | -------------------------------- | --------------------- |
| `GET`      | `/integrations/datadome/tags.js` | DataDome SDK script   |
| `GET/POST` | `/integrations/datadome/js/*`    | Signal collection API |

## How It Works

```mermaid
sequenceDiagram
    participant Browser
    participant TS as Trusted Server
    participant SDK as js.datadome.co
    participant API as api-js.datadome.co

    Browser->>TS: GET /integrations/datadome/tags.js
    TS->>SDK: GET /tags.js
    SDK-->>TS: JavaScript SDK
    Note over TS: Rewrite internal URLs
    TS-->>Browser: Modified SDK (first-party URLs)

    Browser->>TS: POST /integrations/datadome/js/
    TS->>API: POST /js/
    API-->>TS: Response
    TS-->>Browser: Response
```

### Request Flow

1. **SDK Loading**: Browser requests `/integrations/datadome/tags.js`
2. **Proxy & Rewrite**: Trusted Server fetches from `js.datadome.co`, rewrites internal URLs to first-party paths
3. **Signal Collection**: SDK sends signals to `/integrations/datadome/js/`
4. **Transparent Proxy**: Trusted Server forwards to `api-js.datadome.co`, returns response

## Environment Variables

Override configuration via environment variables:

```bash
TRUSTED_SERVER__INTEGRATIONS__DATADOME__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__DATADOME__SDK_ORIGIN=https://js.datadome.co
TRUSTED_SERVER__INTEGRATIONS__DATADOME__API_ORIGIN=https://api-js.datadome.co
TRUSTED_SERVER__INTEGRATIONS__DATADOME__CACHE_TTL_SECONDS=3600
TRUSTED_SERVER__INTEGRATIONS__DATADOME__REWRITE_SDK=true
```

## Client-Side Script Guard

For single-page applications (SPAs) and frameworks like Next.js that dynamically insert script tags, the integration includes a client-side guard. When the `datadome` module is included in your tsjs bundle, it automatically intercepts dynamically inserted DataDome scripts and rewrites them to use first-party paths.

The guard handles:

- `<script src="js.datadome.co/...">` elements
- `<link rel="preload" as="script" href="js.datadome.co/...">` elements
- `<link rel="prefetch" as="script" href="js.datadome.co/...">` elements

This ensures DataDome scripts are always loaded through first-party context, even when inserted dynamically by client-side JavaScript.

## Notes

- **No Captcha Support**: This integration currently focuses on signal collection. CAPTCHA functionality may require additional configuration.
- **Cache Headers**: The SDK response includes caching headers based on `cache_ttl_seconds`.
- **Origin Headers**: Trusted Server forwards appropriate headers to DataDome for proper request context.
- **URL Rewriting**: Both `js.datadome.co` and `api-js.datadome.co` URLs in the SDK are rewritten to first-party paths.

## Troubleshooting

### Script Not Loading

Check that the integration is enabled:

```toml
[integrations.datadome]
enabled = true
```

### Signals Not Sending

Verify that signal collection routes are working:

```bash
curl -X POST https://your-domain.com/integrations/datadome/js/check
```

### HTML Rewriting Not Working

Ensure `rewrite_sdk = true` and that your pages are being proxied through Trusted Server's HTML processing pipeline.

## Server-Side Validation

In addition to first-party JS delivery, Trusted Server can call the DataDome
server-side API to validate each request at the edge before forwarding it to
your origin. Bots are blocked with a `403 Forbidden` response without ever
reaching your backend.

### How It Works

```mermaid
sequenceDiagram
    participant Browser
    participant TS as Trusted Server
    participant DD as api-fastly.datadome.co
    participant Origin

    Browser->>TS: GET /article
    TS->>DD: POST /validate-request\n(IP, path, headers, cookie)
    DD-->>TS: 200 OK (allow) or 403 (block)
    alt allowed
        TS->>Origin: Proxy request
        Origin-->>TS: Response
        TS-->>Browser: Response
    else blocked
        TS-->>Browser: 403 Forbidden
    end
```

Validation runs before the request reaches your origin. The DataDome cookie
(`datadome=`) is forwarded when present so DataDome can maintain session
continuity for users it has already classified.

### Configuration

```toml
[integrations.datadome]
enabled = true
server_side_enabled = true
server_side_key = "your-datadome-server-side-key"
```

Set `server_side_key` via environment variable to keep it out of
`trusted-server.toml`:

```bash
TRUSTED_SERVER__INTEGRATIONS__DATADOME__SERVER_SIDE_KEY=your-key
```

### Configuration Options

| Option                  | Type    | Default                          | Description                                                          |
| ----------------------- | ------- | -------------------------------- | -------------------------------------------------------------------- |
| `server_side_enabled`   | boolean | `false`                          | Enable server-side validation                                        |
| `server_side_key`       | string  | —                                | DataDome server-side API key (required when enabled)                 |
| `validation_endpoint`   | string  | `https://api-fastly.datadome.co` | DataDome validation API base URL                                     |
| `validation_timeout_ms` | integer | `200`                            | Timeout for the validation request (50–1000 ms)                      |
| `fail_open`             | boolean | `true`                           | Allow the request if validation times out or errors                  |
| `sample_rate`           | integer | `100`                            | Percentage of requests to validate (0–100). Use for gradual rollout. |

### Gradual Rollout

Use `sample_rate` to enable validation for a fraction of traffic while you
gain confidence:

```toml
[integrations.datadome]
server_side_enabled = true
server_side_key = "your-key"
sample_rate = 10   # validate 10% of requests
```

Increase toward 100 once you're satisfied with the block rate and latency
impact. Sampling is IP-stable — a given IP address consistently falls in or out
of the sampled set across requests.

### Fail-Open vs Fail-Closed

`fail_open = true` (the default) means any DataDome API error or timeout
results in the request being allowed through. This keeps your site available
even if DataDome is unreachable.

`fail_open = false` blocks requests whenever validation cannot be completed.
Only use this after validating DataDome uptime in your region and at your
traffic volume.

## See Also

- [DataDome First-Party Integration Docs](https://docs.datadome.co/docs/integrations#first-party-javascript-tag)
- [Integrations Overview](/guide/integrations-overview)
- [First-Party Proxy](/guide/first-party-proxy)
