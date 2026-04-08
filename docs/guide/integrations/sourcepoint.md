# Sourcepoint Integration

Sourcepoint provides consent and privacy messaging for publishers. This integration proxies the Sourcepoint CDN and geo endpoints through Trusted Server so the browser loads them from first-party paths.

## Overview

The Sourcepoint integration:

- Proxies `cdn.privacy-mgmt.com` requests through `/integrations/sourcepoint/cdn/*`
- Proxies `geo.privacymanager.io` requests through `/integrations/sourcepoint/geo/*`
- Rewrites matching `src` and `href` attributes during HTML processing
- Installs a client-side script guard for dynamically inserted Sourcepoint assets

## Configuration

Add the following to `trusted-server.toml`:

```toml
[integrations.sourcepoint]
enabled = true
rewrite_sdk = true
cdn_origin = "https://cdn.privacy-mgmt.com"
geo_origin = "https://geo.privacymanager.io"
cache_ttl_seconds = 3600
```

### Configuration Options

| Option              | Type    | Default                         | Description                                                                       |
| ------------------- | ------- | ------------------------------- | --------------------------------------------------------------------------------- |
| `enabled`           | boolean | `false`                         | Enable the Sourcepoint integration                                                |
| `rewrite_sdk`       | boolean | `true`                          | Rewrite matching Sourcepoint URLs in HTML                                         |
| `cdn_origin`        | string  | `https://cdn.privacy-mgmt.com`  | Sourcepoint CDN origin                                                            |
| `geo_origin`        | string  | `https://geo.privacymanager.io` | Sourcepoint geo origin                                                            |
| `cache_ttl_seconds` | integer | `3600`                          | Cache TTL applied to successful CDN responses when the origin omits cache headers |

## Endpoints

| Method     | Path                                                                  | Description                                   |
| ---------- | --------------------------------------------------------------------- | --------------------------------------------- |
| `GET/POST` | `/integrations/sourcepoint/cdn/*`                                     | Proxy Sourcepoint CDN assets and wrapper APIs |
| `GET`      | `/integrations/sourcepoint/geo` and `/integrations/sourcepoint/geo/*` | Proxy Sourcepoint geo lookups                 |

## HTML Rewriting

When `rewrite_sdk = true`, Trusted Server rewrites matching Sourcepoint URLs in HTML responses:

```html
<!-- Original -->
<script src="https://cdn.privacy-mgmt.com/wrapperMessagingWithoutDetection.js"></script>

<!-- Becomes -->
<script src="https://publisher.example.com/integrations/sourcepoint/cdn/wrapperMessagingWithoutDetection.js"></script>
```

Geo lookups are rewritten the same way:

```text
https://geo.privacymanager.io/
-> https://publisher.example.com/integrations/sourcepoint/geo/
```

## Client-Side Guard

Single-page apps often insert CMP scripts after the initial HTML response. The `sourcepoint` tsjs module installs a DOM insertion guard so dynamically inserted Sourcepoint script and preload URLs are rewritten to first-party paths before the browser fetches them.

## Notes

- This first version intentionally scopes the integration to the Sourcepoint hosts observed on Autoblog: `cdn.privacy-mgmt.com` and `geo.privacymanager.io`.
- Adjacent privacy vendors and related endpoints can be added later without changing the integration shape.

## See Also

- [Integration Guide](/guide/integration-guide)
- [Integrations Overview](/guide/integrations-overview)
