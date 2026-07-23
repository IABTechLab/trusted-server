# Configuration

Learn how to configure Trusted Server for your deployment.

## Overview

Trusted Server uses a flexible configuration system based on:

1. **TOML Files** - `trusted-server.toml` for base configuration
2. **Environment Variables** - Build-time overrides with `TRUSTED_SERVER__` prefix (baked into the binary by `build.rs`)
3. **Fastly Stores** - KV/Config/Secret stores for runtime data

## Quick Start

### Minimal Configuration

Create `trusted-server.toml` in your project root:

```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "your-secure-secret-here"

[ec]
passphrase = "replace-with-32-plus-byte-random-secret"
```

### Environment Variable Overrides

Override any setting at build time. Environment variables are merged into the
config by `build.rs` and baked into the compiled binary — they are **not** read
at runtime.

```bash
# Format: TRUSTED_SERVER__SECTION__FIELD
export TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.publisher.com
export TRUSTED_SERVER__EC__PASSPHRASE=replace-with-32-plus-byte-random-secret
```

### Generate Secure Secrets

```bash
# Generate cryptographically random secrets
openssl rand -base64 32
```

### Strict Key Validation

Trusted Server rejects unknown TOML keys in runtime configuration. Before pushing
or upgrading config, remove stale fields and typos; otherwise config loading can
fail and the service will return its startup-error response.

## Configuration Files

| File                  | Purpose                         |
| --------------------- | ------------------------------- |
| `trusted-server.toml` | Main application configuration  |
| `fastly.toml`         | Fastly Compute service settings |
| `.env.dev`            | Local development overrides     |

## Key Sections

| Section             | Purpose                                      |
| ------------------- | -------------------------------------------- |
| `[publisher]`       | Domain, origin, proxy settings               |
| `[ec]`              | Edge Cookie (EC) ID generation               |
| `[tester_cookie]`   | Optional tester-cookie endpoint              |
| `[proxy]`           | Proxy SSRF allowlist and asset routes        |
| `[image_optimizer]` | Reusable Image Optimizer profile sets        |
| `[request_signing]` | Ed25519 request signing                      |
| `[auction]`         | Auction orchestration                        |
| `[integrations.*]`  | Partner integrations (Prebid, Next.js, etc.) |

## Example: Production Setup

```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
proxy_secret = "change-me-to-secure-value"

[ec]
passphrase = "replace-with-32-plus-byte-random-secret"

[request_signing]
enabled = true
config_store_id = "01GXXX"
secret_store_id = "01GYYY"

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "appnexus", "openx"]
client_side_bidders = ["rubicon"]
```

## Detailed Reference

The sections below consolidate the full configuration reference on this page.

## Environment Variable Overrides (Build-Time)

Environment variables with the `TRUSTED_SERVER__` prefix are merged into the
base TOML configuration by `build.rs` at compile time. The resulting config is
embedded in the binary. Changing an environment variable requires a rebuild.

### Format

```
TRUSTED_SERVER__SECTION__SUBSECTION__FIELD
```

**Rules**:

- Prefix: `TRUSTED_SERVER`
- Separator: `__` (double underscore)
- Case: UPPERCASE
- Sections: Match TOML hierarchy

### Examples

**Simple Field**:

```bash
TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
```

**Nested Field**:

```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://prebid.example/auction
```

**Array Field (JSON)**:

```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS='["kargo","rubicon"]'
```

**Array Field (Indexed)**:

```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__0=kargo
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__1=rubicon
```

**Array Field (Comma-Separated)**:

```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS=kargo,rubicon,appnexus
```

## Publisher Configuration

Core publisher settings for domain, origin, and proxy configuration.

### `[publisher]`

| Field                         | Type    | Required | Description                                                                             |
| ----------------------------- | ------- | -------- | --------------------------------------------------------------------------------------- |
| `domain`                      | String  | Yes      | Publisher's apex domain name                                                            |
| `cookie_domain`               | String  | Yes      | Domain for non-EC cookies (typically with leading dot)                                  |
| `origin_url`                  | String  | Yes      | Full URL of publisher origin server                                                     |
| `origin_host_header_override` | String  | No       | Outbound Host header to send while connecting to `origin_url`                           |
| `proxy_secret`                | String  | Yes      | Secret key for encrypting/signing proxy URLs                                            |
| `max_buffered_body_bytes`     | Integer | No       | Max bytes buffered when a publisher response is post-processed in full (default 16 MiB) |

> **Note:** EC cookies (`ts-ec`) derive their domain automatically as `.{domain}` and
> do not use `cookie_domain`. The `cookie_domain` field is used by other cookie helpers.

**Example**:

```toml
[publisher]
domain = "publisher.com"
cookie_domain = ".publisher.com"
origin_url = "https://origin.publisher.com"
# Optional: connect to origin_url but send this outbound Host header.
# origin_host_header_override = "www.publisher.com"
proxy_secret = "change-me-to-secure-random-value"
```

**Environment Override**:

```bash
TRUSTED_SERVER__PUBLISHER__DOMAIN=publisher.com
TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=.publisher.com
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://origin.publisher.com
TRUSTED_SERVER__PUBLISHER__ORIGIN_HOST_HEADER_OVERRIDE=www.publisher.com
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=your-secret-here
TRUSTED_SERVER__PUBLISHER__MAX_BUFFERED_BODY_BYTES=16777216
```

### Field Details

#### `domain`

**Purpose**: Primary domain for the publisher.

**Usage**:

- Used for publisher routing and logging
- Part of request context for proxy/origin handling

**Format**: Hostname without protocol or path

- ✅ `publisher.com`
- ✅ `www.publisher.com`
- ❌ `https://publisher.com`
- ❌ `publisher.com/path`

#### `cookie_domain`

**Purpose**: Domain scope for non-EC cookies.

**Usage**:

- Used by non-EC cookie helpers for domain scoping
- EC cookies (`ts-ec`) use a separate computed domain derived from `domain`

**Format**: Domain with optional leading dot

- `.publisher.com` - Shares across all subdomains
- `publisher.com` - Exact domain only

**Best Practice**: Use leading dot (`.publisher.com`) for subdomain sharing.

