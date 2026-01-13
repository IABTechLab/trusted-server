# API Reference

Quick reference for all Trusted Server HTTP endpoints.

## Endpoint Categories

- [First-Party Endpoints](#first-party-endpoints) - Core ad serving and proxying
- [Request Signing](#request-signing-endpoints) - Cryptographic signing and key management
- [TSJS Library](#tsjs-library-endpoint) - JavaScript library serving
- [Integration Endpoints](#integration-endpoints) - Third-party service proxying

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
- `X-Synthetic-Trusted-Server` - Stable synthetic ID
- `X-Synthetic-Fresh` - One-time fresh ID

**Use Cases:**
- Server-side ad rendering
- Direct iframe embedding
- First-party ad delivery

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
          "sizes": [[728, 90], [970, 250]]
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
          "price": 1.50,
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
- Injects synthetic ID as `synthetic_id` query parameter
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
- **Location:** Reconstructed target URL with synthetic ID injected

**Behavior:**
- Validates `tstoken` against reconstructed URL
- Injects `synthetic_id` query parameter
- Logs click metadata (tsurl, referer, user agent)
- Does not proxy content (redirect only)

**Example:**
```bash
curl -I "https://edge.example.com/first-party/click?tsurl=https://advertiser.com/landing&campaign=123&tstoken=xyz..."
# → 302 Location: https://advertiser.com/landing?campaign=123&synthetic_id=abc123
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

### GET /.well-known/ts.jwks.json

Returns active public keys in JWKS (JSON Web Key Set) format for signature verification.

**Response:**
```json
{
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
```

**Example:**
```bash
curl https://edge.example.com/.well-known/ts.jwks.json
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

### POST /admin/keys/rotate

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
curl -X POST https://edge.example.com/admin/keys/rotate \
  -u admin:password \
  -H "Content-Type: application/json"
```

**Behavior:**
- Keeps both new and previous key active
- Updates `current-kid` to new key
- Preserves old key for graceful transition

See [Key Rotation Guide](./key-rotation.md) for workflow details.

---

### POST /admin/keys/deactivate

Deactivates or deletes a signing key.

**Authentication:** Requires basic auth

**Request Body:**
```json
{
  "kid": "ts-2025-01-14-A",
  "delete": false
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `kid` | string | Yes | Key ID to deactivate |
| `delete` | boolean | No | If true, permanently removes key (default: false) |

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
curl -X POST https://edge.example.com/admin/keys/deactivate \
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
<script src="/static/tsjs=tsjs-unified.min.js?v=a1b2c3d4..." id="trustedserver-js"></script>
```

**Module Selection:**
Controlled at build time via `TSJS_MODULES` environment variable:
```bash
TSJS_MODULES=creative,ext,permutive cargo build
```

See [Configuration](./configuration.md) for TSJS build options.

---

## Integration Endpoints

### Prebid Integration

#### GET /first-party/ad
See [First-Party Endpoints](#get-first-party-ad) above.

#### POST /third-party/ad
See [First-Party Endpoints](#post-third-party-ad) above.

#### GET /prebid.js, /prebid.min.js, etc. (Script Override)
Returns empty JavaScript to override Prebid.js scripts when the Prebid integration is enabled. By default, exact requests to `/prebid.js`, `/prebid.min.js`, `/prebidjs.js`, or `/prebidjs.min.js` will be intercepted and served an empty script.

**Configuration:**
```toml
[integrations.prebid]
# Default patterns (exact paths)
script_remove_patterns = ["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]

# Use wildcard patterns to match paths under a prefix
# script_remove_patterns = ["/static/prebid/*"]
```

**Response:**
- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** `// Script overridden by Trusted Server`
- **Cache:** `immutable, max-age=31536000`

---

### Permutive Integration

#### GET /integrations/permutive/sdk
Serves Permutive SDK from first-party domain.

**Response:**
- **Content-Type:** `application/javascript; charset=utf-8`
- **Body:** Permutive SDK fetched from `{organization_id}.edge.permutive.app/{workspace_id}-web.js`
- **Cache:** 1 hour (configurable via `cache_ttl_seconds`)

#### GET/POST /integrations/permutive/api/*
Proxies to `api.permutive.com`.

**Example:**
```bash
curl https://edge.example.com/integrations/permutive/api/settings
# → Proxies to https://api.permutive.com/settings
```

#### GET/POST /integrations/permutive/secure-signal/*
Proxies to `secure-signals.permutive.app`.

#### GET/POST /integrations/permutive/events/*
Proxies to `events.permutive.app` for event tracking.

#### GET/POST /integrations/permutive/sync/*
Proxies to `sync.permutive.com` for ID synchronization.

#### GET /integrations/permutive/cdn/*
Proxies to `cdn.permutive.com` for static assets.

---

### Testlight Integration

#### POST /integrations/testlight/auction
Testing auction endpoint with synthetic ID injection.

**Request Body:**
```json
{
  "user": {
    "id": null
  },
  "imp": [
    { "id": "slot-1" }
  ]
}
```

**Response:**
Proxies to configured endpoint with `user.id` populated with synthetic ID.

**Response Headers:**
- `X-Synthetic-Trusted-Server` - Stable synthetic ID
- `X-Synthetic-Fresh` - One-time fresh ID

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
path = "^/admin"
username = "admin"
password = "secure-password"
```

**Usage:**
```bash
curl -u admin:secure-password https://edge.example.com/admin/keys/rotate
```

**Protected Endpoints:**
- `/admin/keys/rotate`
- `/admin/keys/deactivate`
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
- Understand [Environment Variables](./environment-variables.md)
