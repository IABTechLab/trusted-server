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

::: warning Migration note
The Sourcepoint browser module is now opt-in through `[integrations.sourcepoint].enabled = true`. Existing deployments that relied on unconditional Sourcepoint JavaScript inclusion should enable this integration explicitly before upgrading.
:::

### Configuration Options

| Option              | Type             | Default                        | Description                                                                                                                                                                |
| ------------------- | ---------------- | ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`           | boolean          | `false`                        | Enable the Sourcepoint integration                                                                                                                                         |
| `rewrite_sdk`       | boolean          | `true`                         | Rewrite matching Sourcepoint URLs in HTML                                                                                                                                  |
| `cdn_origin`        | string           | `https://cdn.privacy-mgmt.com` | Sourcepoint CDN origin                                                                                                                                                     |
| `auth_cookie_name`  | string or `null` | `null`                         | Optional custom Sourcepoint `authCookie` name to forward upstream alongside built-in cookies. Names must be 1-64 characters and contain only letters, numbers, `_`, or `-` |
| `cache_ttl_seconds` | integer          | `3600`                         | Cache TTL applied to successful CDN responses when the origin omits cache headers                                                                                          |

## Endpoints

| Method                  | Path                              | Description                                   |
| ----------------------- | --------------------------------- | --------------------------------------------- |
| `GET/POST/HEAD/OPTIONS` | `/integrations/sourcepoint/cdn/*` | Proxy Sourcepoint CDN assets and wrapper APIs |

## HTML Rewriting

When `rewrite_sdk = true`, Trusted Server rewrites matching Sourcepoint URLs in HTML responses:

```html
<!-- Original -->
<script src="https://cdn.privacy-mgmt.com/wrapperMessagingWithoutDetection.js"></script>

<!-- Becomes -->
<script src="https://publisher.example.com/integrations/sourcepoint/cdn/wrapperMessagingWithoutDetection.js"></script>
```

If a publisher uses a Content Security Policy, `script-src` must allow the first-party Trusted Server host after rewriting. A policy that only allows `https://cdn.privacy-mgmt.com` can block the rewritten first-party script URLs.

## Response body rewriting

With `rewrite_sdk = true`, successful `GET` responses with status 200 and a JavaScript or HTML content type are eligible for body rewriting. JavaScript CDN URLs and privacy-manager HTML root-relative `src` / `href` assets are routed through the first-party proxy.

The input body limit is 5 MiB:

- Bodies at or below the limit are rewritten, including chunked or HTTP/2 responses without `Content-Length`.
- A declared `Content-Length` above the limit skips rewriting and leaves the body unchanged.
- If collection exceeds the limit, Trusted Server stops reading and returns `502 Bad Gateway`. This also applies when `Content-Length` understates the size. The partial body is discarded, not returned to the browser.
- Other content types, disabled rewriting, and ineligible methods or statuses do not collect the body for rewriting.

On Fastly, upstream responses remain streaming until the bounded collector reads them. It retains at most 5 MiB of input plus the current transport chunk, with additional bounded allocations for rewriting. Non-rewritten responses remain streaming. Adapters without streaming support still buffer upstream responses before this check; this limit is not an adapter-level memory guarantee on Cloudflare or Spin.

Likely JavaScript and HTML paths, including `/mms/v2/get_site_data`, request `Accept-Encoding: identity`. Rewritten bodies discard the upstream `Content-Length` and `Content-Encoding`, and remove `Accept-Encoding` from `Vary`. Non-UTF-8 bodies pass through unchanged after bounded collection.

## Runtime Config Rewriting

Trusted Server also injects a `window._sp_` property trap that rewrites known Sourcepoint URL-bearing config fields (`baseEndpoint`, `mmsDomain`, `wrapperAPIOrigin`, `cmpOrigin`, and `metricUrl`). If Sourcepoint introduces another URL-bearing field and third-party `cdn.privacy-mgmt.com` requests remain after enabling this integration, report the missed field so it can be added to the trap.

## Client-Side Guard

Single-page apps often insert CMP scripts after the initial HTML response. The `sourcepoint` tsjs module installs a DOM insertion guard so dynamically inserted Sourcepoint script and preload URLs are rewritten to first-party paths before the browser fetches them.

## Cookie Forwarding and Caching

Trusted Server forwards only Sourcepoint's documented cookie names upstream, plus the optional `auth_cookie_name` when configured. Unrelated publisher cookies are deliberately excluded so first-party application state is not leaked to Sourcepoint.

Responses that include `Set-Cookie` are forced to `Cache-Control: private, no-store` so cookie-bearing Sourcepoint traffic is never marked as publicly cacheable content by the proxy.

Rewritten `/mms/v2/get_site_data` responses preserve upstream cache policy rather than receiving the static JavaScript cache policy. When upstream omits `Cache-Control`, they use `private, max-age=0` if Sourcepoint cookies were forwarded, or the configured public TTL otherwise. Rewritten HTML uses the same policy. Other rewritten JavaScript retains the existing configured public TTL, except for responses that set cookies.

## Notes

- This version scopes the integration to `cdn.privacy-mgmt.com`. Additional Sourcepoint domains (e.g., `geo.privacymanager.io`) can be added later if publishers require them.

## See Also

- [Integration Guide](/guide/integration-guide)
- [Integrations Overview](/guide/integrations-overview)