#### `origin_url`

**Purpose**: Backend origin server URL for publisher content.

**Usage**:

- Fallback proxy target for non-integration requests
- HTML processing rewrites origin URLs to request host
- Base for relative URL resolution

**Format**: Full URL with protocol

- ✅ `https://origin.publisher.com`
- ✅ `https://origin.publisher.com:8080`
- ✅ `http://192.168.1.1:9000`
- ❌ `origin.publisher.com` (missing protocol)

**Port Handling**: Includes port if non-standard (not 80/443).

#### `origin_host_header_override`

**Purpose**: Optional Host header to send to the publisher origin while still
connecting to the host in `origin_url`.

**Usage**:

- Connects, uses SNI, and checks certificates against `origin_url`
- Sends the configured value as the outbound HTTP `Host` header
- Useful when the origin endpoint expects a canonical publisher hostname

**Format**: Hostname with optional port, without protocol, path, query, or fragment

- ✅ `www.publisher.com`
- ✅ `www.publisher.com:8443`
- ❌ `https://www.publisher.com`
- ❌ `www.publisher.com/path`

**Default**: When omitted, Trusted Server sends the host from `origin_url`.

#### `proxy_secret`

**Purpose**: Secret key for HMAC-SHA256 signing of proxy URLs.

**Security**:

- Keep confidential and secure
- Rotate periodically (90 days recommended)
- Use cryptographically random values (32+ bytes)
- Never commit to version control

**Generation**:

```bash
# Generate secure random secret
openssl rand -base64 32
```

**Usage**:

- Signs `/first-party/proxy` URLs
- Signs `/first-party/click` URLs
- Validates incoming proxy requests
- Prevents URL tampering

::: danger Security Warning
Changing `proxy_secret` invalidates all existing signed URLs. Plan rotations carefully and use graceful transition periods.
:::

#### `max_buffered_body_bytes`

**Purpose**: Upper bound on the in-memory buffer used when a publisher origin
response must be processed in full (HTML rewriting and integration injection)
instead of streamed.

**Usage**:

- Caps the _decoded, post-rewrite_ output buffer for any buffered publisher
  response on both the legacy and EdgeZero paths.
- Exceeding the cap fails the response (mapped to a 5xx proxy error) rather than
  allocating past the limit, preventing Wasm-heap exhaustion on highly
  compressible documents.

**Default**: `16777216` (16 MiB).

**Effective Fastly limit**: On Fastly the practical ceiling for a publisher page
is lower. The platform HTTP client rejects any origin response whose raw (still
compressed) body exceeds **10 MiB** before this buffer is filled, so raising the
value only helps highly compressible pages whose decoded size exceeds 16 MiB
while their compressed origin body stays under 10 MiB. Raising it above ~10 MiB
does not lift the platform cap for uncompressed pages.

**Minimum**: Must be at least `1`. A value of `0` is rejected at startup because
a zero-byte cap fails every non-empty buffered response.

**Environment Override**:

```bash
TRUSTED_SERVER__PUBLISHER__MAX_BUFFERED_BODY_BYTES=16777216
```

## Tester Cookie Configuration

Settings for the optional tester-cookie endpoints. This feature is disabled by
default and should only be enabled for intentional QA or troubleshooting flows.

### `[tester_cookie]`

| Field     | Type    | Required | Description                                        |
| --------- | ------- | -------- | -------------------------------------------------- |
| `enabled` | Boolean | No       | Enables routes to set and clear `ts-tester` cookie |

When enabled, `GET /_ts/set-tester` returns `204 No Content` and sets:

```http
Set-Cookie: ts-tester=true; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax
Cache-Control: no-store, private
```

`GET /_ts/clear-tester` returns `204 No Content` and clears the cookie:

```http
Set-Cookie: ts-tester=; Domain=<publisher.cookie_domain>; Path=/; Secure; SameSite=Lax; Max-Age=0
Cache-Control: no-store, private
```

When disabled, both routes return `404 Not Found` and do not set a cookie.

::: warning
The cookie is scoped with `[publisher].cookie_domain`, not the EC-specific
computed domain. Keep `cookie_domain` aligned with the browser scope where your
QA tooling expects to read `ts-tester`.
:::

**Example**:

```toml
[tester_cookie]
enabled = true
```

**Environment Override**:

```bash
TRUSTED_SERVER__TESTER_COOKIE__ENABLED=true
```

## EC Configuration

Settings for generating privacy-preserving Edge Cookie identifiers. The `ec_store` KV store is the only KV-backed EC lifecycle store; it holds identity graph state, minimal consent metadata, source-domain keyed partner UIDs, and withdrawal tombstones. Consent configuration controls request-local interpretation and forwarding, not separate KV persistence.

### `[ec]`

| Field                     | Type           | Required | Description                                                             |
| ------------------------- | -------------- | -------- | ----------------------------------------------------------------------- |
| `passphrase`              | String         | Yes      | Publisher passphrase used as HMAC key                                   |
| `ec_store`                | String or null | No       | Fastly KV store name for EC identity graph and withdrawal state         |
| `pull_sync_concurrency`   | Integer        | No       | Maximum concurrent pull-sync requests per organic response              |
| `cluster_trust_threshold` | Integer        | No       | Cluster size threshold for identity trust decisions                     |
| `cluster_recheck_secs`    | Integer        | No       | Legacy compatibility setting; cluster rechecks no longer use timestamps |
| `partners`                | Array          | No       | Static partner registry entries                                         |

::: tip Partner keying
`source_domain` is the canonical partner key. It matches incoming OpenRTB EID `source` values and is also used as the EC KV `ids` map key.
:::

**Example**:

```toml
[ec]
passphrase = "replace-with-32-plus-byte-random-secret"
ec_store = "ec_identity_store"

[[ec.partners]]
name = "Mocktioneer SSP"
source_domain = "mocktioneer.example"
api_token = "partner-api-token-32-bytes-minimum"
bidstream_enabled = true
```

**Environment Override**:

```bash
TRUSTED_SERVER__EC__PASSPHRASE=your-secret
TRUSTED_SERVER__EC__EC_STORE=ec_identity_store
```

