# JS Asset Proxy — Engineering Spec

**Date:** 2026-04-01  
**Updated:** 2026-05-28  
**Status:** Proposed

---

## Context

Publishers often need to load JavaScript from third-party ad tech or measurement vendors. Those scripts are usually referenced directly from vendor-controlled domains, which means the publisher page depends on external script hostnames at runtime.

The JS Asset Proxy gives Trusted Server a small, explicit way to serve configured third-party JavaScript files from first-party paths. Each proxied asset is declared in `trusted-server.toml`; at request time Trusted Server fetches the configured upstream URL and streams the response back to the browser with controlled response headers.

This spec intentionally follows existing integration proxy patterns already used in Trusted Server. The implementation should be a focused integration-level proxy, not a new storage, build, or asset management subsystem.

---

## Goals

- Serve allowlisted third-party JavaScript assets from configured first-party paths.
- Keep configuration in `trusted-server.toml` under the existing `[integrations.*]` configuration model.
- Fetch only explicitly configured upstream URLs.
- Stream upstream JavaScript responses without server-side body transformation.
- Apply predictable downstream cache headers controlled by Trusted Server configuration.
- Reuse the existing integration registry and proxy request infrastructure.

---

## Configuration

Add a new integration configuration block:

```toml
[integrations.js_asset_proxy]
enabled = false
cache_ttl_seconds = 3600

[[integrations.js_asset_proxy.assets]]
id = "vendor-loader"
path = "/assets/vendor-loader.js"
origin_url = "https://js.vendor.example.com/loader.js"

[[integrations.js_asset_proxy.assets]]
id = "measurement-sdk"
path = "/assets/measurement-sdk.js"
origin_url = "https://cdn.vendor.example.com/sdk/measurement.js"
cache_ttl_seconds = 900
```

### Fields

| Field                        | Required | Description                                                      |
| ---------------------------- | -------: | ---------------------------------------------------------------- |
| `enabled`                    |      Yes | Enables or disables the integration.                             |
| `cache_ttl_seconds`          |       No | Default downstream cache TTL for all assets. Defaults to `3600`. |
| `assets`                     |      Yes | List of JavaScript assets the proxy may serve.                   |
| `assets[].id`                |      Yes | Stable identifier for logs, tests, and response diagnostics.     |
| `assets[].path`              |      Yes | Exact first-party request path handled by Trusted Server.        |
| `assets[].origin_url`        |      Yes | Exact upstream JavaScript URL to fetch.                          |
| `assets[].cache_ttl_seconds` |       No | Per-asset downstream cache TTL override.                         |

### Validation

Configuration validation must reject:

- enabled integration with malformed configured assets;
- empty `assets` when the integration is enabled;
- duplicate asset IDs;
- duplicate asset paths;
- asset paths that do not start with `/`;
- asset paths containing `*`;
- asset paths containing `..` path segments;
- `origin_url` values without an `https://` scheme;
- `origin_url` values with fragments;
- `cache_ttl_seconds = 0`.

The implementation may use stricter validation if it keeps the configuration contract simple and documented.

---

## Routing

The integration registers one exact `GET` route per configured asset path using `IntegrationProxy::routes()`.

Example registration from the configuration above:

| Method | Path                         | Asset ID          | Upstream URL                                        |
| ------ | ---------------------------- | ----------------- | --------------------------------------------------- |
| `GET`  | `/assets/vendor-loader.js`   | `vendor-loader`   | `https://js.vendor.example.com/loader.js`           |
| `GET`  | `/assets/measurement-sdk.js` | `measurement-sdk` | `https://cdn.vendor.example.com/sdk/measurement.js` |

Only exact configured paths are handled. Paths not registered by the integration continue through the existing request dispatch behavior.

The integration should rely on the existing integration registry duplicate-route checks so that an asset path cannot silently shadow another integration endpoint.

---

## Request Flow

For a matching request:

1. Identify the configured asset by exact request path.
2. Build an upstream `GET` request to the asset's configured `origin_url`.
3. Use the existing proxy request infrastructure with streaming passthrough enabled.
4. Do not append EC IDs or any other per-user identifiers to the upstream URL.
5. Do not perform server-side JavaScript rewriting.
6. Finalize the response with the header policy below.

### Proxy request configuration

The JS asset proxy should use the same shape as existing script proxy integrations:

