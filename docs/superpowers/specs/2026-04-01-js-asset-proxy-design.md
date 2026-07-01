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
- Allow configured assets to be individually proxied, disabled, or blocked from publisher HTML.
- Reuse the existing integration registry and proxy request infrastructure.

---

## Configuration

Add a new integration configuration block:

```toml
[integrations.js_asset_proxy]
enabled = false

[[integrations.js_asset_proxy.assets]]
path = "/assets/vendor-loader.js"
origin_url = "https://js.vendor.example.com/loader.js"
proxy = "enabled"

[[integrations.js_asset_proxy.assets]]
path = "/assets/measurement-sdk.js"
origin_url = "https://cdn.vendor.example.com/sdk/measurement.js"
proxy = "enabled"
cache_ttl_seconds = 900

[[integrations.js_asset_proxy.assets]]
path = "/assets/blocked-sdk.js"
origin_url = "https://cdn.vendor.example.com/sdk/blocked.js"
proxy = "blocked"

[[integrations.js_asset_proxy.assets]]
path = "/assets/inactive-sdk.js"
origin_url = "https://cdn.vendor.example.com/sdk/inactive.js"
proxy = "disabled"
```

### Fields

| Field                        | Required | Description                                                                                                            |
| ---------------------------- | -------: | ---------------------------------------------------------------------------------------------------------------------- |
| `enabled`                    |      Yes | Enables or disables the integration.                                                                                   |
| `cache_ttl_seconds`          |       No | Optional downstream cache TTL override for all assets. When unset, preserve the upstream cache policy.                 |
| `assets`                     |      Yes | List of JavaScript assets the proxy may serve.                                                                         |
| `assets[].path`              |      Yes | Stable identifier for logs, tests, and response diagnostics; exact first-party request path handled by Trusted Server. |
| `assets[].origin_url`        |      Yes | Exact upstream JavaScript URL to fetch or match for page rewriting.                                                    |
| `assets[].proxy`             |       No | Per-asset proxy behavior: `enabled`, `disabled`, or `blocked`. Defaults to `enabled`.                                  |
| `assets[].cache_ttl_seconds` |       No | Per-asset downstream cache TTL override. Takes precedence over the integration-level value.                            |

### Validation

Configuration validation must reject:

- enabled integration with malformed configured assets;
- empty `assets` when the integration is enabled;
- duplicate asset paths;
- duplicate `origin_url` values;
- asset paths that do not start with `/`;
- asset paths containing `*`;
- asset paths containing `..` path segments;
- `proxy` values other than `enabled`, `disabled`, or `blocked`;

The implementation may use stricter validation if it keeps the configuration contract simple and documented.

---

## Asset Proxy Behavior

Each asset has a `proxy` setting that controls both page rewriting and route registration:

| Value      | Behavior                                                                                                                                                           |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `enabled`  | Rewrite matching `<script src>` references to the configured first-party `path`, register one exact `GET` route for that path, and proxy requests to `origin_url`. |
| `disabled` | Keep the asset in configuration but perform no page rewriting, no blocking, and no route registration.                                                             |
| `blocked`  | Remove matching `<script src>` elements from HTML and do not register a proxy route.                                                                               |

Page rewriting and blocking match the configured `origin_url` after supported URL normalization, including protocol-relative URLs on matching schemes, host/scheme case normalization, and default-port removal. They do not match by host, path prefix, wildcard, or partial URL.

---

## Routing

For assets with `proxy = "enabled"`, the integration registers one exact `GET` route per configured asset path using `IntegrationProxy::routes()`.

Example registration from the configuration above:

| Method | Path                         | Upstream URL                                        |
| ------ | ---------------------------- | --------------------------------------------------- |
| `GET`  | `/assets/vendor-loader.js`   | `https://js.vendor.example.com/loader.js`           |
| `GET`  | `/assets/measurement-sdk.js` | `https://cdn.vendor.example.com/sdk/measurement.js` |

Only exact configured paths for enabled assets are handled. Paths for disabled or blocked assets are not registered and continue through the existing request dispatch behavior.

The integration should rely on the existing integration registry duplicate-route checks so that an asset path cannot silently shadow another integration endpoint.

---

## Request Flow

For a matching request:

1. Identify the enabled configured asset by exact request path.
2. Build an upstream `GET` request to the asset's configured `origin_url`.
3. Use the existing proxy request infrastructure with streaming passthrough and platform response streaming enabled.
4. Do not append EC IDs or any other per-user identifiers to the upstream URL.
5. Do not perform server-side JavaScript rewriting.
6. Finalize the response with the header policy below.

### Proxy request configuration

The JS asset proxy should use the same shape as existing script proxy integrations:

```rust
let mut config = ProxyRequestConfig::new(origin_url)
    .with_streaming()
    .with_stream_response()
    .without_forward_headers();
config.follow_redirects = false;
config.forward_ec_id = false;
```

The integration may forward this small request header allowlist from the browser request to the upstream request:

- `Accept`
- `Accept-Language`
- `Accept-Encoding`

Do not forward user session or network context headers; examples: `Cookie`, `X-Forwarded-For`.

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
- `Cache-Control` when neither integration-level nor per-asset `cache_ttl_seconds` is set