### Field Details

#### `passphrase`

**Purpose**: Publisher passphrase used as HMAC key for EC ID generation.

**Security**:

- Must be non-empty
- Rotate periodically for security
- Store securely (environment variable recommended)

**Generation**:

```bash
# Generate secure random key
openssl rand -hex 32
```

**Validation**: Application startup fails if:

- Empty string

## Response Headers

Custom headers added to all responses.

### `[response_headers]`

**Purpose**: Add custom HTTP headers to every response.

**Format**: Key-value pairs

**Example**:

```toml
[response_headers]
X-Custom-Header = "custom value"
X-Publisher-ID = "pub-12345"
X-Environment = "production"
Cache-Control = "public, max-age=3600"
```

**Environment Override**:

Use a JSON object to preserve header name casing and hyphens:

```bash
TRUSTED_SERVER__RESPONSE_HEADERS='{"X-Robots-Tag": "noindex", "X-Custom-Header": "custom value"}'
```

::: tip Why JSON?
Individual env var keys like `TRUSTED_SERVER__RESPONSE_HEADERS__X_CUSTOM_HEADER` lose hyphens and casing (becoming `x_custom_header`). The JSON format preserves exact header names.
:::

**Use Cases**:

- Custom tracking headers
- Cache control overrides
- Debugging identifiers
- CORS headers (if needed)

::: warning Header Precedence
Custom headers may be overwritten by application logic. Standard headers (`Content-Type`, `Content-Length`) are controlled by the application.
:::

## Request Signing

Configuration for Ed25519 request signing and JWKS management.

### `[request_signing]`

| Field             | Type    | Required            | Description                             |
| ----------------- | ------- | ------------------- | --------------------------------------- |
| `enabled`         | Boolean | No (default: false) | Enable request signing features         |
| `config_store_id` | String  | If enabled          | Fastly Config Store ID for JWKS         |
| `secret_store_id` | String  | If enabled          | Fastly Secret Store ID for private keys |

**Example**:

```toml
[request_signing]
enabled = true
config_store_id = "01GXXX"  # From Fastly dashboard
secret_store_id = "01GYYY"  # From Fastly dashboard
```

**Environment Override**:

```bash
TRUSTED_SERVER__REQUEST_SIGNING__ENABLED=true
TRUSTED_SERVER__REQUEST_SIGNING__CONFIG_STORE_ID=01GXXX
TRUSTED_SERVER__REQUEST_SIGNING__SECRET_STORE_ID=01GYYY
```

### Store Setup

**Config Store** (for public keys):

```bash
# Create store
fastly config-store create --name=jwks_store

# Get store ID
fastly config-store list
```

**Secret Store** (for private keys):

```bash
# Create store
fastly secret-store create --name=signing_keys

# Get store ID
fastly secret-store list
```

**Local Dev Setup** (`fastly.toml`):

```toml
[local_server.config_stores]
  [local_server.config_stores.jwks_store]
    file = "test-data/jwks_store.json"

[local_server.secret_stores]
  [local_server.secret_stores.signing_keys]
    file = "test-data/signing_keys.json"
```

See [Request Signing](/guide/request-signing) and [Key Rotation](/guide/key-rotation) for usage.

## Basic Authentication Handlers

Path-based HTTP Basic Authentication.

### `[[handlers]]`

**Purpose**: Protect specific paths with username/password authentication.

**Format**: Array of handler objects

| Field      | Type           | Required | Description                       |
| ---------- | -------------- | -------- | --------------------------------- |
| `path`     | String (Regex) | Yes      | Regular expression matching paths |
| `username` | String         | Yes      | HTTP Basic Auth username          |
| `password` | String         | Yes      | HTTP Basic Auth password          |

**Example**:

```toml
# Single handler
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "secure-password"

# Multiple handlers
[[handlers]]
path = "^/secure"
username = "user1"
password = "pass1"

[[handlers]]
path = "^/api/private"
username = "api-user"
password = "api-pass"
```

**Environment Override**:

```bash
# Handler 0
TRUSTED_SERVER__HANDLERS__0__PATH="^/_ts/admin"
TRUSTED_SERVER__HANDLERS__0__USERNAME="admin"
TRUSTED_SERVER__HANDLERS__0__PASSWORD="secure-password"

# Handler 1
TRUSTED_SERVER__HANDLERS__1__PATH="^/api/private"
TRUSTED_SERVER__HANDLERS__1__USERNAME="api-user"
TRUSTED_SERVER__HANDLERS__1__PASSWORD="api-pass"
```

### Path Patterns

**Regex Syntax**: Standard Rust regex patterns

**Examples**:

```toml
# Exact path
path = "^/_ts/admin$"  # Only /_ts/admin

# Prefix match
path = "^/_ts/admin"   # /_ts/admin, /_ts/admin/users, /_ts/admin/settings

# Multiple paths
path = "^/(admin|secure|private)"

# File extension
path = "\\.pdf$"   # All PDF files

# Complex pattern
path = "^/api/v[0-9]+/private"  # /api/v1/private, /api/v2/private
```

**Validation**: Application startup fails if regex is invalid.

### Security Considerations

**Password Storage**:

- Stored in plain text in config
- Use environment variables in production
- Rotate passwords regularly
- Consider using Fastly Secret Store

**Limitations**:

- HTTP Basic Auth (not OAuth/JWT)
- Single username/password per path
- No role-based access control
- No rate limiting (add at edge)

::: warning Production Use
For production, store credentials in environment variables:

```bash
TRUSTED_SERVER__HANDLERS__0__PASSWORD=$(cat /run/secrets/admin_password)
```

:::

## URL Rewrite Configuration

Control which domains are excluded from first-party rewriting.

### `[rewrite]`

| Field             | Type          | Required         | Description               |
| ----------------- | ------------- | ---------------- | ------------------------- |
| `exclude_domains` | Array[String] | No (default: []) | Domains to skip rewriting |

**Example**:

```toml
[rewrite]
exclude_domains = [
    "*.cdn.trusted-partner.com",  # Wildcard
    "first-party.publisher.com",  # Exact match
    "localhost",                  # Development
]
```