```rust
let mut config = ProxyRequestConfig::new(origin_url)
    .with_streaming()
    .without_forward_headers();
config.follow_redirects = false;
config.forward_ec_id = false;
```

The integration may forward this small request header allowlist from the browser request to the upstream request:

- `Accept`
- `Accept-Language`
- `Accept-Encoding`

It must set a fixed `User-Agent` such as `TrustedServer/1.0`.

TLS verification follows the existing Trusted Server proxy backend policy used by `proxy_request()`.

---

## Response Behavior

### Successful upstream response

For upstream `2xx` responses, Trusted Server streams the upstream body to the browser and constructs a response with only the headers needed for JavaScript delivery and diagnostics.

Preserve these upstream response headers when present:

- `Content-Type`
- `Content-Encoding`
- `ETag`
- `Last-Modified`
- `Vary`

Set or override these response headers:

```http
Cache-Control: public, max-age=<resolved cache_ttl_seconds>
X-TS-JS-Asset-Proxy: true
X-TS-JS-Asset-ID: <asset id>
```

If the response includes `Content-Encoding`, ensure `Vary` includes `Accept-Encoding` unless the upstream response uses `Vary: *`.

Do not forward upstream `Set-Cookie` headers.

### Upstream fetch failure

If the upstream request cannot be completed, return:

```http
502 Bad Gateway
X-TS-Error: js-asset-origin-unreachable
X-TS-JS-Asset-ID: <asset id>
```

Log the asset ID and origin host at `warn` level.

### Upstream non-success response

If the upstream responds with a non-`2xx` status, return:

```http
502 Bad Gateway
X-TS-Error: js-asset-origin-status
X-TS-JS-Asset-ID: <asset id>
```

Log the upstream status, asset ID, and origin host at `warn` level.

---

## Security Requirements

- Fetch only the exact `origin_url` values declared in configuration.
- Do not accept user-provided upstream URLs at request time.
- Do not construct upstream hosts from request path segments.
- Do not forward cookies to the upstream JavaScript host.
- Do not forward upstream `Set-Cookie` headers to the browser.
- Do not forward `Referer` or `X-Forwarded-For` to the upstream JavaScript host.
- Do not append EC IDs or other Trusted Server identity values to asset requests.
- Require `https://` upstream URLs.

---

## Implementation Plan

### 1. Add integration configuration types

Add a new `js_asset_proxy` integration module with typed config:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct JsAssetProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl_seconds")]
    pub cache_ttl_seconds: u32,
    #[serde(default)]
    pub assets: Vec<JsAssetProxyAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct JsAssetProxyAsset {
    pub id: String,
    pub path: String,
    #[validate(url)]
    pub origin_url: String,
    pub cache_ttl_seconds: Option<u32>,
}
```

Implement `IntegrationConfig` for `JsAssetProxyConfig` and add custom validation for the rules in this spec.

### 2. Register integration routes

Add `crates/trusted-server-core/src/integrations/js_asset_proxy.rs` and register it from the integration builders list.

`routes()` returns one exact `GET` endpoint per configured asset path.

### 3. Implement request handling

In `handle()`:

1. Match `req.get_path()` to a configured asset.
2. Build the streaming proxy config.
3. Fetch the upstream response through `proxy_request()`.
4. Reject non-success upstream status codes.
5. Return a finalized response with the response header policy in this spec.

### 4. Add sample disabled configuration

Add a disabled sample block to `trusted-server.toml` using only `example.com` domains.

---

## Files

Expected code changes:

- `crates/trusted-server-core/src/integrations/js_asset_proxy.rs`
- `crates/trusted-server-core/src/integrations/mod.rs`
- `trusted-server.toml`

No adapter entry-point changes are expected if the existing integration registry dispatch is sufficient.

---

## Verification

Run the standard Rust verification for the changed integration code:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Add unit tests covering:

- disabled config does not register routes;
- enabled config requires at least one asset;
- duplicate asset IDs are rejected;
- duplicate asset paths are rejected;
- invalid paths are rejected;
- non-HTTPS origins are rejected;
- exact configured routes are registered;
- request path selects the correct asset;
- upstream `2xx` response streams body and sets expected headers;
- upstream fetch failure returns `502` with `X-TS-Error: js-asset-origin-unreachable`;
- upstream non-success response returns `502` with `X-TS-Error: js-asset-origin-status`;
- `Set-Cookie`, `Referer`, `X-Forwarded-For`, and EC values are not forwarded.