Set this response header:

```http
X-TS-JS-Asset-Proxy: true
```

For `Cache-Control`, resolve the downstream cache policy as follows:

1. If `assets[].cache_ttl_seconds` is set, override with `Cache-Control: public, max-age=<asset cache_ttl_seconds>`.
2. Else, if integration-level `cache_ttl_seconds` is set, override with `Cache-Control: public, max-age=<integration cache_ttl_seconds>`.
3. Else, preserve the upstream `Cache-Control` header when present and do not synthesize one when absent.

If the response includes `Content-Encoding`, ensure `Vary` includes `Accept-Encoding` unless the upstream response uses `Vary: *`.

Do not forward upstream `Set-Cookie` headers.

### Upstream fetch failure

If the upstream request cannot be completed, return:

```http
502 Bad Gateway
X-TS-Error: js-asset-origin-unreachable
```

Log the request path and origin host at `warn` level.

### Upstream non-success response

If the upstream responds with a non-`2xx` status, return:

```http
502 Bad Gateway
X-TS-Error: js-asset-origin-status
```

Log the upstream status, request path, and origin host at `warn` level.

---

## Implementation Plan

### 1. Add integration configuration types

Add a new `js_asset_proxy` integration module with typed config:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct JsAssetProxyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cache_ttl_seconds: Option<u32>,
    #[serde(default)]
    pub assets: Vec<JsAssetProxyAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct JsAssetProxyAsset {
    pub path: String,
    #[validate(url)]
    pub origin_url: String,
    #[serde(default)]
    pub proxy: JsAssetProxyMode,
    pub cache_ttl_seconds: Option<u32>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JsAssetProxyMode {
    Enabled,
    Disabled,
    Blocked,
}

impl Default for JsAssetProxyMode {
    fn default() -> Self {
        Self::Enabled
    }
}
```

Implement `IntegrationConfig` for `JsAssetProxyConfig` and add custom validation for the rules in this spec.

### 2. Register integration routes

Add `crates/trusted-server-core/src/integrations/js_asset_proxy.rs` and register it from the integration builders list with both proxy and attribute-rewriter capabilities.

`routes()` returns one exact `GET` endpoint per enabled configured asset path.

### 3. Implement page rewriting

Implement attribute rewriting for `<script src="...">` values:

1. Match only exact configured `origin_url` values.
2. For `proxy = "enabled"`, replace the `src` value with the asset's configured first-party `path`.
3. For `proxy = "blocked"`, remove the entire script element.
4. For `proxy = "disabled"`, leave the script element unchanged.

### 4. Implement request handling

In `handle()`:

1. Match `req.get_path()` to an enabled configured asset.
2. Build the streaming proxy config.
3. Fetch the upstream response through `proxy_request()`.
4. Reject non-success upstream status codes.
5. Return a finalized response with the response header policy in this spec.

### 5. Add sample disabled configuration

Add a disabled sample block to `trusted-server.toml` using only `example.com` domains.

---

## Files

Expected code changes:

- `crates/trusted-server-core/src/integrations/js_asset_proxy.rs`
- `crates/trusted-server-core/src/integrations/mod.rs`
- `trusted-server.toml`

No adapter entry-point changes are expected if the existing integration registry dispatch is sufficient.

---

## Future Trusted Server CLI Support

When the Trusted Server CLI is ready, it should provide tooling for generating and managing `integrations.js_asset_proxy.assets` entries so operators do not need to edit these blocks manually.

The CLI should be able to read the generated `js-assets.toml` file and create matching `[[integrations.js_asset_proxy.assets]]` configuration entries. Generated entries should use randomized first-party JavaScript paths by default, including the `/assets/` subdirectory and a `.js` suffix, for example:

```toml
path = "/assets/8f4c2a91d7b3.js"
origin_url = "https://cdn.vendor.example.com/sdk.js"
proxy = "enabled"
```

The randomized path should avoid embedding vendor names or other semantic identifiers in the public URL.

The CLI should eventually support asset-management operations such as listing assets, adding assets, deleting assets, enabling proxying, disabling proxying, and blocking assets. Command names are intentionally not fixed by this spec, but they are expected to live under the `ts` CLI rather than requiring direct TOML edits.

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
- `proxy = "enabled"` registers a route and rewrites matching script `src` URLs to the configured first-party path;
- `proxy = "disabled"` does not register a route and leaves matching script `src` URLs unchanged;
- `proxy = "blocked"` does not register a route and removes matching script elements;
- non-exact `origin_url` matches are not rewritten or blocked;
- duplicate asset paths are rejected;
- duplicate `origin_url` values are rejected;
- invalid paths are rejected;
- non-HTTPS origins are rejected;
- exact configured routes are registered;
- request path selects the correct asset;
- upstream `2xx` response streams body and sets expected headers;
- upstream `Cache-Control` is preserved when no cache TTL override is configured;
- configured cache TTL overrides upstream `Cache-Control`;
- upstream fetch failure returns `502` with `X-TS-Error: js-asset-origin-unreachable`;
- upstream non-success response returns `502` with `X-TS-Error: js-asset-origin-status`;
- `Set-Cookie`, `Referer`, `X-Forwarded-For`, and EC values are not forwarded.
