# Product Requirements: First-Party JS Asset Proxy

**Status:** Draft
**Author:** Trusted Server Product
**Last updated:** 2026-03-26

---

## Table of Contents

1. [Overview](#1-overview)
2. [Problem Statement](#2-problem-statement)
3. [Goals and Non-Goals](#3-goals-and-non-goals)
4. [Target Customers](#4-target-customers)
5. [Opaque URL Routing](#5-opaque-url-routing)
6. [KV Asset Cache](#6-kv-asset-cache)
7. [HTML Script Tag Injection](#7-html-script-tag-injection)
8. [Cache Lifecycle](#8-cache-lifecycle)
9. [Configuration](#9-configuration)
10. [Routes](#10-routes)
11. [Security](#11-security)
12. [Open Questions](#12-open-questions)
13. [Success Metrics](#13-success-metrics)

---

## 1. Overview

The JS Asset Proxy allows Trusted Server to fetch third-party JavaScript files from configured origin URLs, cache them in Fastly or similar KV Store under opaque first-party paths, and serve them from the publisher's own domain. When running in full HTML proxy mode, TS also injects the corresponding `<script>` tag into the page `<head>` automatically.

The primary use case is publisher-controlled delivery of ad tech scripts (header bidding wrappers, Prebid loaders, measurement SDKs) that today load from vendor-controlled infrastructure outside the publisher's trust boundary. By serving these assets from `assets2.publisher.com`, TS brings third-party script delivery inside the publisher's operational control without requiring any ongoing changes from the publisher.

TS Lite acts as a publisher-controlled JS execution gateway. Before any third-party script reaches a user's browser, it passes through the publisher's own edge service — where TS can enforce consent, inspect cache state, apply rate limits, or disable delivery entirely. This is the same trust model publishers use for their own first-party assets, extended to vendor dependencies.

---

## 2. Problem Statement

Publishers using third-party ad tech wrappers have every component of the ad stack served from vendor-controlled domains outside the publisher's infrastructure:

| Asset | Third-party origin | Publisher control |
|---|---|---|
| Prebid bootstrapper | `web.prebidwrapper.com` | None — vendor infrastructure |
| Vendor loader | `raven-edge.vendor.io` | None — vendor infrastructure |
| Vendor bundle | `raven-static.vendor.io` | None — vendor infrastructure |
| PBS auction XHR | `vendor-pbs.com` | None — vendor infrastructure |
| PBS cookie sync | `vendor-pbs.com/cookie_sync` | None — vendor infrastructure |
| Google Tag Manager | `googletagmanager.com` | None — vendor infrastructure |

Publishers have no ability to inspect, version-lock, throttle, or disable any of these assets without contacting the vendor. A misbehaving or slow script on `raven-static.vendor.io` is outside the publisher's operational control entirely. DNS resolution and TLS handshakes to 4+ distinct origins also add measurable latency on every uncached pageview.

### 2.1 The fix is publisher-owned infrastructure

Serving JS from the publisher's own domain (`assets2.publisher.com`) puts the asset delivery path inside the publisher's operational boundary. The publisher can audit what runs, when it was last fetched, and disable any asset centrally without a vendor call. The publisher sets a single CNAME to Fastly or similar and hardcodes one `<script>` tag in their HTML template. TS handles all fetching, caching, and refreshing transparently.

### 2.2 Stable opaque paths provide namespace isolation

Even on a first-party domain, a predictable path like `/prebid-load.js` is fragile — it couples the publisher's asset namespace to vendor naming conventions and breaks if the vendor renames their file. Serving under an opaque path (`/sdk/gWnLmpLy.js`, derived from a publisher-specific token) keeps the publisher's asset namespace decoupled from vendor naming conventions and protects against vendor URL changes breaking the publisher's page. The path never changes after initial setup regardless of what the vendor does upstream.

---

## 3. Goals and Non-Goals

### Goals

- Fetch third-party JS from configured origin URLs and cache in `js_asset_store` KV
- Serve cached JS under opaque, first-party paths at `assets2.publisher.com`
- Give publishers operational control over which third-party scripts execute on their pages — inspectable, version-lockable, and disableable without vendor involvement
- Initial TTL of **1 hour**, matching the most common origin `Cache-Control: max-age=3600`
- Use ETag-based conditional refresh: only re-fetch body on `200`; on `304` extend TTL only
- Store content brotli-compressed in KV; serve with matching `Content-Encoding` header
- In full HTML proxy mode: inject the `<script>` tag into `<head>` automatically using the existing `html_processor.rs` head injection pipeline
- In TS Lite mode: publisher hardcodes the opaque `<script>` tag once; TS serves it
- Support versioned wildcard paths for assets with dynamic version segments (e.g., `/raven-static/*/raven.js`)

### Non-Goals

- URL rewriting within served JS content (origin providers are expected to make CDN URLs configurable rather than hardcoded)
- Serving non-JS assets (images, CSS, fonts) through this pipeline — ad tech JS only
- Real-time cache invalidation webhooks (TTL-based expiry is sufficient for v1)
- Per-request personalization of JS content (JS is the same for all users of a given publisher)
- Rotating opaque filenames over time — paths are stable per publisher

---

## 4. Target Customers

| Customer | Deployment mode | How they use this feature |
|---|---|---|
| Publisher (TS Lite) | `assets2.publisher.com` → Fastly or similar | Hardcodes one opaque `<script>` tag; all vendor JS delivered through publisher-controlled infrastructure |
| Publisher (full TS) | Full HTML proxy | TS injects `<script>` tag automatically; no HTML change needed |
| Ad tech partner | Ships TS Lite config to their pub customers | Configures asset mappings; publishers flip a DNS record |

---

## 5. Opaque URL Routing

### 5.1 Path generation

The opaque path token is derived deterministically from the publisher token using the first 8 characters of a base62 encoding of the publisher ID. The token is stable — it does not rotate and the publisher sets it once.

**Example mapping for publisher `golf-WnLmpLyEjL`:**

| Opaque path | Asset type | Origin URL |
|---|---|---|
| `/sdk/gWnLmpLy.js` | Prebid bootstrapper | `https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js` |
| `/raven/golfmain.js` | Vendor loader | `https://raven-edge.vendor.io/raven/golf-main-L1Zrx/library.js` |
| `/raven-static/*/raven.js` | Vendor bundle (versioned) | `https://raven-static.vendor.io/prod/*/raven.js` |

### 5.2 Route matching

TS matches inbound paths against the asset registry in `trusted-server.toml`. Wildcard segments (`*`) in the path are forwarded verbatim to the origin URL. Paths not matching any registered asset return `404`.

### 5.3 Why opaque and not content-hash

Content-hash URLs (e.g., `tsjs-core.abc123.js`) are appropriate for assets TS generates itself, where the hash is computed at build time. Third-party JS has content that changes independently — using a publisher-token-derived slug decouples the URL from the content version. The ETag mechanism handles freshness.

---

## 6. KV Asset Cache

### 6.1 KV store

**Store name:** `js_asset_store` (configured in `trusted-server.toml` alongside `ec_store` and `partner_store`)

### 6.2 Key format

```
js-asset:{asset_slug}:{path_suffix}
```

| Key | Origin |
|---|---|
| `js-asset:golf-WnLmpLyEjL:prebid-load` | `https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js` |
| `js-asset:golf-main-L1Zrx:vendor-library` | `https://raven-edge.vendor.io/raven/golf-main-L1Zrx/library.js` |
| `js-asset:vendor-static:prod/1.19.8-hcskhn` | `https://raven-static.vendor.io/prod/1.19.8-hcskhn/raven.js` |

For versioned wildcard paths, the wildcard segment is included in the key suffix so each version is cached independently.

### 6.3 KV entry structure

Fastly or similar KV has two layers: metadata (≤2048 bytes, fast non-streaming read) and body (≤8MB, streamed). This feature uses both.

**Metadata** (read first on every request, no streaming cost):

```json
{
  "v": 1,
  "origin_url": "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js",
  "content_type": "application/javascript",
  "etag": "W/\"72a5efa508944ba68ea00764ce5ebe3d\"",
  "fetched_at": 1742910400,
  "expires_at": 1742914000,
  "asset_slug": "golf-WnLmpLyEjL:prebid-load"
}
```

**Body:** Brotli-compressed JS content, matching the origin's `Content-Encoding`. Stored compressed to minimize KV read/write cost and to serve directly without re-compression.

**KV TTL:** `3600` seconds on initial write. Extended (without body re-write) on `304 Not Modified` from origin.

### 6.4 Cache read flow

```
Inbound request: GET assets2.golf.com/sdk/gWnLmpLy.js
        │
        ▼
Read KV metadata only  (~1ms, no stream, no WASM heap cost)
        │
        ├── expires_at > now  (cache hit)
        │       └── stream KV body → response
        │           Content-Type: application/javascript
        │           Content-Encoding: br
        │           Cache-Control: max-age=3600
        │           ETag: {stored etag}
        │
        └── expires_at ≤ now  (cache miss or expired)
                │
                ▼
        Conditional GET to origin
        If-None-Match: {stored etag}
                │
                ├── 304 Not Modified
                │       → write updated metadata (new expires_at only, no body write)
                │       → stream existing KV body → response
                │
                └── 200 OK
                        → write new metadata
                        → write new body (brotli-compressed)
                        → stream body → response
```

Reading metadata before deciding whether to stream the body is the key optimization: on a cache hit, JS content is never loaded into WASM memory. It streams directly from KV to the response buffer.

### 6.5 KV degraded behavior

| Condition | Behavior |
|---|---|
| KV read fails | Fall back to direct origin fetch. Log at `warn`. Serve origin response. Do not cache (avoid writing to a degraded store). |
| KV write fails after successful origin fetch | Serve the fetched content. Log at `warn`. Next request will retry the write. |
| Origin fetch fails | Return `502` with `X-ts-error: asset-origin-unreachable`. Do not clear cached entry — stale content is preferable to a broken page. |
| Origin returns non-200/304 | Return `502`. Log response status at `warn`. |

Stale-on-error: if the KV entry is expired but the origin returns a non-200/304, the stale cached body is served with an additional response header `X-ts-cache: stale` rather than breaking the page.

---

## 7. HTML Script Tag Injection

### 7.1 Full TS proxy mode

When TS is operating as a full HTML proxy (HTML proxy enabled in `trusted-server.toml`), it injects the asset's `<script>` tag automatically using the existing `html_processor.rs` head injection pipeline.

The injection point is the start of `<head>`, before the TS core bundle, consistent with how integration head injectors work today (`head_inserts()` in `integration_registry.rs`). This ensures the vendor JS is available as early as possible in page parse order.

**Injected tag (example):**
```html
<head>
  <script src="https://assets2.golf.com/sdk/gWnLmpLy.js"></script>
  <!-- existing TS bundle injection follows -->
  <script src="/static/tsjs=tsjs-unified.min.js"></script>
```

### 7.2 TS Lite mode (HTML proxy disabled)

TS does not modify HTML responses. The publisher hardcodes the opaque `<script>` tag in their HTML template once at onboarding. TS only responds to requests for the opaque asset URLs.

```html
<!-- Publisher adds once to HTML <head> template, never changes -->
<script src="https://assets2.golf.com/sdk/gWnLmpLy.js"></script>
```

### 7.3 Tag attributes

The injected or hardcoded tag has no `async` or `defer` attributes by default. Ad tech bootstrapper scripts (Prebid loaders, vendor loaders) are render-blocking by design — they must execute before GPT slot definitions and auction calls. The ad tech partner controls whether their subsequently loaded scripts are async/defer.

---

## 8. Cache Lifecycle

### 8.1 Initial TTL

**1 hour (3600 seconds)** for v1. This matches the most common `Cache-Control: max-age=3600` from ad tech CDN origins. The TTL is configurable per asset entry in `trusted-server.toml` to accommodate partners with different cache policies.

### 8.2 ETag-based conditional refresh

On TTL expiry, TS issues a conditional `GET` with `If-None-Match: {stored_etag}` to the origin. A `304 Not Modified` response costs only a TCP roundtrip — no body is transferred. The KV body is reused as-is, and only the metadata `expires_at` is updated. This is the expected common case: vendor files are typically updated daily, not hourly.

### 8.3 Cache warming

On first request after deployment (cold KV), the origin fetch adds latency to that one request. This is acceptable for v1. A pre-warm step (fetching and populating KV at service deploy time) is a follow-on improvement.

### 8.4 Manual invalidation

TS operators can force a cache bust by deleting the KV entry via the KV management API. The next request will treat it as a cold miss and fetch from origin. No binary redeploy required.

---

## 9. Configuration

New section in `trusted-server.toml`:

```toml
[kv_stores]
js_asset_store = "golf-js-asset-store"   # new, alongside ec_store and partner_store

# One [[js_assets]] entry per proxied asset
[[js_assets]]
slug = "golf-WnLmpLyEjL:prebid-load"
path = "/sdk/gWnLmpLy.js"
origin_url = "https://web.prebidwrapper.com/golf-WnLmpLyEjL/default-v2/prebid-load.js"
ttl_sec = 3600
inject_in_head = true          # inject <script> tag in full HTML proxy mode

[[js_assets]]
slug = "golf-main-L1Zrx:vendor-library"
path = "/raven/golfmain.js"
origin_url = "https://raven-edge.vendor.io/raven/golf-main-L1Zrx/library.js"
ttl_sec = 3600
inject_in_head = false         # loaded by prebid-load.js, not injected directly

[[js_assets]]
slug = "vendor-static"
path = "/raven-static/*"       # wildcard — version segment forwarded to origin
origin_url = "https://raven-static.vendor.io/prod/*"
ttl_sec = 3600
inject_in_head = false
```

### 9.1 Backend configuration

Each distinct origin hostname requires a declared backend in `trusted-server.toml` (Fastly or similar Compute requires backends to be declared at deploy time):

```toml
[backends.vendor_prebid]
url = "https://web.prebidwrapper.com"

[backends.vendor_raven_edge]
url = "https://raven-edge.vendor.io"

[backends.vendor_raven_static]
url = "https://raven-static.vendor.io"
```

---

## 10. Routes

| Method | Path | Handler | Notes |
|---|---|---|---|
| `GET` | `/{asset_path}` matching a configured `[[js_assets]]` entry | `handle_js_asset` | KV cache read → conditional origin fetch |
| Any | `/{asset_path}` not matching any entry | `404` with `X-ts-error: asset-not-found` | |

The asset path matching runs before publisher origin proxying in the request dispatch order. If a path matches a configured `[[js_assets]]` entry, it is handled entirely by `handle_js_asset` and never forwarded to the publisher's origin.

---

## 11. Security

### 11.1 Origin allowlist

Only URLs declared in `[[js_assets]]` are ever fetched. TS does not accept arbitrary origin URLs at request time. The wildcard `*` in path patterns is forwarded verbatim to the declared origin only — it cannot be used to construct requests to other hosts.

### 11.2 Content validation

TS does not execute or parse the JS content. It fetches, stores, and serves it as an opaque byte stream. No XSS sanitization is applied — the expectation is that the configured origins are trusted ad tech partners explicitly allowlisted by the publisher.

### 11.3 Path traversal

Wildcard path segments are validated to contain only URL-safe characters (`[A-Za-z0-9._-/]`) before being appended to the origin URL. Segments containing `..`, `%2e`, or other traversal patterns return `400`.

### 11.4 Response size limit

Origin responses larger than 8MB are rejected with `502` and not written to KV. This matches Fastly or similar KV per-entry body limits. Ad tech JS files are typically 100–500KB compressed; 8MB provides substantial headroom.

---

## 12. Open Questions

1. **Pre-warming:** Should TS populate KV entries at service deploy time rather than on first user request? Eliminates cold-miss latency for the first user after a deployment.
2. **Stale TTL extension:** Should the stale-on-error window be configurable per asset, or is a fixed 24-hour stale window sufficient for v1?
3. **Multiple publishers sharing a service:** The current design assumes one publisher per TS service instance. If multiple publishers share a service, the `[[js_assets]]` table needs a `publisher_id` field for disambiguation.
4. **Versioned wildcard paths and KV growth:** Each unique vendor bundle version is a separate KV entry. If the vendor deploys frequently, old entries accumulate. Should entries with wildcard paths have a shorter TTL or an explicit eviction policy?
5. **Vendor CDN URL configurability:** This PRD assumes the vendor makes their CDN URL configurable per publisher rather than hardcoded in their loader script. If that change is not made, TS will need to rewrite the string in the served loader body before caching. Confirm with the vendor before finalizing the implementation.

---

## 13. Success Metrics

| Metric | Target | Measurement |
|---|---|---|
| KV cache hit rate | >95% of asset requests served from KV (not origin) | KV read vs. origin fetch log counters |
| Third-party delivery consistency | Measurable increase in asset load success rate; reduction in failed or partially loaded ad stack across user segments | Publisher-reported asset load rates before/after |
| Origin fetch latency (cache miss) | p99 < 300ms (origin RTT + KV write) | Edge log `bereq_elapsed` |
| KV read latency (cache hit) | p99 < 5ms | Edge log timing |
| Stale responses | < 0.1% of total | `X-ts-cache: stale` header count in logs |
