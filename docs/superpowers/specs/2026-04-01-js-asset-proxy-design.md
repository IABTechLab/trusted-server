# JS Asset Proxy — Engineering Spec

**Date:** 2026-04-01  
**Status:** Approved for engineering breakdown  
**Related:** [JS Asset Auditor spec](2026-04-01-js-asset-auditor-design.md), [PRD](../../js-asset-proxy-prd.md)

---

## Context

Publishers using third-party ad tech (Prebid loaders, header bidding wrappers, vendor bundles) have every JS asset served from vendor-controlled infrastructure. They cannot inspect, version-lock, throttle, or disable any of these without contacting the vendor. A misbehaving or slow script on a vendor CDN is completely outside the publisher's operational control.

This feature brings third-party JS delivery inside the publisher's operational boundary. TS fetches vendor JS, caches it in a KV store under opaque first-party paths, and serves it from the publisher's own domain. The publisher sets one CNAME to Fastly. TS handles all fetching, caching, and refreshing transparently.

---

## PRD Open Question Resolutions

| Question | Resolution |
|---|---|
| Pre-warming | Deferred to v2. Cold-miss on first request is acceptable. Document as known gap. |
| Stale TTL | Default 86400s (24h). Optional `stale_ttl_sec` per asset entry to override. |
| Multiple publishers per service | Closed — one publisher per service. Non-issue. |
| Versioned wildcard KV growth | Wildcard entries default `ttl_sec = 1800` (30 min) if not set. Eviction job v2. |
| Vendor CDN URL configurability | Vendors must make CDN URLs configurable (Option 2). Per-vendor launch dependency. String rewriting deferred. |
| Backend declarations (PRD §9.1) | **Removed.** `BackendConfig::from_url()` in `crates/trusted-server-core/src/backend.rs` creates dynamic backends at runtime. No `[backends.*]` toml entries needed. |

---

## Config Contract: `js-assets.toml` (new file)

Separate from `trusted-server.toml` to avoid bloat. Loaded via the same `include_bytes!` / `build.rs` pattern as the main config.

```toml
# js-assets.toml
[kv_stores]
js_asset_store = "golf-js-asset-store"

[[js_assets]]
# https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js
slug = "aB3kR7mN:prebid-load"
path = "/sdk/aB3kR7mN.js"
origin_url = "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"
inject_in_head = true
# ttl_sec = 3600           # optional, defaults to 3600 (fixed) or 1800 (wildcard)
# stale_ttl_sec = 86400    # optional, defaults to 86400

[[js_assets]]
# https://raven-static.vendor.io/prod/1.19.8-hcskhn/raven.js (wildcard detected)
slug = "xQ9pL2wY:raven"
path = "/raven-static/*"
origin_url = "https://raven-static.vendor.io/prod/*/raven.js"
inject_in_head = false
```

### Rust config structs

Add to `crates/trusted-server-core/src/settings.rs`:

```rust
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct KvStores {
    #[serde(default)]
    pub js_asset_store: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct JsAsset {
    pub slug: String,
    pub path: String,
    pub origin_url: String,
    pub ttl_sec: Option<u32>,        // None = use default
    pub stale_ttl_sec: Option<u32>,  // None = 86400
    #[serde(default)]
    pub inject_in_head: bool,
}

impl JsAsset {
    pub fn is_wildcard(&self) -> bool { self.path.contains('*') }

    pub fn resolved_ttl_sec(&self) -> u32 {
        self.ttl_sec.unwrap_or(if self.is_wildcard() { 1800 } else { 3600 })
    }

    pub fn resolved_stale_ttl_sec(&self) -> u32 {
        self.stale_ttl_sec.unwrap_or(86400)
    }
}
```

Extend `Settings`:
```rust
#[serde(default)]
pub kv_stores: KvStores,
#[serde(default)]
pub js_assets: Vec<JsAsset>,
```

**Note on `ttl_sec: Option<u32>`:** Using `Option` (not a default) is intentional — it distinguishes "not set" from "explicitly set to the default value", which is required for the wildcard TTL defaulting logic.

---

## KV Data Model

**Store name:** `js_asset_store`  
**Key format:** `js-asset:{slug}:{path_suffix}`

For wildcard paths, the wildcard segment is included in the key suffix so each version is cached independently (e.g., `js-asset:xQ9pL2wY:raven:prod/1.19.8-hcskhn`).

