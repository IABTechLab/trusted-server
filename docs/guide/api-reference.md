# API Reference

Quick reference for all Trusted Server HTTP endpoints.

## Endpoint Categories

- [First-Party Endpoints](#first-party-endpoints) - Core ad serving and proxying
- [Edge Cookie Endpoints](#edge-cookie-endpoints) - Identity sync and enrichment
- [Request Signing](#request-signing-endpoints) - Cryptographic signing and key management
- [Admin Diagnostics](#admin-diagnostic-endpoints) - Protected EC troubleshooting
- [TSJS Library](#tsjs-library-endpoint) - JavaScript library serving
- [Utility Endpoints](#utility-endpoints) - Optional operational helpers
- [Integration Endpoints](#integration-endpoints) - Third-party service proxying

---

## Adapter and startup support

| Adapter      | Release status | Health     | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling     | Request normalization            |
| ------------ | -------------- | ---------- | -------------- | -------------- | ---------------- | ------------------------------ | -------------------------------- |
| `axum`       | development    | real       | `500`          | no             | multiple         | outermost sanitize             | none                             |
| `cloudflare` | development    | absent     | `500`          | no             | single           | outermost sanitize             | none                             |
| `fastly`     | production     | pre router | `500`          | yes            | multiple         | entry-point resolve + sanitize | none                             |
| `spin`       | experimental   | real       | `503`          | yes            | single           | outermost sanitize             | innermost Spin-header derivation |

`trusted client IP handling` describes the optional
`[trusted_client_ip]` configuration. Fastly resolves the authenticated header
at the entry point and sanitizes all forwarding headers. The other adapters
install the sanitizer as the outermost router middleware but do not resolve
that configuration. Spin additionally normalizes its runtime-provided
authority, scheme, and client-address headers in the innermost middleware.

## Route availability

| Router  | Path                                   | Methods                                                    | Shape          | Predicate                             | Fastly                | Axum                  | Cloudflare            | Spin                  |
| ------- | -------------------------------------- | ---------------------------------------------------------- | -------------- | ------------------------------------- | --------------------- | --------------------- | --------------------- | --------------------- |
| normal  | `/.well-known/trusted-server.json`     | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/__ts/page-bids`                      | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/__ts/page-bids`                      | `OPTIONS`                                                  | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/_ts/admin/ec/{id}`                   | `GET`                                                      | template       | `always`                              | real                  | unsupported           | unsupported           | unsupported           |
| normal  | `/_ts/admin/ec`                        | `GET`                                                      | literal        | `always`                              | real                  | unsupported           | unsupported           | unsupported           |
| normal  | `/_ts/admin/eids`                      | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/admin/keys/deactivate`           | `POST`                                                     | literal        | `always`                              | real                  | unsupported           | unsupported           | unsupported           |
| normal  | `/_ts/admin/keys/rotate`               | `POST`                                                     | literal        | `always`                              | real                  | unsupported           | unsupported           | unsupported           |
| normal  | `/_ts/api/v1/batch-sync`               | `POST`                                                     | literal        | `always`                              | real                  | —                     | —                     | —                     |
| normal  | `/_ts/api/v1/identify`                 | `GET`, `OPTIONS`                                           | literal        | `always`                              | real                  | —                     | —                     | —                     |
| normal  | `/_ts/clear-tester`                    | `GET`                                                      | literal        | `settings.tester_cookie.enabled`      | real                  | —                     | —                     | —                     |
| normal  | `/_ts/debug/ja4`                       | `GET`                                                      | conditional    | `settings.debug.ja4_endpoint_enabled` | real                  | —                     | —                     | —                     |
| normal  | `/_ts/page-bids`                       | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/_ts/page-bids`                       | `OPTIONS`                                                  | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/_ts/set-tester`                      | `GET`                                                      | literal        | `settings.tester_cookie.enabled`      | real                  | —                     | —                     | —                     |
| normal  | `/admin/keys/deactivate`               | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/admin/keys/rotate`                   | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `always`                              | guarded               | guarded               | guarded               | guarded               |
| normal  | `/auction`                             | `POST`                                                     | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/click`                   | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/proxy-rebuild`           | `GET`, `POST`                                              | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/proxy`                   | `GET`                                                      | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/first-party/sign`                    | `GET`, `POST`                                              | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/health`                              | `GET`                                                      | literal        | `always`                              | real                  | real                  | —                     | real                  |
| normal  | `/static/tsjs=<file>`                  | `GET`                                                      | template       | `path.starts_with(/static/tsjs=)`     | real                  | real                  | real                  | real                  |
| normal  | `/verify-signature`                    | `POST`                                                     | literal        | `always`                              | real                  | real                  | real                  | real                  |
| normal  | `/{*rest}`                             | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | template       | `publisher_fallback`                  | publisher fallback    | publisher fallback    | publisher fallback    | publisher fallback    |
| normal  | `/`                                    | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `publisher_fallback`                  | publisher fallback    | publisher fallback    | publisher fallback    | publisher fallback    |
| normal  | `<proxy.asset_routes[].prefix>{*rest}` | `GET`, `HEAD`                                              | config derived | `settings.proxy.asset_routes[]`       | real                  | —                     | —                     | —                     |
| startup | `/health`                              | `GET`                                                      | literal        | `startup_error`                       | real                  | —                     | —                     | real                  |
| startup | `/{*rest}`                             | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | template       | `startup_error`                       | startup error (`500`) | startup error (`500`) | startup error (`500`) | startup error (`503`) |
| startup | `/`                                    | `DELETE`, `GET`, `HEAD`, `OPTIONS`, `PATCH`, `POST`, `PUT` | literal        | `startup_error`                       | startup error (`500`) | startup error (`500`) | startup error (`500`) | startup error (`503`) |

`real`, `unsupported`, `guarded`, `publisher fallback`, and `startup error` are
distinct observable dispositions. An em dash means that the adapter has no
local route; the publisher fallback can therefore receive that path. The
startup rows are a separate degraded router, not additional normal routes.

## Contract conventions

Each endpoint contract below states authentication, request and response shape,
status behavior, cache and CORS behavior, configuration gates, rate limiting,
and an example or an explicit not-applicable value.

`Auth: none` means that the endpoint has no built-in authentication. An
operator can still wrap any path with a `[[handlers]]` Basic-auth rule. Unless
an endpoint says otherwise, it has no endpoint-specific CORS policy and no
in-process rate limiter. Responses still pass through the adapter's standard
response finalizer. Upstream proxies can preserve selected upstream headers;
that is not a blanket CORS grant.

There is no repository-wide error body schema. Some handlers return structured
JSON, some return an empty body, and shared adapter errors are plain text with
safe client-facing messages. Treat the status and body documented for each
endpoint as authoritative.

## Utility Endpoints

### GET /\_ts/set-tester

Sets a first-party tester marker cookie for QA or troubleshooting workflows.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: none.

**Configuration:** Disabled by default. Enable with:

```toml
[tester_cookie]
enabled = true
```

**Response when enabled:**

- **Status:** `204 No Content`
- **Headers:**

```http
Set-Cookie: ts-tester=true; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax
Cache-Control: no-store, private
```

The cookie domain comes from `[publisher].cookie_domain`.

**Response when disabled:**

- **Status:** `404 Not Found`
- **Set-Cookie:** none

**Example:**

```bash
curl -i "https://edge.example.com/_ts/set-tester"
```

### GET /\_ts/clear-tester

Clears the first-party tester marker cookie for QA or troubleshooting workflows.

**Contract:** Auth: none. Request body: not applicable. Rate limit: none.
Endpoint-specific CORS: none.

**Configuration:** Uses the same `[tester_cookie].enabled` flag as `/_ts/set-tester`.

**Response when enabled:**

- **Status:** `204 No Content`
- **Headers:**

```http
Set-Cookie: ts-tester=; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax; Max-Age=0
Cache-Control: no-store, private
```

The cookie domain comes from `[publisher].cookie_domain`.

**Response when disabled:**

- **Status:** `404 Not Found`
- **Set-Cookie:** none

**Example:**

```bash
curl -i "https://edge.example.com/_ts/clear-tester"
```

---

## First-Party Endpoints

### POST /auction

Browser and programmatic auction endpoint. It accepts the Trusted Server ad-unit
request shape and returns an OpenRTB response. Creative markup follows the
independent `[auction].sanitize_creatives` and `[auction].rewrite_creatives`
settings; sanitization is opt-in and rewriting is enabled by default.

**Contract:** Auth: none. The buffered JSON body is limited to 256 KiB. A
successful auction and an intentional no-bid both return `200` JSON; disabling
`[auction]` or denying consent produces the no-bid form without calling a
provider. An oversized body returns `413`; malformed input and validation
failures use the shared adapter error mapping; provider failures use the shared
`5xx` mapping. The endpoint sets no dedicated cache or CORS policy and has no
in-process rate limiter. Provider selection, timeouts, and fan-out are governed
by `[auction]` and the enabled provider profiles.

**Request Body:**

```json
{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": { "banner": { "sizes": [[728, 90]] } },
      "bids": [
        {
          "bidder": "example-server-bidder",
          "params": { "placement": "example-placement" }
        }
      ]
    }
  ]
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/auction \
  -H "Content-Type: application/json" \
  -d '{"adUnits":[{"code":"banner","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}'
```

### GET /\_ts/page-bids and GET /\_\_ts/page-bids

Runs the page-level auction used by the TSJS single-page-application hook. The
`/__ts/page-bids` spelling is a deprecated compatibility alias and adds a
`Link` header pointing to its removal issue.

**Request schema:** `path` is optional and defaults to `/`; query and fragment
text inside it are removed before creative-opportunity matching. `format` is
optional and must be `json` when present. A request is admitted only when
`Sec-Fetch-Site: same-origin`, or when `Sec-Fetch-Site` is absent and the
request carries `X-TSJS-Page-Bids`. Every `OPTIONS` request is denied so a
cross-origin caller cannot acquire permission to send that non-simple header.

**Response schema:** `200 application/json` with `{ "slots": [...], "bids":
{...} }`. The ad-stack kill switch, an auction kill switch, or denied consent
returns empty collections. Unknown `format` returns `400`; a rejected GET or
any `OPTIONS` returns `403`; missing `[creative_opportunities]` returns `404`.
The `200`, `400`, and `403` terminal responses are `private, no-store`. The
endpoint has no built-in authentication, CORS grant, or rate limiter.

**Configuration:** `[creative_opportunities]`, `[auction]`, slot page patterns,
provider profiles, and consent settings. Bot and prefetch requests retain slot
shape but skip live provider calls.

**Example:**

```bash
curl 'https://edge.example.com/_ts/page-bids?path=%2Fnews&format=json' \
  -H 'Sec-Fetch-Site: same-origin'
```

---

## Edge Cookie Endpoints

Partners are configured statically in `[[ec.partners]]` and loaded into an in-memory registry at startup. There is no runtime partner-registration endpoint and the legacy browser pixel sync endpoint has been removed; browser-resolved IDs are ingested through Prebid EID cookies.

---

### GET /\_ts/api/v1/identify

Returns EC identity plus the authenticated partner's UID and EID for the current user.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Contract:** The request has no body and uses the `ts-ec` cookie plus the
request consent context. `200` returns the JSON shape below; `204` means there
is no active EC ID; `401` means the Bearer token is unknown; and `403` means
consent or the request origin was denied. A KV read failure still returns `200`
with `degraded: true`. Every response is `no-store`, varies on `Origin` and
`Authorization`, and also sends `Pragma: no-cache`. HTTPS origins equal to or
below `[publisher].domain` receive reflected credentialed CORS for
`GET, OPTIONS`; every other supplied origin receives `403`. `OPTIONS` returns
`200` for no origin or an allowed origin. There is no endpoint rate limiter.
The endpoint is Fastly-only and requires the EC store and a statically
configured `[[ec.partners]]` token.

**Request:**

- Uses `ts-ec` cookie and consent signals

**Response (example):**

```json
{
  "ec": "954d...e0c3.nZ1GxL",
  "consent": "ok",
  "degraded": false,
  "source_domain": "ssp.example.com",
  "uid": "mock-user-123",
  "eid": {
    "source": "ssp.example.com",
    "uids": [{ "id": "mock-user-123", "atype": 3 }]
  },
  "cluster_size": 3
}
```

`uid`, `eid`, and `cluster_size` are optional and omitted when unavailable
(e.g. no partner UID synced yet, KV read degraded, or cluster size not
re-evaluated within the recheck window).

---

### POST /\_ts/api/v1/batch-sync

Server-to-server batch sync endpoint for writing EC ID to partner UID mappings. Mapping timestamps are retained in the request schema for compatibility, but they no longer order writes because EC identity entries do not store per-partner sync timestamps. Valid mappings use idempotent last-write-wins semantics.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Contract:** The JSON body is limited to 2 MiB and 1,000 mappings. The
per-partner, per-minute limit is `ec.partners[].batch_rate_limit`. Responses are
`200` when every mapping is accepted or `207` when at least one is rejected;
item reasons are `invalid_ec_id`, `invalid_partner_uid`, `ineligible`, or
`kv_unavailable`. Authentication failure returns `401 {"error":"invalid_token"}`;
rate exhaustion returns `429`; an oversized body returns `413`; too many
mappings returns `400`. The endpoint emits JSON but defines no dedicated cache
or CORS headers. It is Fastly-only and requires the EC KV store.

**Request Body:**

```json
{
  "mappings": [
    {
      "ec_id": "954d8e7398dd993f78e3875ca1ef7841249781240e913157c1f2d6a6c960e0c3.nZ1GxL",
      "partner_uid": "mock-user-123",
      "timestamp": 1775147300
    }
  ]
}
```

**Response:**

```json
{
  "accepted": 1,
  "rejected": 0,
  "errors": []
}
```

**Example:** `curl -X POST https://edge.example.com/_ts/api/v1/batch-sync -H
'Authorization: Bearer <partner-api-key>' -H 'Content-Type: application/json'
--data @mappings.json`.

---

### GET /first-party/proxy

Unified proxy for resources referenced by creatives (images, scripts, CSS, etc.).

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tsurl` | string | Yes | Target URL without query parameters (base URL) |
| `tstoken` | string | Yes | Base64url SHA-256 token derived from the encrypted reconstructed target URL |
| `tsexp` | integer | No | Unix expiry carried by newly minted URLs and covered by `tstoken` |
| `*` | any | No | Original target URL query parameters (preserved as-is) |

**Response:**

- **Content-Type:** Mirrors upstream or inferred from content
- **Body:** Proxied resource content
  - HTML responses: Rewritten with creative processor
  - Image responses: Proxied with content-type inference
  - Other: Passed through

**Behavior:**

- Validates `tstoken` against reconstructed URL
- Follows redirects (301/302/303/307/308, max 4 hops)
- Injects EC ID as `ts-ec` query parameter
- Logs 1×1 pixel impressions
- Enforces `proxy.allowed_domains` on the initial fetch and every redirect;
  an empty list is open mode

**Contract:** Auth: signed query token, not HTTP credentials. Success preserves
the upstream status and selected headers; HTML and CSS can be rewritten and
image content type can be inferred. Missing `tsurl`/`tstoken`, malformed or
expired `tsexp`, and upstream proxy failures currently use the shared `502`
proxy-error mapping; a mismatched token or blocked host returns `403`.
Redirects are followed for 301/302/303/307/308, with at most four hops. Cache
and CORS are derived from the sanitized upstream response and response
finalization; there is no endpoint CORS grant or rate limiter. This endpoint
validates an existing signature—it does not mint one. Signing depends on
`publisher.proxy_secret`; server-side fetches also enforce
`proxy.allowed_domains`.

**Example:**

```bash
# Original URL: https://ad.doubleclick.net/pixel?id=123&type=view
# Signed proxy URL:
curl "https://edge.example.com/first-party/proxy?tsurl=https://ad.doubleclick.net/pixel&id=123&type=view&tstoken=abc123xyz..."
```

**Error Responses:**

- `403 Forbidden` - Token validation or allowed-domain validation failed
- `502 Bad Gateway` - Missing signing fields, invalid/expired expiration, or
  upstream proxy failure

---

### GET /first-party/click

Click tracking redirect endpoint.

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tsurl` | string | Yes | Target redirect URL without query parameters |
| `tstoken` | string | Yes | Base64url SHA-256 token derived from the encrypted reconstructed target URL |
| `tsexp` | integer | No | Optional Unix expiry covered by `tstoken` |
| `*` | any | No | Original target URL query parameters |

**Response:**

- **Status:** `302 Found`
- **Location:** Reconstructed target URL with EC ID injected

**Behavior:**

- Validates `tstoken` against reconstructed URL
- Injects `ts-ec` query parameter
- Logs click metadata (tsurl, referer, user agent)
- Does not proxy content (redirect only)

**Contract:** Auth: signed query token. A valid request returns `302` with
`Cache-Control: no-store, private`; it does not apply `proxy.allowed_domains`
because it performs no server-side fetch. Signature errors have the same `403`
versus shared `502` split as `/first-party/proxy`. The route has no dedicated
CORS grant or rate limiter and validates rather than mints a signature.

**Example:**

```bash
curl -I "https://edge.example.com/first-party/click?tsurl=https://advertiser.com/landing&campaign=123&tstoken=xyz..."
# → 302 Location: https://advertiser.com/landing?campaign=123&ts-ec=abc123
```

---

### GET/POST /first-party/sign

URL signing endpoint. Returns a signed first-party proxy URL for a valid HTTP or HTTPS target. When `proxy.allowed_domains` is non-empty, the endpoint checks the parsed target host before signing. An empty list permits every valid host.

**Contract:** Auth: none unless an operator adds a handler. GET accepts a `url`
query value; POST accepts `{ "url": "..." }` and is limited to 64 KiB. `200`
returns `{href, base}` and gives `href` a 30-second `tsexp`. A disallowed host
returns `403`, an oversized POST returns `413`, and malformed/unsupported URLs
or JSON use the current shared error mapping. The endpoint sets JSON content
type but no dedicated cache or CORS policy and has no rate limiter. It mints a
new proxy signature; it does not validate a caller-supplied `tstoken`.
Signing depends on `publisher.proxy_secret` and checks
`proxy.allowed_domains` before minting.

**Request Methods:** GET or POST

**GET Request:**

```bash
curl "https://edge.example.com/first-party/sign?url=https://cdn.example.com/pixel.gif"
```

**POST Request:**

```bash
curl -X POST https://edge.example.com/first-party/sign \
  -H "Content-Type: application/json" \
  -d '{"url":"https://cdn.example.com/pixel.gif"}'
```

**Response:**

```json
{
  "href": "/first-party/proxy?tsurl=https%3A%2F%2Fcdn.example.com%2Fpixel.gif&tstoken=abc123...&tsexp=1234567890",
  "base": "https://cdn.example.com/pixel.gif"
}
```

`href` is the signed proxy path. `base` is the normalized target without its query or fragment.

**Error Responses:**

- `403 Forbidden`: The target has a valid host that does not match a non-empty `proxy.allowed_domains` list
- `413 Payload Too Large`: The POST body exceeds 64 KiB
- Shared `5xx`: Malformed JSON or an invalid/unsupported target under the
  current proxy-error mapping

**Use Cases:**

- TSJS creative runtime (image/iframe proxying)
- Dynamic URL signing in client-side code
- Testing proxy URL generation

---

### POST /first-party/proxy-rebuild

URL mutation recovery endpoint. Re-signs a click URL after creative JavaScript modifies its query parameters. The original `tstoken` is validated first, and `tsurl`, `tstoken`, and `tsexp` can never be added or removed.

**Contract:** Auth: the original signed click URL. JSON and form POST bodies
are limited to 64 KiB. A JSON POST returns `200` with the response below; GET
and form-encoded POST return `302` to the rebuilt click URL. All success forms
are `no-store, private`. A bad original token returns `403`; malformed input,
an expired token, a non-click `tsclick`, a reserved-field mutation, or an
attempt to overwrite an existing query parameter uses the shared error
mapping; oversized bodies return `413`. There is no dedicated CORS grant or
rate limiter. Rebuild validates the old signature before minting its
replacement and never permits `tsurl`, `tstoken`, or `tsexp` mutation.
Validation and re-signing depend on `publisher.proxy_secret`.

**Request Body:**

```json
{
  "tsclick": "/first-party/click?tsurl=https%3A%2F%2Fadvertiser.example&campaign=123&tstoken=original...",
  "add": {
    "utm_source": "banner"
  },
  "del": ["old_param"]
}
```

`tsclick` may be root-relative (the form the rewriter emits) or absolute.

**Response:**

```json
{
  "href": "/first-party/click?tsurl=https%3A%2F%2Fadvertiser.example&campaign=123&utm_source=banner&tstoken=new...",
  "base": "https://advertiser.example",
  "added": { "utm_source": "banner" },
  "removed": ["old_param"]
}
```

**Use Cases:**

- TSJS click guard (automatic URL repair) on same-origin pages
- Handling creative JavaScript that modifies tracking URLs

---

### GET /first-party/proxy-rebuild

Navigation form of the same recovery, for creatives rendered in a sandboxed iframe without `allow-same-origin`. Their opaque origin makes the JSON POST a CORS-preflighted cross-origin request that the endpoint does not answer, so the click guard navigates here instead — navigations are not subject to CORS.

**Query Parameters:**

| Parameter | Required | Description                                       |
| --------- | -------- | ------------------------------------------------- |
| `tsclick` | Yes      | URL-encoded signed click URL                      |
| `add`     | No       | URL-encoded JSON object of parameters to add      |
| `del`     | No       | URL-encoded JSON array of parameter names to drop |

**Response:** `302` with the rebuilt `/first-party/click?...` URL in `Location` and `Cache-Control: no-store, private`. The browser follows it to `/first-party/click`, which redirects on to the advertiser.

Validation is identical to the POST form.

**Form-encoded POST (navigation, no URL length limit):** when the GET recovery URL would exceed the platform's request-URL limit (Fastly Compute rejects request URLs over 8192 bytes before the handler runs), the click guard submits a form instead. A `POST` carrying `Content-Type: application/x-www-form-urlencoded` with the same `tsclick`/`add`/`del` fields is treated as a navigation and answered with the same `302`, not the JSON body.

---

## Request Signing Endpoints

### GET /.well-known/trusted-server.json

Returns the Trusted Server discovery document, which includes active public keys in JWKS
format for signature verification.

**Contract:** Auth: none. Request body: not applicable. `200 application/json`
returns `{version, jwks}`; signing-store lookup, parse, or serialization failure
uses the shared `500` response. The endpoint has no dedicated cache, CORS, or
rate-limit policy. It requires usable `[request_signing]` storage and at least
the store state needed to enumerate active public keys.

**Response:**

```json
{
  "version": "1.0",
  "jwks": {
    "keys": [
      {
        "kty": "OKP",
        "crv": "Ed25519",
        "kid": "ts-2025-01-A",
        "use": "sig",
        "x": "UVTi04QLrIuB7jXpVfHjUTVN5aIdcbPNr50umTtN8pw"
      }
    ]
  }
}
```

**Example:**

```bash
curl https://edge.example.com/.well-known/trusted-server.json
```

**Use Cases:**

- Signature verification by downstream systems
- Key rotation validation
- Integration testing

---

### POST /verify-signature

Verifies a signature against a payload and key ID.

**Contract:** Auth: none. The JSON body is limited to 4 KiB. A syntactically
valid request always returns `200 application/json`: `verified` is false for an
invalid signature, missing key, or internal verification failure, and the
internal failure is exposed only as the sanitized `internal verification error`
value. An oversized body returns `413`; malformed JSON uses the shared `500`
configuration-error mapping. There is no dedicated cache, CORS, or rate-limit
policy. `[request_signing]` storage must be readable.

**Request Body:**

```json
{
  "payload": "base64-encoded-data",
  "signature": "base64-signature",
  "kid": "ts-2025-01-A"
}
```

**Response (Success):**

```json
{
  "verified": true,
  "kid": "ts-2025-01-A",
  "message": "Signature verified successfully"
}
```

**Response (Failure):**

```json
{
  "verified": false,
  "kid": "ts-2025-01-A",
  "message": "Signature verification failed",
  "error": "Invalid signature"
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/verify-signature \
  -H "Content-Type: application/json" \
  -d '{"payload":"SGVsbG8gV29ybGQ=","signature":"abc123...","kid":"ts-2025-01-A"}'
```

---

### POST /\_ts/admin/keys/rotate

Generates and activates a new signing key.

**Authentication:** Requires basic auth (configured via `handlers` in `trusted-server.toml`)

**Contract:** Fastly implements this route; Axum, Cloudflare, and Spin return
`501`. Startup requires complete Basic-auth coverage of `/_ts/admin`. The body
is empty or JSON `{ "kid": "..." }`, limited to 4 KiB. A supplied KID must be
1–128 characters, use ASCII alphanumerics plus `-_.:`, and start with a
lowercase ASCII letter. Success returns `200`; invalid KIDs return structured
`400`; store/rotation failures return structured `500`. Successful and failure
bodies use the same schema and set `success` accordingly. The handler defines
no dedicated cache, CORS, or rate-limit policy.

**Request Body (Optional):**

```json
{
  "kid": "custom-key-id"
}
```

If omitted, auto-generates date-based ID (e.g., `ts-2025-01-15-A`).

**Response:**

```json
{
  "success": true,
  "message": "Key rotated successfully",
  "new_kid": "ts-2025-01-15-A",
  "previous_kid": "ts-2025-01-14-A",
  "active_kids": ["ts-2025-01-15-A", "ts-2025-01-14-A"],
  "jwk": { "kty": "OKP", "crv": "Ed25519", "kid": "ts-2025-01-15-A" }
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/_ts/admin/keys/rotate \
  -u admin:password \
  -H "Content-Type: application/json"
```

**Behavior:**

- Keeps both new and previous key active
- Updates `current-kid` to new key
- Preserves old key for graceful transition

See [Key Rotation Guide](./key-rotation.md) for workflow details.

---

### POST /\_ts/admin/keys/deactivate

Deactivates or deletes a signing key.

**Authentication:** Requires basic auth

**Contract:** Fastly implements this route; Axum, Cloudflare, and Spin return
`501`. The JSON body is limited to 4 KiB. Deactivation accepts legacy KIDs that
satisfy the 1–128 character and safe-character rules but do not satisfy the
new-key lowercase-leading rule. Success returns `200`; invalid KIDs return a
structured `400`; storage failures return a structured `500`. The handler
defines no dedicated cache, CORS, or rate-limit policy.

**Request Body:**

```json
{
  "kid": "ts-2025-01-14-A",
  "delete": false
}
```

| Field    | Type    | Required | Description                                       |
| -------- | ------- | -------- | ------------------------------------------------- |
| `kid`    | string  | Yes      | Key ID to deactivate                              |
| `delete` | boolean | No       | If true, permanently removes key (default: false) |

**Response:**

```json
{
  "success": true,
  "message": "Key deactivated successfully",
  "deactivated_kid": "ts-2025-01-14-A",
  "deleted": false,
  "remaining_active_kids": ["ts-2025-01-15-A"]
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/_ts/admin/keys/deactivate \
  -u admin:password \
  -H "Content-Type: application/json" \
  -d '{"kid":"ts-2025-01-14-A","delete":true}'
```

---

## Admin Diagnostic Endpoints

These endpoints expose sensitive identity and cookie data and require HTTP Basic Authentication. Configure a handler that covers the entire `/_ts/admin` namespace; startup rejects configurations that do not protect every admin route, including handlers that match only some `/_ts/admin/ec/{id}` values — the dynamic route needs a prefix-level matcher such as `^/_ts/admin` or `^/_ts/admin/ec/`. The whole `/_ts/admin` prefix is reserved: any admin path that reaches publisher fallback — unknown, malformed, or percent-encoded (`/_ts/admin%2Fec`) — is answered locally with `404` and is never proxied, so an admin `Authorization` header and request body never reach the publisher origin. The retired non-`/_ts` `/admin/keys` aliases are reserved the same way. Normal diagnostic-handler responses after successful authentication are JSON with `Cache-Control: no-store`. Missing or invalid credentials receive the shared plaintext `401 Unauthorized` Basic-auth challenge. Unexpected configuration or KV failures use the adapter's shared plaintext `5xx` error response. Those authentication and internal-error responses are outside the diagnostic JSON and cache-header contract.

The diagnostic handlers have no CORS grant or in-process rate limiter. Apply an
edge policy if operator access also needs request-rate enforcement.

The examples below use fictional IDs and values only.

### GET /\_ts/admin/ec

### GET /\_ts/admin/ec/`{id}`

Reads an EC identity-graph record for troubleshooting. The explicit route accepts an EC ID in `{64 lowercase hex}.{6 alphanumeric}` format. The bare route uses the request's `ts-ec` cookie.

This lookup is implemented only by the Fastly adapter because the identity graph is stored in Fastly KV. Other adapters return `501 Not Implemented`.

**Response fields:**

- `ec_id`, `store`, and `generation` identify the raw KV lookup.
- `entry` preserves the stored JSON shape, including unknown and legacy fields. Derived `created_iso` and `consent.updated_iso` fields are added only when absent.
- `metadata` preserves the stored metadata JSON shape.
- `tombstone` reports whether consent has been withdrawn. It is absent when the entry body cannot be parsed as JSON or deserialized as the typed EC schema.
- `auction.eids` previews the partner EIDs the stored record can contribute; `auction.skipped` explains filtered IDs.
- `entry_error`, `metadata_error`, and `raw_body` keep malformed or schema-incompatible records inspectable.

The auction preview validates the stored record and partner configuration, but cannot reproduce live per-request consent checks. It must not be treated as proof that a specific auction request will receive those EIDs.

**Status codes:**

| Status | Meaning                                                     |
| ------ | ----------------------------------------------------------- |
| `200`  | Record found, including inspectable corrupt records         |
| `400`  | Invalid explicit EC ID                                      |
| `401`  | Missing or invalid Basic credentials                        |
| `404`  | Record not found, or the bare route has no `ts-ec` cookie   |
| `405`  | Method other than `GET` (`Allow: GET`)                      |
| `501`  | EC identity graph unavailable on this adapter or deployment |
| `5xx`  | Unexpected configuration or KV failure (plaintext)          |

```bash
curl -u 'admin:<resolved-admin-password>' \
  "https://edge.example.com/_ts/admin/ec/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.abc123"

curl -u 'admin:<resolved-admin-password>' \
  --cookie "ts-ec=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.abc123" \
  "https://edge.example.com/_ts/admin/ec"
```

### GET /\_ts/admin/eids

Parses the request's `ts-eids` and `sharedId` cookies and previews which configured partner IDs cookie ingestion would match or drop. It performs request inspection only: it does not read or write KV and is available on every adapter.

After successful authentication this endpoint always returns `200 OK`; missing or malformed cookies are represented by `cookie_present`, `sharedid_present`, and `parse_error`. The `ingest.matched` and `ingest.unmatched` arrays show the ingestion preview. Each unmatched entry contains its `source` and either a `no_partner` reason when no configured partner recognizes it or `no_valid_uid` when the partner exists but every supplied UID is empty or exceeds the storage limit.

The request body is not applicable. The `200` JSON response is `no-store`.
`401`, `405`, and unexpected `5xx` responses follow the shared admin behavior
above. The route is available on every adapter and depends on configured EC
partners, but not on the EC KV store.

```json
{
  "ingest": {
    "matched": [
      {
        "source_domain": "configured.example",
        "uid": "fictional-uid"
      }
    ],
    "unmatched": [
      {
        "source": "unknown.example",
        "reason": "no_partner"
      }
    ]
  }
}
```

```bash
curl -u 'admin:<resolved-admin-password>' \
  --cookie "sharedId=fictional-shared-id" \
  "https://edge.example.com/_ts/admin/eids"
```

Malformed diagnostic paths return a local `404`, and unsupported methods return a local `405`; they are never forwarded to the publisher origin.

### Legacy /admin/keys/rotate and /admin/keys/deactivate aliases

All seven fallback methods return local `404 text/plain`. These retired aliases
accept no schema, invoke no key operation, forward nothing to the publisher,
and define no cache, CORS, or rate-limit policy. Example: `curl -i
https://edge.example.com/admin/keys/rotate` must not reach the origin.

---

## Operational and fallback endpoints

### GET /health

**Contract:** Auth: none. Request and JSON schema: not applicable. Fastly,
Axum, and Spin return `200 text/plain` with `ok`; Cloudflare has no local health
route. Fastly short-circuits before application construction. Spin keeps the
probe live in its startup-error router; Fastly also remains live because it is
pre-router. The route has no configuration gate, dedicated cache or CORS
policy, or rate limiter. Example: `curl -fsS https://edge.example.com/health`.

This is liveness, not readiness: a `200` does not prove that configuration,
stores, publisher proxying, or integrations are usable.

### GET /\_ts/debug/ja4

**Contract:** Fastly-only; gated by `[debug].ja4_endpoint_enabled`. Auth: none
unless wrapped by a handler. The request has no body. Enabled requests return
`200 text/plain` with JA4, HTTP/2 fingerprint, cipher, TLS version, user-agent,
and client-hint values. Disabled requests return `404`. The response is
`no-store, private` and varies on the user-agent/client-hint fields. It defines
no CORS policy or rate limiter. Example: `curl -i
https://edge.example.com/_ts/debug/ja4`.

### GET or HEAD `<proxy.asset_routes[].prefix>{path}`

**Contract:** Fastly-only and registered once for each
`[[proxy.asset_routes]]` entry. Auth: optional S3 SigV4 origin credentials,
which authenticate Trusted Server to the origin rather than the caller.
Request body: not applicable. The route maps the remainder of the path and the
configured query policy to the selected origin, preserves range and conditional
request semantics, and returns the origin status/body after stripping unsafe
response headers. Cache behavior follows the route's normalized cache policy;
credentialed or otherwise private responses are forced `private, no-store`.
No dedicated CORS grant or rate limiter is installed. Example: if the prefix is
`/assets/`, request `curl -I https://edge.example.com/assets/logo.svg`.

### Publisher fallback: `/` and `/{*rest}`

**Contract:** `GET`, `POST`, `HEAD`, `OPTIONS`, `PUT`, `PATCH`, and `DELETE`
proxy to `[publisher].backend`. The request and response schemas are the
publisher origin's schemas. Trusted Server may rewrite eligible HTML, assemble
templates, inject configured integrations, apply EC finalization, and preserve
streaming where supported. The origin status is normally preserved; fetch or
processing failures use shared adapter errors. Cacheability is decided from
the origin response plus personalization, cookie, template, and rewrite
effects. CORS is therefore the sanitized publisher-origin policy, not a global
Trusted Server policy. There is no built-in endpoint rate limiter. Example:
`curl -i https://edge.example.com/article`.

When startup fails, the degraded router replaces normal routing: Fastly,
Axum, and Cloudflare return `500` for both fallback shapes and all seven
methods; Spin returns a generic `503`. Only the health behavior shown in the
table survives.

---

## TSJS Library Endpoint

### GET /static/tsjs=`<filename>`

Serves the TSJS (Trusted Server JavaScript) library.

**Path Pattern:** `/static/tsjs=<filename>?v=<hash>`

**Supported filenames:** `tsjs-unified.js` and `tsjs-unified.min.js` serve the
core plus enabled immediate modules. `tsjs-<integration>.js` and
`tsjs-<integration>.min.js` serve a known enabled deferred module, plus the
enabled standalone `gpt_diagnostics` module. An unknown, disabled, or
non-deferred module filename returns `404`.

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `v` | string | No | Cache-busting hash (SHA256 of bundle contents) |

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** TSJS bundle (IIFE format)
- **Headers:** ETag for caching

**Contract:** Auth: none. Request body: not applicable. A known eligible bundle
returns `200 application/javascript`; conditional requests can return `304`;
unknown/disabled filenames return `404`. When `v` exactly equals the generated
content hash, the response is public and immutable for one year. Other bundle
responses retain validator-based static caching without the immutable promise.
The route defines no CORS policy or rate limiter. Its module gate is the
compiled integration registry, not an arbitrary filename lookup.

**Example:**

```html
<script
  src="/static/tsjs=tsjs-unified.min.js?v=a1b2c3d4..."
  id="trustedserver-js"
></script>
```

**Module Selection:**
All integration modules are built at compile time. At runtime, the server concatenates only the modules whose integrations are enabled in `trusted-server.toml` (or env vars). No rebuild is required to change the module set.

---

## Integration Endpoints

Every row below records a compiled integration registration predicate.
An integration with no HTTP route can still contribute a browser module,
rewriter, injector, post-processor, request filter, or auction mediator.

| Integration          | Registration predicate                                           | HTTP routes                                       |
| -------------------- | ---------------------------------------------------------------- | ------------------------------------------------- |
| `adserver_mock`      | `auction.mediator=adserver_mock;enabled=true`                    | None                                              |
| `aps`                | `plan.has_profile(aps);rendering_mode=publisher_native`          | None                                              |
| `aps`                | `plan.has_profile(aps);rendering_mode=trusted_server`            | `GET /integrations/aps/renderer`                  |
| `creative`           | `always`                                                         | None                                              |
| `datadome`           | `enabled=true;enable_protection=false`                           | `GET /integrations/datadome/js/*`                 |
| `datadome`           | `enabled=true;enable_protection=false`                           | `GET /integrations/datadome/js/`                  |
| `datadome`           | `enabled=true;enable_protection=false`                           | `GET /integrations/datadome/tags.js`              |
| `datadome`           | `enabled=true;enable_protection=false`                           | `POST /integrations/datadome/js/*`                |
| `datadome`           | `enabled=true;enable_protection=false`                           | `POST /integrations/datadome/js/`                 |
| `datadome`           | `enabled=true;enable_protection=true`                            | `GET /integrations/datadome/js/*`                 |
| `datadome`           | `enabled=true;enable_protection=true`                            | `GET /integrations/datadome/js/`                  |
| `datadome`           | `enabled=true;enable_protection=true`                            | `GET /integrations/datadome/tags.js`              |
| `datadome`           | `enabled=true;enable_protection=true`                            | `POST /integrations/datadome/js/*`                |
| `datadome`           | `enabled=true;enable_protection=true`                            | `POST /integrations/datadome/js/`                 |
| `didomi`             | `enabled=true;prefix=proxy_path\|\|/integrations/didomi/consent` | `GET <prefix>/*`                                  |
| `didomi`             | `enabled=true;prefix=proxy_path\|\|/integrations/didomi/consent` | `POST <prefix>/*`                                 |
| `google_tag_manager` | `enabled=true`                                                   | `GET /integrations/google_tag_manager/collect`    |
| `google_tag_manager` | `enabled=true`                                                   | `GET /integrations/google_tag_manager/g/collect`  |
| `google_tag_manager` | `enabled=true`                                                   | `GET /integrations/google_tag_manager/gtag.js`    |
| `google_tag_manager` | `enabled=true`                                                   | `GET /integrations/google_tag_manager/gtag/js`    |
| `google_tag_manager` | `enabled=true`                                                   | `GET /integrations/google_tag_manager/gtm.js`     |
| `google_tag_manager` | `enabled=true`                                                   | `POST /integrations/google_tag_manager/collect`   |
| `google_tag_manager` | `enabled=true`                                                   | `POST /integrations/google_tag_manager/g/collect` |
| `gpt_diagnostics`    | `enabled=true`                                                   | None                                              |
| `gpt`                | `enabled=true`                                                   | `GET /integrations/gpt/pagead/*`                  |
| `gpt`                | `enabled=true`                                                   | `GET /integrations/gpt/script`                    |
| `gpt`                | `enabled=true`                                                   | `GET /integrations/gpt/tag/*`                     |
| `lockr`              | `enabled=true`                                                   | `GET /integrations/lockr/api/*`                   |
| `lockr`              | `enabled=true`                                                   | `GET /integrations/lockr/sdk`                     |
| `lockr`              | `enabled=true`                                                   | `POST /integrations/lockr/api/*`                  |
| `nextjs`             | `enabled=true`                                                   | None                                              |
| `osano`              | `enabled=true`                                                   | None                                              |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/api/*`               |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/cdn/*`               |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/events/*`            |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/sdk`                 |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/secure-signal/*`     |
| `permutive`          | `enabled=true`                                                   | `GET /integrations/permutive/sync/*`              |
| `permutive`          | `enabled=true`                                                   | `POST /integrations/permutive/api/*`              |
| `permutive`          | `enabled=true`                                                   | `POST /integrations/permutive/events/*`           |
| `permutive`          | `enabled=true`                                                   | `POST /integrations/permutive/secure-signal/*`    |
| `permutive`          | `enabled=true`                                                   | `POST /integrations/permutive/sync/*`             |
| `prebid`             | `enabled=true;script_patterns=config-derived`                    | `GET /integrations/prebid/bundle.js`              |
| `prebid`             | `enabled=true;script_patterns=config-derived`                    | `GET <integrations.prebid.script_patterns[]>`     |
| `sourcepoint`        | `enabled=true`                                                   | `GET /integrations/sourcepoint/cdn/*`             |
| `sourcepoint`        | `enabled=true`                                                   | `HEAD /integrations/sourcepoint/cdn/*`            |
| `sourcepoint`        | `enabled=true`                                                   | `OPTIONS /integrations/sourcepoint/cdn/*`         |
| `sourcepoint`        | `enabled=true`                                                   | `POST /integrations/sourcepoint/cdn/*`            |
| `testlight`          | `enabled=true`                                                   | `POST /integrations/testlight/auction`            |

### Integration proxy contracts

All integration routes are registered only when the documented predicate is
true. A disabled integration registers no route, so its path continues through
normal routing and can reach the publisher fallback. Duplicate registrations
are startup errors. None of these routes has built-in caller authentication or
an in-process rate limiter; `[[handlers]]` and platform controls remain
available when a deployment needs either.

| Route family      | Request and success contract                                                                                                                                           | Errors, cache, and CORS                                                                                                                                                                       | Example                                                                                                                                   |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| APS renderer      | `GET /integrations/aps/renderer`; no body; `200 text/html` static opaque-frame renderer with CSP, `nosniff`, and `no-referrer`                                         | Present only for an enabled APS profile in `trusted_server` rendering mode; no dedicated cache/CORS headers                                                                                   | Browser-internal iframe target; curl is not a meaningful auction test                                                                     |
| DataDome tag      | `GET /integrations/datadome/tags.js`; query forwarded; successful upstream `200` is rewritten and returned as JavaScript                                               | Non-200 upstream status is preserved; rewritten `200` uses `cache_ttl_seconds`; upstream `Access-Control-Allow-Origin` is copied when present                                                 | `curl -i https://edge.example.com/integrations/datadome/tags.js`                                                                          |
| DataDome signals  | `GET` or `POST /integrations/datadome/js/` and `/js/*`; method, query, bounded POST body, and selected browser headers forwarded to `api_origin`                       | Upstream status/body/headers preserved; transport or size failures use shared integration errors; no added cache/CORS policy                                                                  | Browser SDK traffic; manual payload is upstream-schema-specific                                                                           |
| Didomi consent    | `GET` or `POST` under the configured prefix (default `/integrations/didomi/consent/*`); path selects SDK or API origin; query and bounded POST body forwarded          | Upstream status/body preserved; SDK responses receive the integration's CORS headers; API responses retain selected upstream headers; no local cache policy                                   | `curl -i https://edge.example.com/integrations/didomi/consent/loader.js`                                                                  |
| GTM/gtag scripts  | `GET` the generated `gtm.js`, `gtag.js`, or `gtag/js` paths; query forwarded or configured container ID supplied; successful script is rewritten                       | Non-success upstream status preserved; rewritten scripts use `cache_max_age`; oversized rewritten upstream bodies use shared integration errors                                               | `curl -i 'https://edge.example.com/integrations/google_tag_manager/gtm.js?id=GTM-XXXX'`                                                   |
| Google collect    | `GET` or `POST` the generated `collect` or `g/collect` paths; query, selected headers, and bounded body proxy to the configured Google origin                          | Malformed `Content-Length` returns `400`; body over `max_beacon_body_size` returns `413`; stream-read failure returns `502`; upstream response otherwise preserved                            | Browser beacon; body schema belongs to Google Analytics                                                                                   |
| GPT               | `GET` `/script`, `/pagead/*`, or `/tag/*`; path/query proxy to the configured GPT origins and script content can be rewritten                                          | Upstream status is preserved; successful scripts/assets apply integration cache rules; selected upstream CORS is preserved                                                                    | `curl -i https://edge.example.com/integrations/gpt/script`                                                                                |
| Lockr SDK         | `GET /integrations/lockr/sdk`; no body; fetches and returns the configured SDK as JavaScript                                                                           | Successful SDK uses `cache_ttl_seconds`; upstream/transport failures follow integration mapping; no added CORS policy                                                                         | `curl -i https://edge.example.com/integrations/lockr/sdk`                                                                                 |
| Lockr API         | `GET` or `POST /integrations/lockr/api/*`; path, query, selected headers, and bounded body proxy to `api_endpoint`; publisher credentials are stripped                 | Upstream status/body preserved; no local cache/CORS policy                                                                                                                                    | Payload is Lockr-specific; use the SDK for normal calls                                                                                   |
| Permutive SDK     | `GET /integrations/permutive/sdk`; no body; fetches `{organization_id}.edge.permutive.app/{workspace_id}-web.js`                                                       | Successful SDK is JavaScript cached for `cache_ttl_seconds`; transport failures follow integration mapping                                                                                    | `curl -i https://edge.example.com/integrations/permutive/sdk`                                                                             |
| Permutive proxies | Generated `GET`/`POST` API, secure-signal, events, and sync routes plus `GET` CDN; suffix, query, selected headers, and bounded body proxy to the corresponding origin | Upstream status/body preserved; no added cache/CORS policy                                                                                                                                    | `curl -i https://edge.example.com/integrations/permutive/api/settings`                                                                    |
| Prebid bundle     | `GET /integrations/prebid/bundle.js`; no request body; proxies the configured external bundle                                                                          | Requires `external_bundle_url` and its host in `proxy.allowed_domains`; `v` must satisfy the configured cache mode; response strips unsafe headers and applies the bundle cache contract      | `curl -i 'https://edge.example.com/integrations/prebid/bundle.js?v=<configured-sha256>'`                                                  |
| Prebid blockers   | `GET` each exact configured `script_patterns` path; returns `200` empty JavaScript to suppress the publisher bundle                                                    | No upstream call; empty list registers none; dedicated CORS is not applicable                                                                                                                 | `curl -i https://edge.example.com/prebid.js`                                                                                              |
| Sourcepoint CDN   | `GET`, `HEAD`, `OPTIONS`, or `POST /integrations/sourcepoint/cdn/*`; suffix/query and a bounded body proxy to `cdn_origin`; selected consent cookies can round-trip    | Upstream status preserved; cookie-bearing responses are private; eligible static responses use `cache_ttl_seconds`; redirect locations and eligible JS/HTML content are rewritten first-party | `curl -i https://edge.example.com/integrations/sourcepoint/cdn/unified/wrapperMessagingWithoutDetection.js`                               |
| Testlight auction | `POST /integrations/testlight/auction`; OpenRTB JSON is bounded, parsed, and sent to the configured endpoint with `user.id` populated from the consent-allowed EC ID   | Malformed/oversized input and transport errors use shared mappings; upstream status/body preserved; no added cache/CORS policy and no EC response header                                      | `curl -X POST https://edge.example.com/integrations/testlight/auction -H 'Content-Type: application/json' -d '{"imp":[{"id":"slot-1"}]}'` |

`adserver_mock`, `creative`, `gpt_diagnostics`, `nextjs`, and `osano` have no
HTTP endpoint. Their request schema, statuses, cache/CORS contract, rate limit,
and curl example are therefore not applicable; their browser, mediator,
rewriter, or diagnostic behavior is documented in the integration guides.

### Prebid Integration

#### POST /auction

See [First-Party Endpoints](#post-auction) above.

#### GET /integrations/prebid/bundle.js

Proxies the configured external Prebid bundle through the first-party domain.
The optional `v` query value is the configured SHA-256 cache key.

#### GET `<script_patterns>` (Optional)

Each configured Prebid script pattern registers an endpoint that returns empty
JavaScript, preventing the publisher's original Prebid bundle from loading.
The defaults include `/prebid.js`, `/prebid.min.js`, `/prebidjs.js`, and
`/prebidjs.min.js`; set `script_patterns = []` to disable interception.

---

### Permutive Integration

#### GET /integrations/permutive/sdk

Serves Permutive SDK from first-party domain.

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** Permutive SDK fetched from `{organization_id}.edge.permutive.app/{workspace_id}-web.js`
- **Cache:** 1 hour (configurable via `cache_ttl_seconds`)

#### GET/POST /integrations/permutive/api/\*

Proxies to `api.permutive.com`.

**Example:**

```bash
curl https://edge.example.com/integrations/permutive/api/settings
# → Proxies to https://api.permutive.com/settings
```

#### GET/POST /integrations/permutive/secure-signal/\*

Proxies to `secure-signals.permutive.app`.

#### GET/POST /integrations/permutive/events/\*

Proxies to `events.permutive.app` for event tracking.

#### GET/POST /integrations/permutive/sync/\*

Proxies to `sync.permutive.com` for ID synchronization.

#### GET /integrations/permutive/cdn/\*

Proxies to `cdn.permutive.com` for static assets.

---

### Testlight Integration

#### POST /integrations/testlight/auction

Testing auction endpoint with EC ID injection.

**Request Body:**

```json
{
  "user": {
    "id": null
  },
  "imp": [{ "id": "slot-1" }]
}
```

**Response:**
Proxies to configured endpoint with `user.id` populated with EC ID.

**Response Headers:**

No EC ID response header is emitted. EC identity is maintained with the `ts-ec` cookie.

---

## Authentication and policy boundaries

### Basic Authentication

Endpoints under protected paths require HTTP Basic Authentication:

**Configuration:**

```toml
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "admin_password"
```

`password` is a key in the Trusted Server secret store. Provision the actual
Basic Authentication password under `admin_password`.

**Usage:**

```bash
curl -u 'admin:<resolved-admin-password>' https://edge.example.com/_ts/admin/keys/rotate
```

**Protected Endpoints:**

- `/_ts/admin/keys/rotate`
- `/_ts/admin/keys/deactivate`
- `/_ts/admin/ec`
- `/_ts/admin/ec/{id}`
- `/_ts/admin/eids`
- Any paths matching configured `handlers` patterns

The EC partner APIs use their own Bearer-token contract. Signed first-party
proxy routes use `tstoken` instead of HTTP authentication. All other built-in
routes are unauthenticated unless the operator deliberately wraps them with a
handler.

### Rate limiting

`/_ts/api/v1/batch-sync` is the only endpoint in this reference with a
built-in request-rate limiter. Its limit is per partner and configured with
`ec.partners[].batch_rate_limit`. Other limits must be implemented with the
deployment platform or an upstream service; this project does not prescribe
universal numeric limits.

### CORS

CORS is endpoint-specific. `/_ts/api/v1/identify` implements a closed,
publisher-domain policy and an explicit `OPTIONS` handler. Page-bids explicitly
denies preflight. Several integration proxies preserve or synthesize only the
headers described in their contract. `response_headers` is standard response
finalization, not a substitute for registering and validating an `OPTIONS`
route; do not infer cross-origin support from a configured header alone.

### Errors

Error representation is also endpoint-specific. Consult the contract above
and [Error Reference](./error-reference.md). Do not write a client that assumes
every failure is JSON or that every upstream failure has the same status.

---

## Next Steps

- Explore [Integrations Overview](./integrations-overview.md)
- Learn about [Configuration](./configuration.md)
- Review [Error Reference](./error-reference.md)
- Understand [Configuration Reference](./configuration.md)
