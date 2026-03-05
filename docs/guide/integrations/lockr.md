# Lockr Integration

**Category**: Identity / Data Collection
**Status**: Production
**Type**: Proxy + Attribute Rewriter

## Overview

The Lockr integration serves Lockr's SDK and API through first-party routes. It has two main goals:

1. Rewrite Lockr SDK URLs in HTML so browsers load the SDK from your domain.
2. Proxy Lockr API calls through Trusted Server instead of calling Lockr directly from the page.

This integration currently focuses on transport and routing. It does **not** inject OpenRTB EIDs or run a dedicated identity-sync endpoint.

## Configuration

```toml
[integrations.lockr]
enabled = true
app_id = "your-lockr-app-id"

# Optional overrides
api_endpoint = "https://identity.lockr.kr"
sdk_url = "https://aim.loc.kr/identity-lockr-v1.0.js"
cache_ttl_seconds = 3600
rewrite_sdk = true
rewrite_sdk_host = true
# origin_override = "https://www.example.com"
```

### Configuration Options

| Field               | Type    | Default                                     | Description                                            |
| ------------------- | ------- | ------------------------------------------- | ------------------------------------------------------ |
| `enabled`           | boolean | `true`                                      | Enable/disable the integration                         |
| `app_id`            | string  | Required                                    | Lockr app identifier (required by config validation)   |
| `api_endpoint`      | string  | `https://identity.lockr.kr`                 | Upstream Lockr API base URL                            |
| `sdk_url`           | string  | `https://aim.loc.kr/identity-lockr-v1.0.js` | Upstream Lockr SDK URL                                 |
| `cache_ttl_seconds` | integer | `3600`                                      | Cache TTL for proxied SDK response                     |
| `rewrite_sdk`       | boolean | `true`                                      | Rewrite Lockr SDK script URLs in HTML                  |
| `rewrite_sdk_host`  | boolean | `true`                                      | Rewrite obfuscated host assignment inside the SDK body |
| `origin_override`   | string  | `None`                                      | Optional `Origin` override forwarded to Lockr API      |

## Routes

When enabled, Lockr registers these first-party routes:

- `GET /integrations/lockr/sdk`
  - Fetches the SDK from `sdk_url`
  - Optionally rewrites the SDK host assignment to `/integrations/lockr/api`
  - Returns JavaScript with cache headers

- `GET /integrations/lockr/api/*`
- `POST /integrations/lockr/api/*`
  - Proxies requests to `api_endpoint` with the same path suffix and query string
  - Forwards common request headers and custom `X-*` headers (excluding TS-internal headers)

## HTML Rewriting

If `rewrite_sdk = true`, the integration rewrites Lockr SDK URLs in `src` and `href` attributes to:

`https://{request_host}/integrations/lockr/sdk`

This allows pages to load Lockr from your first-party domain.

## SDK Host Rewriting

If `rewrite_sdk_host = true`, Trusted Server rewrites the obfuscated host expression inside the SDK body to:

`'host': '/integrations/lockr/api'`

That keeps SDK API traffic on the first-party route automatically.

## Example Flow

1. Page includes Lockr SDK URL.
2. HTML rewriter swaps it to `/integrations/lockr/sdk`.
3. Browser requests first-party SDK endpoint.
4. Trusted Server fetches upstream SDK, optionally rewrites host, and serves it.
5. SDK API calls go to `/integrations/lockr/api/*`.
6. Trusted Server proxies those API requests to Lockr upstream.

## Troubleshooting

### SDK URL not rewritten

- Ensure `rewrite_sdk = true`.
- Confirm the page uses a Lockr SDK URL pattern the integration recognizes.
- Verify the Lockr integration is enabled.

### API requests do not reach Lockr

- Confirm calls target `/integrations/lockr/api/*`.
- Verify `api_endpoint` is reachable from the edge.
- If Lockr rejects origin, set `origin_override`.

### SDK loads but still calls third-party host

- Ensure `rewrite_sdk_host = true`.
- Check response header `X-Lockr-Host-Rewritten` on `/integrations/lockr/sdk`.

## Implementation

See `crates/common/src/integrations/lockr.rs` for the full implementation.
