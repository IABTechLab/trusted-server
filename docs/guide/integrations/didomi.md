# Didomi Integration

**Category**: Consent Management Platform

**Status**: Production

**Type**: First-party SDK and API reverse proxy

## Overview

The Didomi integration serves Didomi SDK assets and API calls through the
publisher's Trusted Server domain. It proxies SDK requests to
`sdk.privacy-center.org`, routes `/api/*` requests to `api.privacy-center.org`,
and injects the configured first-party SDK path into the browser integration.

Didomi generates notice loaders according to the visitor's country and region.
On Fastly, Trusted Server can put its trusted platform geo in the loader URL so
the browser, Fastly cache, and Didomi origin all identify the same geographic
variant.

See [Didomi's reverse-proxy requirements](https://developers.didomi.io/api-and-platform/domains/reverse-proxy)
for the upstream contract.

## Configuration

Add the integration to the operator-owned `trusted-server.toml`:

```toml
[integrations.didomi]
enabled = true
geo_query_parameters = true
# proxy_path = "my-custom-consent"
# sdk_origin = "https://sdk.privacy-center.org"
# api_origin = "https://api.privacy-center.org"
```

Publish application configuration with:

```bash
ts config push --adapter fastly
```

| Field                  | Type    | Required | Default                               | Description                                             |
| ---------------------- | ------- | -------- | ------------------------------------- | ------------------------------------------------------- |
| `enabled`              | boolean | No       | `true` in a present integration block | Enables the integration                                 |
| `geo_query_parameters` | boolean | No       | `false`                               | Enables trusted geo canonicalization for notice loaders |
| `proxy_path`           | string  | No       | `integrations/didomi/consent`         | Changes the first-party path prefix                     |
| `sdk_origin`           | string  | No       | `https://sdk.privacy-center.org`      | Changes the SDK origin, primarily for testing           |
| `api_origin`           | string  | No       | `https://api.privacy-center.org`      | Changes the API origin, primarily for testing           |

`geo_query_parameters` is disabled by default for compatibility. It currently
supports Fastly only because Cloudflare does not expose a trusted region through
the pinned EdgeZero adapter, while Axum and Spin do not provide platform geo.

The normal configuration source is TOML. `TRUSTED_SERVER__...` variables are
optional overlays applied by `ts config validate` and `ts config push`; they are
not read by a running deployment. An overlay can replace only a scalar leaf that
already exists in the TOML input.

### Custom proxy path

`proxy_path` helps avoid a predictable integration path:

```toml
[integrations.didomi]
enabled = true
geo_query_parameters = true
proxy_path = "my-custom-consent"
```

This serves Didomi at `/my-custom-consent/*`. The path:

- must not be empty, root-only, or end in `/`;
- may contain ASCII letters, numbers, `-`, `_`, `.`, `~`, and `/` separators;
- must not contain `//`, percent escapes, or `.` and `..` path segments; and
- may start with `/`; Trusted Server normalizes the leading slash.

Trusted Server passes the resolved path to its browser bundle through
`window.__tsjs_didomi.proxyPath`.

## Notice-loader geo flow

Geo handling applies only when `geo_query_parameters = true` and the request is
a `GET` whose path after the proxy prefix is exactly
`/<public-api-key>/loader.js`.

For example, a California request to:

```text
/integrations/didomi/consent/example-key/loader.js?target_type=notice&target=example-notice
```

receives a `307 Temporary Redirect` to:

```text
/integrations/didomi/consent/example-key/loader.js?target_type=notice&target=example-notice&country=US&region=CA
```

The redirect uses a relative same-origin `Location` and is private and
non-storable. Trusted Server does not contact Didomi for that request. When the
browser follows the canonical URL, Trusted Server proxies the same path and query
to:

```text
https://sdk.privacy-center.org/example-key/loader.js?target_type=notice&target=example-notice&country=US&region=CA
```

The final browser URL includes geo, which prevents a loader cached in one
location from being reused under the same geo-less browser URL after the visitor
moves or changes network location.

### Trust and precedence

Trusted Server obtains geo from `RuntimeServices.geo()` using the trusted client
IP. Browser query parameters and request headers are not geo authorities.

For eligible loader URLs, Trusted Server:

- trims ASCII whitespace and uppercases platform country and region;
- accepts a two-letter ASCII country other than `XX` or `ZZ`;
- accepts a one-to-three-character ASCII alphanumeric subdivision;
- converts a matching country-prefixed value such as `US-CA` to `CA`;
- removes every case-insensitive, URL-decoded `country` and `region` query pair;
  and
- appends one authoritative `country` followed by one `region`.

The canonical upstream request also sets `X-Geo-Country`, `X-Geo-Region`, and
`CloudFront-Viewer-Country` from that same normalized pair. Conflicting caller
query parameters and Fastly-style geo headers cannot override it.

When geo lookup fails or returns missing or invalid country/region data, the
eligible loader returns a private, non-storable `503 Service Unavailable` without
contacting Didomi. This includes locations for which Fastly supplies no region.
Publishers should verify geo coverage before enabling the option globally.

Other SDK assets and all behavior with `geo_query_parameters = false` retain the
existing proxy flow.

## Endpoints and caching

### SDK

All paths under the proxy prefix other than `/api/*` use the SDK origin. Trusted
Server forwards the incoming SDK path and query, except for the authoritative
notice-loader canonicalization described above.

SDK responses keep Didomi's `Cache-Control`, `Expires`, validators, age, and CDN
cache headers. Shared caches must include the full path and query in their cache
key and honor the geo-less redirect's `private, no-store` policy. Trusted Server
does not replace Didomi's freshness policy with a hard-coded TTL.

### API

Paths under `<proxy-prefix>/api/*` use the API origin. Country and region are not
appended to API URLs. The incoming API path, query, supported method, headers, and
body continue through the proxy.

Every API request bypasses the platform outbound cache. Every API response is
returned with `Cache-Control: private, no-store`; freshness validators and
independent edge-cache headers are removed.

## Forwarded data

Trusted Server forwards selected HTTP headers needed by Didomi, including
`Accept`, `Accept-Language`, `Accept-Encoding`, `Content-Type`, `User-Agent`,
`Referer`, and `Origin`. It derives `X-Forwarded-For` from trusted client info.

Cookies and the publisher's `Authorization` header are not forwarded to Didomi.
The latter can contain publisher-site credentials and is not a Didomi API
credential.

SDK responses receive these CORS headers:

```http
Access-Control-Allow-Origin: *
Access-Control-Allow-Headers: Content-Type, Authorization, X-Requested-With
Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS
```

## Rollout checks

Before enabling `geo_query_parameters`:

1. Confirm the loader redirect remains same-origin and is allowed by the site's
   Content Security Policy.
2. Confirm Fastly returns complete country and region values for the publisher's
   supported traffic.
3. Confirm every shared cache keys SDK objects by the complete path and query.
4. Test at least two locations that require different notices.
5. Verify repeated requests for one country/region reuse the cached SDK response
   and retain Didomi's freshness and validator headers.
6. Verify API requests never enter the outbound or downstream cache.
7. Purge loader and API entries cached before this behavior was enabled.

Do not enable `geo_query_parameters` on Cloudflare, Axum, or Spin until those
adapters provide complete trusted geo and their support is documented.

## Troubleshooting

If an enabled loader returns `503`, verify that Fastly resolved both a valid
country and region for the trusted client IP. A country-only result is deliberately
rejected.

If the browser redirects repeatedly, inspect the full query at every cache layer.
A component that drops, reorders, or rewrites the authoritative parameters can
prevent the URL from reaching its canonical form.

If the wrong notice appears, verify that the final browser URL contains the
expected pair and that the cache key includes the full query. Purge stale loader
entries after correcting cache configuration.

If consent events fail, verify `/api/*` routing and `api_origin`. Publisher basic
authentication is intentionally removed before the request reaches Didomi.