**Environment Override**:

```bash
# JSON array
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS='["*.cdn.example.com","localhost"]'

# Indexed
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS__0="*.cdn.example.com"
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS__1="localhost"

# Comma-separated
TRUSTED_SERVER__REWRITE__EXCLUDE_DOMAINS="*.cdn.example.com,localhost"
```

### Pattern Matching

**Wildcard Patterns** (`*`):

```toml
"*.cdn.example.com"
```

Matches:

- ✅ `assets.cdn.example.com`
- ✅ `images.cdn.example.com`
- ✅ `cdn.example.com` (base domain)
- ❌ `cdn.example.com.evil.com` (different domain)

**Exact Patterns** (no `*`):

```toml
"api.example.com"
```

Matches:

- ✅ `api.example.com`
- ❌ `www.api.example.com`
- ❌ `api.example.com.evil.com`

### Use Cases

**Trusted Partners**:

```toml
exclude_domains = ["*.approved-cdn.com"]
```

**First-Party Resources**:

```toml
exclude_domains = ["assets.publisher.com", "static.publisher.com"]
```

**Development**:

```toml
exclude_domains = ["localhost", "127.0.0.1", "*.local"]
```

**Performance** (already first-party):

```toml
exclude_domains = ["*.publisher.com"]  # Skip unnecessary proxying
```