**Metadata** (≤2048 bytes, fast non-streaming read — no WASM heap cost on cache hit):
```json
{
  "v": 1,
  "origin_url": "https://web.prebidwrapper.com/...",
  "content_type": "application/javascript",
  "etag": "W/\"72a5efa508944ba68ea00764ce5ebe3d\"",
  "fetched_at": 1742910400,
  "expires_at": 1742914000,
  "asset_slug": "aB3kR7mN:prebid-load"
}
```

**Body:** JS content compressed to brotli on the cold path and stored in that form. On every subsequent request the compressed bytes stream directly from KV to the browser — TS never decompresses or re-compresses them. The browser receives the brotli bytes, sees `Content-Encoding: br`, and decompresses natively.

**Slug derivation** (shared algorithm — must be identical between Proxy config and Auditor tool):  
`slug = "{publisher_prefix}:{asset_stem}"` where `publisher_prefix` = first 8 chars of base62(sha256(`publisher.domain` + `origin_url`)).  
Engineering should define this as a shared utility to guarantee consistency.

---

## Cache Read Flow

```
Inbound: GET assets2.publisher.com/sdk/aB3kR7mN.js
        │
        ▼
Read KV metadata only  (~1ms, no stream)
        │
        ├── expires_at > now  → stream stored compressed bytes → browser
        │       Content-Type: application/javascript
        │       Content-Encoding: br   (browser decompresses natively)
        │       Cache-Control: max-age=3600
        │       ETag: {stored_etag}
        │
        └── expires_at ≤ now
                │
                ▼
        Conditional GET: If-None-Match: {stored_etag}
                │
                ├── 304 Not Modified
                │       → update expires_at in metadata only (no body write)
                │       → stream existing KV body → response
                │
                └── 200 OK
                        → write new metadata + new brotli body
                        → stream body → response
```

---

## Degraded Behavior

| Condition | Behavior |
|---|---|
| KV read fails | Fall through to direct origin fetch. Log `warn`. Do not cache (avoid writing to degraded store). |
| KV write fails after successful fetch | Serve fetched content. Log `warn`. Next request retries write. |
| Origin fetch fails | Return `502` with `X-ts-error: asset-origin-unreachable`. If stale KV entry exists, serve it with `X-ts-cache: stale`. |
| Origin returns non-200/304 | Return `502`. Log status at `warn`. |
| Response body > 8MB | Return `502`. Do not write to KV. |

---

## Implementation Phases

### Phase 1: Config schema + routing + KV data model

No user-visible behavior. Establishes the foundation.

**Scope:**
- Provision `js_asset_store` KV store in Fastly and link to the Compute service (see Fastly provisioning below)
- Add local dev KV store entry to `fastly.toml`
- Load `js-assets.toml` via `build.rs` alongside existing `trusted-server.toml` pipeline
- Add `KvStores` and `JsAsset` structs to `settings.rs`; extend `Settings`
- Route matcher: inbound path → `[[js_assets]]` entry lookup before publisher origin fallback in `main.rs`
  - Wildcard segment extraction from path
  - Unmatched paths → `404` with `X-ts-error: asset-not-found`
- Open `js_asset_store` KV store by name at runtime
- Define KV metadata struct and key format (`js-asset:{slug}:{suffix}`)

**Fastly KV store provisioning (one-time, per publisher service):**

```bash
# 1. Create the KV store in Fastly
fastly kv-store create --name "golf-js-asset-store"
# Note the store ID from the output

# 2. Link the store to the Compute service
fastly resource-link create \
  --service-id <service-id> \
  --resource-id <kv-store-id> \
  --version <service-version>

# 3. Activate the new service version
fastly service-version activate --version <service-version>
```

**`fastly.toml` — add local dev entry** (matches existing pattern for `counter_store`, `consent_store` etc.):
```toml
[[local_server.kv_stores.js_asset_store]]
    key = "placeholder"
    data = "placeholder"
```

The `key`/`data` values are literal dummy strings — Viceroy requires at least one seed entry to declare the store at startup. The real `js-asset:` entries are written at runtime by the handler.

For local testing of the cache hit (hot) path without a live vendor origin, engineering should add a pre-populated test entry alongside the placeholder:
```toml
[[local_server.kv_stores.js_asset_store]]
    key = "js-asset:test-slug:test-asset"
    data = "{\"v\":1,\"origin_url\":\"https://example.com/test.js\",\"content_type\":\"application/javascript\",\"etag\":\"\\\"test\\\"\",\"fetched_at\":0,\"expires_at\":9999999999,\"asset_slug\":\"test-slug:test-asset\"}"
```

