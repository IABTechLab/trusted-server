# API Reference

Quick reference for all Trusted Server HTTP endpoints.

## Endpoint Categories

- [First-Party Endpoints](#first-party-endpoints) - Core ad serving and proxying
- [Edge Cookie Endpoints](#edge-cookie-endpoints) - Identity sync and enrichment
- [Request Signing](#request-signing-endpoints) - Cryptographic signing and key management
- [TSJS Library](#tsjs-library-endpoint) - JavaScript library serving
- [Utility Endpoints](#utility-endpoints) - Optional operational helpers
- [Integration Endpoints](#integration-endpoints) - Third-party service proxying

---

## Utility Endpoints

### GET /\_ts/set-tester

Sets a first-party tester marker cookie for QA or troubleshooting workflows.

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

### GET /first-party/ad

Server-side ad rendering endpoint. Returns complete HTML for a single ad slot.

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `slot` | string | Yes | Ad slot identifier (matches ad unit code) |
| `w` | integer | Yes | Ad width in pixels |
| `h` | integer | Yes | Ad height in pixels |

**Response:**

- **Content-Type:** `text/html; charset=utf-8`
- **Body:** Complete HTML creative with first-party proxying applied

**Example:**

```bash
curl "https://edge.example.com/first-party/ad?slot=header-banner&w=728&h=90"
```

**Response Headers:**

No EC ID response header is emitted. EC identity is maintained with the `ts-ec` cookie.

**Use Cases:**

- Server-side ad rendering
- Direct iframe embedding
- First-party ad delivery

---

## Edge Cookie Endpoints

Partners are configured statically in `[[ec.partners]]` and loaded into an in-memory registry at startup. There is no runtime partner-registration endpoint and the legacy browser pixel sync endpoint has been removed; browser-resolved IDs are ingested through Prebid EID cookies.

---

### GET /\_ts/api/v1/identify

Returns EC identity plus the authenticated partner's UID and EID for the current user.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Request:**

- Uses `ts-ec` cookie and consent signals

**Response (example):**

```json
{
  "ec": "954d...e0c3.nZ1GxL",
  "consent": "ok",
  "degraded": false,
  "source_domain": "formally-vital-lion.edgecompute.app",
  "uid": "mock-user-123",
  "eid": {
    "source": "formally-vital-lion.edgecompute.app",
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

Server-to-server batch sync endpoint for writing EC ID to partner UID mappings. Mapping timestamps are retained in the request schema for compatibility, but they no longer order writes because EC identity entries do not store per-partner sync timestamps.

**Auth:** Bearer token (`Authorization: Bearer <partner-api-key>`)

**Batch processing behavior:**

- Every mapping is validated before any KV update. Validation errors retain their
  original input index.
- Valid mappings are grouped by normalized EC ID: only the 64-character hex
  prefix is lowercased; the six-character suffix remains case-sensitive.
- Groups are processed in first-valid-occurrence order, with one call to the
  CAS-protected KV update path per distinct normalized EC ID. Within a group, the
  last valid `partner_uid` in request order is persisted. An invalid mapping
  never replaces a group's final UID.
- A successful or unchanged update accepts every valid mapping in its group.
  Missing and withdrawn EC entries reject every valid mapping in their group as
  `ineligible`.
- If a KV infrastructure failure occurs, every valid mapping in the failing
  group and each unprocessed valid group is rejected as `kv_unavailable`; no
  later group is updated. Already processed groups keep their outcomes, and
  validation errors are preserved.
- `errors` is sorted by original input index. Therefore each input has exactly
  one outcome and `accepted + rejected` equals the number of submitted
  mappings. The endpoint returns `200 OK` only when all mappings are accepted;
  otherwise it returns `207 Multi-Status`.

Groupwise failure behavior is intentional: for `A(valid), B(valid), A(valid)`,
if A's group succeeds and B's group has an infrastructure failure, both A
mappings are accepted even though the second A appears after B in the input.

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

---

### POST /third-party/ad

Client-side auction endpoint for TSJS library.

**Request Body:**

```json
{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": {
        "banner": {
          "sizes": [
            [728, 90],
            [970, 250]
          ]
        }
      }
    }
  ],
  "config": {
    "debug": false
  }
}
```

**Response:**

```json
{
  "seatbid": [
    {
      "bid": [
        {
          "impid": "header-banner",
          "adm": "<html>...</html>",
          "price": 1.5,
          "w": 728,
          "h": 90
        }
      ]
    }
  ]
}
```

**Example:**

```bash
curl -X POST https://edge.example.com/third-party/ad \
  -H "Content-Type: application/json" \
  -d '{"adUnits":[{"code":"banner","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}'
```

---

### GET /first-party/proxy

Unified proxy for resources referenced by creatives (images, scripts, CSS, etc.).

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tsurl` | string | Yes | Target URL without query parameters (base URL) |
| `tstoken` | string | Yes | Base64 URL-safe SHA-256 digest of encrypted full target URL |
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

**Example:**

```bash
# Original URL: https://ad.doubleclick.net/pixel?id=123&type=view
# Signed proxy URL:
curl "https://edge.example.com/first-party/proxy?tsurl=https://ad.doubleclick.net/pixel&id=123&type=view&tstoken=abc123xyz..."
```

**Error Responses:**

- `400 Bad Request` - Missing or invalid `tstoken`
- `403 Forbidden` - Token validation failed
- `500 Internal Server Error` - Upstream fetch failed

---

### GET /first-party/click

Click tracking redirect endpoint.

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tsurl` | string | Yes | Target redirect URL without query parameters |
| `tstoken` | string | Yes | Base64 URL-safe SHA-256 digest of encrypted full target URL |
| `*` | any | No | Original target URL query parameters |

**Response:**

- **Status:** `302 Found`
- **Location:** Reconstructed target URL with EC ID injected

**Behavior:**

- Validates `tstoken` against reconstructed URL
- Injects `ts-ec` query parameter
- Logs click metadata (tsurl, referer, user agent)
- Does not proxy content (redirect only)

**Example:**

```bash
curl -I "https://edge.example.com/first-party/click?tsurl=https://advertiser.com/landing&campaign=123&tstoken=xyz..."
# → 302 Location: https://advertiser.com/landing?campaign=123&ts-ec=abc123
```

---

### GET/POST /first-party/sign

URL signing endpoint. Returns signed first-party proxy URL for a given target URL.

**Request Methods:** GET or POST

**GET Request:**

```bash
curl "https://edge.example.com/first-party/sign?url=https://external.com/pixel.gif"
```

**POST Request:**

```bash
curl -X POST https://edge.example.com/first-party/sign \
  -H "Content-Type: application/json" \
  -d '{"url":"https://external.com/pixel.gif"}'
```

**Response:**

```json
{
  "signed_url": "https://edge.example.com/first-party/proxy?tsurl=https://external.com/pixel.gif&tstoken=abc123..."
}
```

**Use Cases:**

- TSJS creative runtime (image/iframe proxying)
- Dynamic URL signing in client-side code
- Testing proxy URL generation

---

### POST /first-party/proxy-rebuild

URL mutation recovery endpoint. Rebuilds signed proxy URL after creative JavaScript modifies query parameters.

**Request Body:**

```json
{
  "tsclick": "https://edge.example.com/first-party/click?tsurl=https://advertiser.com&campaign=123&tstoken=original...",
  "add": {
    "utm_source": "banner"
  },
  "del": ["old_param"]
}
```

**Response:**

```json
{
  "url": "https://edge.example.com/first-party/click?tsurl=https://advertiser.com&campaign=123&utm_source=banner&tstoken=new..."
}
```

**Use Cases:**

- TSJS click guard (automatic URL repair)
- Handling creative JavaScript that modifies tracking URLs

---

## Request Signing Endpoints

### GET /.well-known/trusted-server.json

Returns the Trusted Server discovery document, which includes active public keys in JWKS
format for signature verification.

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
  "message": "Invalid signature"
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
  "new_kid": "ts-2025-01-15-A",
  "previous_kid": "ts-2025-01-14-A",
  "active_kids": ["ts-2025-01-15-A", "ts-2025-01-14-A"],
  "message": "Key rotation successful"
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
  "kid": "ts-2025-01-14-A",
  "active_kids": ["ts-2025-01-15-A"],
  "message": "Key deactivated successfully"
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

## TSJS Library Endpoint

### GET /static/tsjs=`<filename>`

Serves the TSJS (Trusted Server JavaScript) library.

**Path Pattern:** `/static/tsjs=<filename>?v=<hash>`

**Supported Filenames:**

- `tsjs-unified.js`
- `tsjs-unified.min.js`

**Query Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `v` | string | No | Cache-busting hash (SHA256 of bundle contents) |

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** TSJS bundle (IIFE format)
- **Headers:** ETag for caching

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

### Prebid Integration

#### GET /first-party/ad

See [First-Party Endpoints](#get-first-party-ad) above.

#### POST /third-party/ad

See [First-Party Endpoints](#post-third-party-ad) above.

#### GET /prebid.js (Optional)

Returns empty JavaScript to override Prebid.js when `script_handler` is configured.

**Configuration:**

```toml
[integrations.prebid]
script_handler = "/prebid.js"
```

**Response:**

- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** `// Prebid.js override by Trusted Server`
- **Cache:** `immutable, max-age=31536000`

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

## Error Responses

All endpoints use consistent error response format:

```json
{
  "error": "Error type",
  "message": "Detailed error description",
  "details": {
    "field": "Additional context"
  }
}
```

**Common HTTP Status Codes:**
| Code | Meaning | Common Causes |
|------|---------|---------------|
| 400 | Bad Request | Missing required parameters, invalid JSON |
| 401 | Unauthorized | Missing or invalid basic auth credentials |
| 403 | Forbidden | Invalid token signature, disabled integration |
| 404 | Not Found | Unknown endpoint, missing resource |
| 500 | Internal Server Error | Upstream service failure, configuration error |
| 502 | Bad Gateway | Backend service unavailable |
| 504 | Gateway Timeout | Backend service timeout exceeded |

---

## Authentication

### Basic Authentication

Endpoints under protected paths require HTTP Basic Authentication:

**Configuration:**

```toml
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "secure-password"
```

**Usage:**

```bash
curl -u admin:secure-password https://edge.example.com/_ts/admin/keys/rotate
```

**Protected Endpoints:**

- `/_ts/admin/keys/rotate`
- `/_ts/admin/keys/deactivate`
- Any paths matching configured `handlers` patterns

---

## Rate Limiting

Trusted Server relies on Fastly's built-in rate limiting. Configure in Fastly dashboard:

**Recommended Limits:**

- Public endpoints: 1000 req/min per IP
- Admin endpoints: 10 req/min per IP
- TSJS serving: 10000 req/min (highly cacheable)

---

## CORS

CORS headers are not enabled by default. Configure `response_headers` for cross-origin requests:

```toml
[response_headers]
Access-Control-Allow-Origin = "*"
Access-Control-Allow-Methods = "GET, POST, OPTIONS"
Access-Control-Allow-Headers = "Content-Type, Authorization"
```

---

## Next Steps

- Explore [Integrations Overview](./integrations-overview.md)
- Learn about [Configuration](./configuration.md)
- Review [Error Reference](./error-reference.md)
- Understand [Configuration Reference](./configuration.md)