See [Creative Processing](/guide/creative-processing#exclude-domains) for details.

## Proxy Configuration

Controls first-party proxy security settings and path-based asset routes.

### `[proxy]`

| Field               | Type          | Required             | Description                                            |
| ------------------- | ------------- | -------------------- | ------------------------------------------------------ |
| `allowed_domains`   | Array[String] | No (default: `[]`)   | Redirect destinations the proxy is permitted to follow |
| `certificate_check` | Boolean       | No (default: `true`) | Verify TLS certificates when proxying HTTPS origins    |
| `asset_routes`      | Array[Table]  | No (default: `[]`)   | Path prefixes proxied directly to configured origins   |

**Example**:

```toml
[proxy]
allowed_domains = [
  "tracker.com",         # Exact match
  "*.adserver.com",      # Wildcard: adserver.com and all subdomains
  "*.trusted-cdn.net",
]
```

**Environment Override**:

```bash
# JSON array
TRUSTED_SERVER__PROXY__ALLOWED_DOMAINS='["tracker.com","*.adserver.com"]'

# Indexed
TRUSTED_SERVER__PROXY__ALLOWED_DOMAINS__0="tracker.com"
TRUSTED_SERVER__PROXY__ALLOWED_DOMAINS__1="*.adserver.com"

# Comma-separated
TRUSTED_SERVER__PROXY__ALLOWED_DOMAINS="tracker.com,*.adserver.com"
```

### Field Details

#### `allowed_domains`

**Purpose**: Allowlist of redirect destinations the proxy is permitted to follow.

**Behavior**: When the proxy receives an HTTP redirect (301/302/303/307/308) during a request to `/first-party/proxy`, the redirect target host is checked against this list. A redirect whose host is not matched is blocked with a 403 error.

**Default — open mode**: When `allowed_domains` is absent or empty, every redirect destination is allowed. This default is intentional for zero-config development but should not be used in production.

**Pattern Matching**:

| Pattern         | Matches                                             | Does not match     |
| --------------- | --------------------------------------------------- | ------------------ |
| `tracker.com`   | `tracker.com`                                       | `sub.tracker.com`  |
| `*.tracker.com` | `tracker.com`, `sub.tracker.com`, `a.b.tracker.com` | `evil-tracker.com` |

- `"example.com"` — exact match only.
- `"*.example.com"` — matches the base domain and any subdomain at any depth.
- Matching is case-insensitive; entries are normalized to lowercase at startup.
- Blank entries are ignored.
- The `*` wildcard requires a dot boundary: `*.example.com` does **not** match `evil-example.com`.

::: danger Production Recommendation
Always configure `allowed_domains` in production. Without an explicit allowlist, a signed proxy URL can be used to follow redirects to arbitrary hosts, creating an SSRF risk.

```toml
[proxy]
allowed_domains = [
  "*.your-ad-network.com",
  "tracker.your-partner.com",
]
```

:::

See [First-Party Proxy](/guide/first-party-proxy#proxy-allowlist) for usage details.

#### `certificate_check`

**Purpose**: Control TLS certificate verification for HTTPS proxy and asset-route origins.

**Default**: `true`

Set this to `false` only for local development with self-signed certificates.

### `[[proxy.asset_routes]]`

Asset routes proxy selected first-party paths to an alternate asset origin without requiring signed `/first-party/proxy` URLs.

| Field             | Type   | Required | Description                                     |
| ----------------- | ------ | -------- | ----------------------------------------------- |
| `prefix`          | String | Yes      | Request path prefix to match                    |
| `origin_url`      | String | Yes      | Absolute `http` or `https` origin URL           |
| `path_pattern`    | String | No       | Regex matched against the incoming request path |
| `target_path`     | String | No       | Replacement path used with `path_pattern`       |
| `auth`            | Table  | No       | Optional origin authentication                  |
| `image_optimizer` | Table  | No       | Optional route-level Image Optimizer settings   |

**Example**:

```toml
[[proxy.asset_routes]]
prefix = "/assets/"
origin_url = "https://assets.example.com"
```

**Path rewrite example**:

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://assets-cdn.example.com"
path_pattern = "^/\\.image/(.*)/[^/]+\\.([^/.]+)$"
target_path = "/image/upload/$1.$2"
```

**Behavior**:

- Only `GET` and `HEAD` requests use asset routes.
- Built-in and integration routes take precedence.
- The longest matching asset-route prefix wins.
- `path_pattern` and `target_path` must be configured together.
- `origin_url` must not include userinfo, a path, a query string, or a fragment.
- Unsafe origin response headers such as `Set-Cookie` are stripped before the response reaches the browser.

### `[proxy.asset_routes.auth]`

The first supported origin auth type is `s3_sigv4`.

| Field               | Type   | Required | Default             | Description                                     |
| ------------------- | ------ | -------- | ------------------- | ----------------------------------------------- |
| `type`              | String | Yes      | none                | Must be `s3_sigv4`                              |
| `region`            | String | Yes      | none                | AWS region used in the SigV4 credential scope   |
| `secret_store`      | String | No       | `s3-auth`           | Runtime secret store containing AWS credentials |
| `access_key_id`     | String | No       | `access_key_id`     | Secret key containing the AWS access key ID     |
| `secret_access_key` | String | No       | `secret_access_key` | Secret key containing the AWS secret access key |
| `session_token`     | String | No       | unset               | Optional secret key containing a session token  |
| `origin_query`      | String | No       | route default       | `preserve` or `strip`                           |

**Example**:

```toml
[[proxy.asset_routes]]
prefix = "/.image/"
origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

[proxy.asset_routes.auth]
type = "s3_sigv4"
region = "us-east-1"
origin_query = "strip"
secret_store = "s3-auth"
access_key_id = "access_key_id"
secret_access_key = "secret_access_key"
# session_token = "session_token"
```

S3 auth uses header-based AWS SigV4 with `UNSIGNED-PAYLOAD`. It is scoped to read-only asset requests and expects `origin_url` to use the S3 host that AWS validates. Credentials are cached per process by configured secret names after the first successful read.

Effective `origin_query` precedence is auth-level `origin_query`, then enabled Image Optimizer `origin_query`, then the route default.

### `[proxy.asset_routes.image_optimizer]`

Route-level Image Optimizer configuration selects a reusable profile set.

| Field          | Type    | Required         | Default              | Description                                                                 |
| -------------- | ------- | ---------------- | -------------------- | --------------------------------------------------------------------------- |
| `enabled`      | Boolean | No               | `true`               | Enable Image Optimizer for the route                                        |
| `region`       | String  | Yes when enabled | none                 | Fastly IO processing region, such as `us_east`                              |
| `profile_set`  | String  | Yes when enabled | none                 | Name under `[image_optimizer.profile_sets.*]`                               |
| `origin_query` | String  | No               | `strip` when enabled | `preserve` or `strip`; effective `preserve` is rejected while IO is enabled |

**Example**:

```toml
[proxy.asset_routes.image_optimizer]
enabled = true
region = "us_east"
profile_set = "default_images"
```

### `[image_optimizer.profile_sets.<name>]`

Profile sets convert small request query controls into a closed set of Image Optimizer parameters.

| Field                | Type   | Required | Default       | Description                                      |
| -------------------- | ------ | -------- | ------------- | ------------------------------------------------ |
| `base_params`        | String | No       | `""`          | Params applied before profile-specific params    |
| `default_profile`    | String | No       | `default`     | Profile used when no profile is requested        |
| `unknown_profile`    | String | No       | `use_default` | `use_default` or `reject`                        |
| `profile_param`      | String | No       | `profile`     | Query parameter containing the profile name      |
| `aspect_ratio_param` | String | No       | `ar`          | Query parameter containing aspect ratio          |
| `debug_param`        | String | No       | `_io_debug`   | Query parameter that disables IO when set to `1` |

Profile values live under `[image_optimizer.profile_sets.<name>.profiles]` and use query-string syntax.

```toml
[image_optimizer.profile_sets.default_images]
base_params = "quality=70&resize-filter=bicubic"
default_profile = "default"
unknown_profile = "use_default"
profile_param = "profile"
aspect_ratio_param = "ar"
debug_param = "_io_debug"

[image_optimizer.profile_sets.default_images.profiles]
default = "width=1920"
medium = "format=auto&width=828"
thumbnail = "width=150&crop=1:1,smart"
```

Supported profile parameters are `quality`, `resize-filter`, `format`, `width`, `height`, and `crop`. Unknown profile parameters fail configuration validation.

### `[image_optimizer.profile_sets.<name>.aspect_ratios]`

| Field      | Type          | Required | Description                                         |
| ---------- | ------------- | -------- | --------------------------------------------------- |
| `allowed`  | Array[String] | No       | Allowed query values such as `1-1` or `16-9`        |
| `profiles` | Array[String] | No       | Defined profiles that accept aspect-ratio overrides |

```toml
[image_optimizer.profile_sets.default_images.aspect_ratios]
allowed = ["1-1", "16-9", "4-3"]
profiles = ["medium", "thumbnail"]
```

### `[image_optimizer.profile_sets.<name>.crop_offsets]`

| Field          | Type           | Required | Default                | Description                                  |
| -------------- | -------------- | -------- | ---------------------- | -------------------------------------------- |
| `enabled`      | Boolean        | No       | `true`                 | Enable offset bucketing                      |
| `x_param`      | String         | No       | `x`                    | Query parameter for x-axis offset            |
| `y_param`      | String         | No       | `y`                    | Query parameter for y-axis offset            |
| `buckets`      | Array[Integer] | No       | `[10, 30, 50, 70, 90]` | Offset buckets in `0..=100`                  |
| `default`      | Integer        | No       | `50`                   | Offset used when input is missing or invalid |
| `when_missing` | String         | No       | `smart`                | `smart` or `none` when neither offset exists |

```toml
[image_optimizer.profile_sets.default_images.crop_offsets]
enabled = true
x_param = "x"
y_param = "y"
buckets = [10, 30, 50, 70, 90]
default = 50
when_missing = "smart"
```

See [Asset Routes](/guide/asset-routes) for request flow, S3 auth details, and Image Optimizer behavior.

## Integration Configurations

Settings for built-in integrations (Prebid, Next.js, Osano, Permutive, Testlight). For other
integrations (APS, Didomi, Lockr, GAM, etc.), see the relevant integration guides.

### Common Fields

All integrations support an `enabled` flag. Defaults vary by integration and only
apply when the integration section exists in `trusted-server.toml`.

| Field     | Type    | Description                    |
| --------- | ------- | ------------------------------ |
| `enabled` | Boolean | Enable/disable the integration |

### Prebid Integration

**Section**: `[integrations.prebid]`

| Field                      | Type          | Default                                                                | Description                                                                                                                                           |
| -------------------------- | ------------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `enabled`                  | Boolean       | `true`                                                                 | Enable Prebid integration                                                                                                                             |
| `server_url`               | String        | Required                                                               | Prebid Server endpoint URL                                                                                                                            |
| `timeout_ms`               | Integer       | `1000`                                                                 | Request timeout in milliseconds                                                                                                                       |
| `bidders`                  | Array[String] | `["mocktioneer"]`                                                      | List of enabled bidders                                                                                                                               |
| `bid_param_overrides`      | Table         | `{}`                                                                   | Static per-bidder param overrides; normalized into the canonical override-rule engine and shallow-merged into bidder params                           |
| `bid_param_zone_overrides` | Table         | `{}`                                                                   | Per-bidder, per-zone param overrides; normalized into the canonical override-rule engine and shallow-merged into bidder params                        |
| `bid_param_override_rules` | Array[Table]  | `[]`                                                                   | Canonical ordered override rules with `when` matchers and `set` objects; evaluated after compatibility fields so later rules win on conflicts         |
| `suppress_nurl`            | Boolean       | `false`                                                                | Strip `nurl` and `burl` from every PBS bid when the PBS deployment fires win/billing notifications server-side                                        |
| `suppress_nurl_bidders`    | Array[String] | `[]`                                                                   | Bidder seats whose `nurl` and `burl` should be stripped while preserving client-side win/billing pixels for other bidders                             |
| `debug`                    | Boolean       | `false`                                                                | Enable debug mode (sets `ext.prebid.debug` and `returnallbidstatus`; surfaces debug metadata in responses)                                            |
| `test_mode`                | Boolean       | `false`                                                                | Set OpenRTB `test: 1` flag for non-billable test traffic (independent of `debug`)                                                                     |
| `debug_query_params`       | String        | `None`                                                                 | Extra query params appended for debugging                                                                                                             |
| `client_side_bidders`      | Array[String] | `[]`                                                                   | Bidders that run client-side via native Prebid.js adapters instead of server-side (see [Prebid docs](/guide/integrations/prebid#client-side-bidders)) |
| `script_patterns`          | Array[String] | `["/prebid.js", "/prebid.min.js", "/prebidjs.js", "/prebidjs.min.js"]` | URL patterns for Prebid script interception                                                                                                           |

**Example**:

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example/openrtb2/auction"
timeout_ms = 1200
bidders = ["kargo", "appnexus", "openx"]
debug = false
# test_mode = false

# Bidders that run client-side via native Prebid.js adapters
client_side_bidders = ["rubicon"]

# Customize script interception (optional)
script_patterns = ["/prebid.js", "/prebid.min.js"]

[integrations.prebid.bid_param_overrides.criteo]
networkId = 99999
pubid = "server-pub"

[integrations.prebid.bid_param_zone_overrides.kargo]
header = { placementId = "_s2sHeaderPlacement" }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_s2sHeaderPlacement" }
```

**Environment Override**:

```bash
TRUSTED_SERVER__INTEGRATIONS__PREBID__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__PREBID__SERVER_URL=https://prebid.example/auction
TRUSTED_SERVER__INTEGRATIONS__PREBID__TIMEOUT_MS=1200
TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS=kargo,appnexus,openx
TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_OVERRIDES='{"criteo":{"networkId":99999,"pubid":"server-pub"}}'
TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_ZONE_OVERRIDES='{"kargo":{"header":{"placementId":"_s2sHeaderPlacement"}}}'
TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_OVERRIDE_RULES='[{"when":{"bidder":"kargo","zone":"header"},"set":{"placementId":"_s2sHeaderPlacement"}}]'
TRUSTED_SERVER__INTEGRATIONS__PREBID__CLIENT_SIDE_BIDDERS=rubicon
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG=false
TRUSTED_SERVER__INTEGRATIONS__PREBID__TEST_MODE=false
TRUSTED_SERVER__INTEGRATIONS__PREBID__DEBUG_QUERY_PARAMS=debug=1
TRUSTED_SERVER__INTEGRATIONS__PREBID__SCRIPT_PATTERNS='["/prebid.js","/prebid.min.js"]'
```

**Script Pattern Matching**:

The `script_patterns` configuration determines which Prebid scripts are intercepted and replaced with empty JavaScript responses. This prevents client-side Prebid.js from loading when using server-side bidding.

- **Suffix matching**: `/prebid.min.js` matches any URL ending with that path
- **Wildcard patterns**: `/static/prebid/*` matches paths under that prefix
- **Disable interception**: Set `script_patterns = []` to keep client-side Prebid

See [Prebid Integration](/guide/integrations/prebid) for full details.

**Bid Param Override Surfaces**:

- `bid_param_overrides`: Static per-bidder shallow-merge overrides.
- `bid_param_zone_overrides`: Per-bidder, per-zone shallow-merge overrides.
- `bid_param_override_rules`: Canonical ordered rules with `when` matchers and `set` objects.

Compatibility fields are normalized into the same runtime engine as canonical rules. Explicit `bid_param_override_rules` run after compatibility-derived rules, so later canonical rules win on conflicts.

### Next.js Integration

**Section**: `[integrations.nextjs]`

| Field                        | Type          | Default                 | Description                   |
| ---------------------------- | ------------- | ----------------------- | ----------------------------- |
| `enabled`                    | Boolean       | `false`                 | Enable Next.js integration    |
| `rewrite_attributes`         | Array[String] | `["href","link","url"]` | Attributes to rewrite         |
| `max_combined_payload_bytes` | Integer       | `10485760`              | Max combined RSC payload size |

**Example**:

```toml
[integrations.nextjs]
enabled = true
rewrite_attributes = ["href", "link", "url", "src"]
max_combined_payload_bytes = 10485760
```

**Environment Override**:

```bash
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__ENABLED=true
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__REWRITE_ATTRIBUTES=href,link,url,src
TRUSTED_SERVER__INTEGRATIONS__NEXTJS__MAX_COMBINED_PAYLOAD_BYTES=10485760
```

### Osano Integration

**Section**: `[integrations.osano]`

| Field     | Type    | Default | Description                             |
| --------- | ------- | ------- | --------------------------------------- |
| `enabled` | Boolean | `false` | Enable the Osano browser consent mirror |

**Example**:

```toml
[integrations.osano]
enabled = true
```

**Environment Override**:

```bash
TRUSTED_SERVER__INTEGRATIONS__OSANO__ENABLED=true
```

The Osano mirror runs in the browser, so consent cookies it writes are available to Trusted Server on requests after the page where Osano consent APIs become ready. See [Osano Integration](/guide/integrations/osano) for details.

### Permutive Integration

**Section**: `[integrations.permutive]`

| Field                     | Type    | Default                                | Description                      |
| ------------------------- | ------- | -------------------------------------- | -------------------------------- |
| `enabled`                 | Boolean | `true`                                 | Enable Permutive integration     |
| `organization_id`         | String  | Required                               | Permutive organization ID        |
| `workspace_id`            | String  | Required                               | Permutive workspace ID           |
| `project_id`              | String  | `""`                                   | Permutive project ID             |
| `api_endpoint`            | String  | `https://api.permutive.com`            | Permutive API URL                |
| `secure_signals_endpoint` | String  | `https://secure-signals.permutive.app` | Secure signals URL               |
| `cache_ttl_seconds`       | Integer | `3600`                                 | Cache TTL in seconds             |
| `rewrite_sdk`             | Boolean | `true`                                 | Rewrite Permutive SDK references |

**Example**:

```toml
[integrations.permutive]
enabled = true
organization_id = "org-12345"
workspace_id = "ws-67890"
project_id = "proj-abcde"
api_endpoint = "https://api.permutive.com"
secure_signals_endpoint = "https://secure-signals.permutive.app"
cache_ttl_seconds = 7200
rewrite_sdk = true
```

### Testlight Integration

**Section**: `[integrations.testlight]`

| Field             | Type    | Default                                     | Description                         |
| ----------------- | ------- | ------------------------------------------- | ----------------------------------- |
| `enabled`         | Boolean | `true`                                      | Enable Testlight integration        |
| `endpoint`        | String  | Required                                    | Testlight auction endpoint          |
| `timeout_ms`      | Integer | `1000`                                      | Request timeout in milliseconds     |
| `shim_src`        | String  | `/static/tsjs=tsjs-unified.min.js?v=<hash>` | Script source for testlight shim    |
| `rewrite_scripts` | Boolean | `false`                                     | Rewrite Testlight script references |

**Example**:

```toml
[integrations.testlight]
enabled = true
endpoint = "https://testlight.example/openrtb2/auction"
timeout_ms = 1500
rewrite_scripts = true
```

## Auction Configuration

Settings for the auction orchestrator that coordinates multiple bid providers.

### `[auction]`

| Field            | Type          | Default            | Description                                                 |
| ---------------- | ------------- | ------------------ | ----------------------------------------------------------- |
| `enabled`        | Boolean       | `false`            | Enable the auction orchestrator                             |
| `providers`      | Array[String] | `[]`               | Provider names that participate (e.g., `["prebid", "aps"]`) |
| `mediator`       | String        | Optional           | Mediator provider name (runs parallel mediation when set)   |
| `timeout_ms`     | Integer       | `2000`             | Auction timeout in milliseconds                             |
| `creative_store` | String        | `"creative_store"` | Deprecated; creatives are now delivered inline              |

**Example**:

```toml
[auction]
enabled = true
providers = ["aps", "prebid"]
timeout_ms = 2000

[integrations.aps]
enabled = true
pub_id = "example-publisher"
endpoint = "https://aps.example.com/e/dtb/bid"

[integrations.prebid]
enabled = true
server_url = "https://prebid-server.example.com/openrtb2/auction"
```

**Environment Override**:

```bash
TRUSTED_SERVER__AUCTION__ENABLED=true
TRUSTED_SERVER__AUCTION__PROVIDERS=aps,prebid
TRUSTED_SERVER__AUCTION__PROVIDERS__0=aps
TRUSTED_SERVER__AUCTION__PROVIDERS__1=prebid
TRUSTED_SERVER__AUCTION__MEDIATOR=adserver_mock
TRUSTED_SERVER__AUCTION__TIMEOUT_MS=2000
TRUSTED_SERVER__AUCTION__CREATIVE_STORE=creative_store
```

## Creative Opportunities Configuration

### `[creative_opportunities]`

Defines the ad slots the trusted server offers on a page: which pages each slot
appears on (`page_patterns`), its supported sizes (`formats`), and the GAM ad
unit it maps to (`gam_unit_path`).

```toml
[creative_opportunities]
gam_network_id = "123456789"
price_granularity = "dense"

# Shared placeholder value for the site root ("/") — see {section} below.
section_root = "home"

[[creative_opportunities.slot]]
id = "ad-header"
gam_unit_path = "/{network_id}/example/{section}"
page_patterns = ["/", "/news/*", "/reviews/*"]
formats = [{ width = 728, height = 90 }]
```

### `gam_unit_path` templating

`gam_unit_path` is a template. A publisher whose ad unit varies by site section
expresses that in **one** slot rule instead of one rule per (slot × section).

Supported placeholders:

| Placeholder    | Resolves to                                                    |
| -------------- | -------------------------------------------------------------- |
| `{network_id}` | `gam_network_id`                                               |
| `{slot_id}`    | the slot's `id`                                                |
| `{section}`    | first path segment of the request (see derivation rules below) |

A template with **no** placeholders is used verbatim. A slot with **no**
`gam_unit_path` falls back to `/<network_id>/<slot_id>`. Both preserve the
pre-templating behavior, so existing static configs are unchanged.

### `{section}` derivation

`{section}` is derived from the request path at request time:

- It is the **first non-empty path segment**. `/news/article-123` → `news`.
- It is sanitized: each run of characters outside `[A-Za-z0-9_-]` becomes a
  single `_`.
- The path is used **raw — it is not percent-decoded**. So `/new%20s` →
  `new_20s` (only `%` is disallowed; `2` and `0` are kept), never the decoded
  `new_s`. This keeps `{section}` consistent with how `page_patterns` match the
  same raw path.
- When the path has no segment (`/`, or repeated slashes), `{section}` is
  `section_root`.

`section_root` is **required** whenever any slot's template uses `{section}`,
and must match `[A-Za-z0-9_-]+`. There is no default: the home-section name is
publisher-specific, so the URL→section convention lives in config, not core.
Startup fails if `{section}` is used without a valid `section_root`.

Example resolution for `gam_unit_path = "/{network_id}/example/{section}"` with
`gam_network_id = "123456789"` and `section_root = "home"`:

| Request path    | `gam_unit_path`              |
| --------------- | ---------------------------- |
| `/`             | `/123456789/example/home`    |
| `/news`         | `/123456789/example/news`    |
| `/news/article` | `/123456789/example/news`    |
| `/reviews/x`    | `/123456789/example/reviews` |

An **unmatched route** — a path matched by no slot's `page_patterns` — produces
no slot at all, so no template is rendered for it.

Startup validation rejects a malformed template: an unknown placeholder (e.g.
`{oops}`), an unmatched or nested `{`, a stray `}`, or an empty `gam_unit_path`.

## Fastly Runtime Config Store

After the EdgeZero cutover, the Fastly adapter always dispatches through the
EdgeZero entry point. The former `edgezero_enabled` and `edgezero_rollout_pct`
canary keys are no longer read.

The Fastly service must still provide a `trusted_server_config` config store
because the entry point opens it before dispatch and passes the handle to
EdgeZero-backed platform services. The store may be empty unless another feature
adds keys to it.

**Local development** (`fastly.toml`):

```toml
[local_server.config_stores]
  [local_server.config_stores.trusted_server_config]
    format = "inline-toml"
    [local_server.config_stores.trusted_server_config.contents]
```

**Production setup** (Fastly CLI):

```bash
# Create the store once and attach it to the service.
fastly config-store create --name trusted_server_config
```

Rollback to the legacy entry point is no longer controlled by runtime config
keys. Use the normal deployment rollback path to restore a pre-cleanup service
version if that is required.

## Validation

### Automatic Validation

Configuration is validated at startup:

**Publisher Validation**:

- All fields non-empty
- `origin_url` is valid URL

**EC Validation**:

- `passphrase` ≥ 1 character
- `passphrase` ≠ known placeholders (`"secret-key"`, `"secret_key"`, `"trusted-server"` — case-insensitive)

**Handler Validation**:

- `path` is valid regex
- `username` non-empty
- `password` non-empty

**Integration Validation**:

- Each integration implements `Validate` trait
- Custom rules per integration

### Validation Errors

**Startup Failure** if:

- Required fields missing
- Invalid data types
- Regex compilation fails
- Secret key is default value
- Integration config fails validation

**Error Format**:

```
Configuration error: Integration 'prebid' configuration failed validation:
server_url: must not be empty
```

## Best Practices

### Configuration Management

**Development**:

```toml
# trusted-server.dev.toml
[publisher]
domain = "localhost"
origin_url = "http://localhost:3000"
proxy_secret = "dev-secret"
```

**Staging**:

```bash
# .env.staging
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://staging.publisher.com
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret_staging)
```

**Production**:

```bash
# All secrets from environment
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=$(cat /run/secrets/proxy_secret)
TRUSTED_SERVER__EC__PASSPHRASE=$(cat /run/secrets/ec_secret)
TRUSTED_SERVER__HANDLERS__0__PASSWORD=$(cat /run/secrets/admin_password)
```

### Secret Management

**Do**:
✅ Use environment variables for secrets  
✅ Rotate secrets periodically  
✅ Generate cryptographically random values  
✅ Store in secure secret management (Fastly Secret Store, Vault)  
✅ Use different secrets per environment

**Don't**:
❌ Commit secrets to version control  
❌ Use default/placeholder values  
❌ Share secrets across environments  
❌ Log secret values  
❌ Expose in error messages

### File Organization

**Recommended Structure**:

```
trusted-server.toml          # Base config
trusted-server.dev.toml      # Development overrides
.env.development             # Dev environment vars
.env.staging                 # Staging environment vars
.env.production              # Production environment vars (not in git)
.env.example                 # Example template (in git)
```

**.gitignore**:

```
.env.production
.env.staging
.env.local
*.secret
```

## Troubleshooting

### Common Issues

**"Failed to build configuration"**:

- Check TOML syntax (trailing commas, quotes)
- Verify all required fields present
- Check environment variable format

**"Configuration field '...' is set to a known placeholder value"**:

- `ec.passphrase` cannot be `"secret-key"`, `"secret_key"`, or `"trusted-server"` (case-insensitive)
- `publisher.proxy_secret` cannot be `"change-me-proxy-secret"` (case-insensitive)
- Must be non-empty
- Change to a secure random value (see generation commands above)

**"Invalid regex"**:

- Handler `path` must be valid regex
- Test pattern: `echo "^/_ts/admin" | grep -E "^/_ts/admin"`
- Escape special characters: `\.`, `\$`, etc.

**"Integration configuration could not be parsed"**:

- Check JSON syntax in env vars
- Verify indexed arrays (0, 1, 2...)
- Check field names match exactly

**Environment Variables Not Applied**:

- Env vars are applied at **build time** only — rebuild after changing them
- Verify prefix: `TRUSTED_SERVER__`
- Check separator: `__` (double underscore)
- Confirm variable is exported: `echo $VARIABLE_NAME`
- Try explicit string: `VARIABLE='value'` not `VARIABLE=value`

### Debug Configuration

**Print Loaded Config** (test only):

```rust
use trusted_server_core::settings_data::get_settings;

let settings = get_settings()?;
println!("{:#?}", settings);
```

**Check Environment**:

```bash
# List all TRUSTED_SERVER variables
env | grep TRUSTED_SERVER
```

**Validate TOML**:

```bash
# Use any TOML validator
cat trusted-server.toml | npx toml-cli validate
```

## Next Steps

- Set up [Request Signing](/guide/request-signing) for secure API calls
- Configure [First-Party Proxy](/guide/first-party-proxy) for URL proxying
- Learn about [Edge Cookies](/guide/edge-cookies) for privacy-preserving identification
- Review [Integrations](/guide/integrations-overview) for partner support