**Files:**
- `js-assets.toml` — new, checked in with placeholder entries
- `fastly.toml` — add `[[local_server.kv_stores.js_asset_store]]` entry
- `crates/trusted-server-core/src/settings.rs` — add `KvStores`, `JsAsset`, extend `Settings`
- `crates/trusted-server-adapter-fastly/src/main.rs` — add asset path routing before publisher fallback
- `crates/trusted-server-adapter-fastly/build.rs` — include `js-assets.toml`
- `crates/trusted-server-core/src/js_assets/mod.rs` (new module) — route matcher, KV key format, metadata types

### Phase 2: Fetch + cache pipeline

Core value. Requires populated `js-assets.toml` from the Auditor (see delivery order).

**Scope:**
- `handle_js_asset()` handler wired into route dispatch
- Cold path: `BackendConfig::from_url()` → origin fetch → compress body to brotli → write compressed bytes + metadata to KV
- Hot path: KV metadata read → `expires_at > now` check → stream stored compressed bytes directly to browser (no decompression or re-compression)
- Conditional refresh: `If-None-Match` → handle 304 (metadata-only update) and 200 (full write)
- All degraded behavior from the table above

**Response headers on serve:**
```
Content-Type: application/javascript
Content-Encoding: br
Cache-Control: public, max-age=3600
ETag: {stored_etag}
```

**Caching layers — all aligned at 1h:**
- **KV store TTL (1h):** how long TS keeps its cached copy before issuing a conditional GET to the vendor origin
- **Fastly CDN edge cache (s-maxage=3600 via `public, max-age=3600`):** Fastly serves the asset from edge POP without hitting the TS WASM at all
- **Browser cache (max-age=3600):** browser does not request the asset again until the hour expires

All three are intentionally aligned. Do not differentiate them without revisiting the full caching strategy.

**Files:**
- `crates/trusted-server-core/src/js_assets/` — `handle_js_asset()`, fetch logic, KV read/write, conditional refresh
- `crates/trusted-server-adapter-fastly/src/main.rs` — wire handler into dispatch

**Reuse:**
- `BackendConfig::from_url()` in `crates/trusted-server-core/src/backend.rs` — dynamic backend creation, no toml changes needed
- Brotli compression patterns from existing streaming pipeline in `crates/trusted-server-core/src/streaming_processor.rs`

### Phase 3: HTML injection + security hardening

Completes full proxy mode.

**Scope:**
- For each `[[js_assets]]` entry with `inject_in_head = true`: inject `<script src="...">` at start of `<head>` using existing `html_processor.rs` pipeline. Injection point: before TS core bundle, consistent with existing `head_inserts()` pattern in `integrations/registry.rs`.
- Path traversal validation: wildcard segments must match `[A-Za-z0-9._\-/]` only. Segments containing `..`, `%2e`, or traversal patterns → `400`.
- Origin allowlist enforcement: fetch only for URLs declared in `[[js_assets]]`. Wildcard `*` forwarded verbatim to declared origin only.

**Files:**
- `crates/trusted-server-core/src/html_processor.rs` — js asset head injection
- `crates/trusted-server-core/src/js_assets/` — path traversal validation
- `crates/trusted-server-core/src/integrations/registry.rs` — integrate asset injectors alongside existing `head_inserts()`

---

## Known Gaps (v2)

- Pre-warming: populate KV at deploy time (eliminates cold-miss for first user after deploy)
- KV eviction job for orphaned wildcard entries
- String rewriting within served JS content (vendor CDN URL rewriting)
- Tag Auditor for ongoing JS firewall visibility (separate future spec)

---

## Verification

- `cargo test --workspace` — unit + integration tests covering:
  - Route matching for fixed and wildcard paths
  - Wildcard segment extraction and forwarding
  - Path traversal rejection (`..`, `%2e`)
  - KV hit path (metadata read → stream body)
  - KV miss path (origin fetch → KV write → stream body)
  - Conditional refresh: 304 (metadata-only update) and 200 (full write)
  - Stale-on-error serve with `X-ts-cache: stale`
  - Degraded behavior fallbacks (KV read fail, KV write fail, origin fail, oversized body)
- Manual: `fastly compute serve` with a test `[[js_assets]]` entry → verify cache hit/miss response headers and KV population
- Manual: verify `inject_in_head = true` entry appears as `<script>` before TS bundle in `<head>`
