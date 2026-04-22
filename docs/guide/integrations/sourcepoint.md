# Sourcepoint Integration

Sourcepoint provides consent and privacy messaging for publishers. This integration proxies the Sourcepoint CDN endpoint through Trusted Server so the browser loads it from a first-party path.

## Overview

The Sourcepoint integration:

- Proxies `cdn.privacy-mgmt.com` requests through `/integrations/sourcepoint/cdn/*`
- Rewrites matching `src` and `href` attributes during HTML processing
- Rewrites JavaScript response bodies so webpack chunks and API calls route through the proxy
- Injects a `window._sp_` property trap for config URLs set by Next.js hydration chunks
- Installs a client-side script guard for dynamically inserted Sourcepoint assets

## Configuration

Add the following to `trusted-server.toml`:

```toml
[integrations.sourcepoint]
enabled = true
rewrite_sdk = true
cdn_origin = "https://cdn.privacy-mgmt.com"
# Optional: forward a custom Sourcepoint authCookie name upstream.
# auth_cookie_name = "sp_auth"
cache_ttl_seconds = 3600
```

### Configuration Options

| Option              | Type             | Default                        | Description                                                                                  |
| ------------------- | ---------------- | ------------------------------ | -------------------------------------------------------------------------------------------- |
| `enabled`           | boolean          | `false`                        | Enable the Sourcepoint integration                                                           |
| `rewrite_sdk`       | boolean          | `true`                         | Rewrite matching Sourcepoint URLs in HTML                                                    |
| `cdn_origin`        | string           | `https://cdn.privacy-mgmt.com` | Sourcepoint CDN origin                                                                       |
| `auth_cookie_name`  | string or `null` | `null`                         | Optional custom Sourcepoint `authCookie` name to forward upstream alongside built-in cookies |
| `cache_ttl_seconds` | integer          | `3600`                         | Cache TTL applied to successful CDN responses when the origin omits cache headers            |

## Endpoints

| Method     | Path                              | Description                                   |
| ---------- | --------------------------------- | --------------------------------------------- |
| `GET/POST` | `/integrations/sourcepoint/cdn/*` | Proxy Sourcepoint CDN assets and wrapper APIs |

## HTML Rewriting

When `rewrite_sdk = true`, Trusted Server rewrites matching Sourcepoint URLs in HTML responses:

```html
<!-- Original -->
<script src="https://cdn.privacy-mgmt.com/wrapperMessagingWithoutDetection.js"></script>

<!-- Becomes -->
<script src="https://publisher.example.com/integrations/sourcepoint/cdn/wrapperMessagingWithoutDetection.js"></script>
```

## Client-Side Guard

Single-page apps often insert CMP scripts after the initial HTML response. The `sourcepoint` tsjs module installs a DOM insertion guard so dynamically inserted Sourcepoint script and preload URLs are rewritten to first-party paths before the browser fetches them.

## Cookie Forwarding and Caching

Trusted Server forwards only Sourcepoint's documented cookie names upstream, plus the optional `auth_cookie_name` when configured. Unrelated publisher cookies are deliberately excluded so first-party application state is not leaked to Sourcepoint.

Responses that include `Set-Cookie` are forced to `Cache-Control: private, no-store` so cookie-bearing Sourcepoint traffic is never marked as publicly cacheable content by the proxy.

## Notes

- This version scopes the integration to `cdn.privacy-mgmt.com`. Additional Sourcepoint domains (e.g., `geo.privacymanager.io`) can be added later if publishers require them.

## See Also

- [Integration Guide](/guide/integration-guide)
- [Integrations Overview](/guide/integrations-overview)
