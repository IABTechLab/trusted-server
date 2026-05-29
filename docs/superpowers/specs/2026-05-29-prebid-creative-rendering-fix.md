# Prebid Creative Rendering Fix Design

_Author · 2026-05-29_

---

## 1. Problem Statement

The Trusted Server server-side auction returns winning bids from PBS, but ads never
render on the Prebid path because `hb_adid` carries the wrong value.

The Prebid Universal Creative in GAM constructs the creative fetch URL as:

```
https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
```

TS currently sets `hb_adid` from `bid.adid` or `bid.id` (the OpenRTB bid ID /
impression ID). PBS actually caches the creative markup and returns the cache UUID
in `ext.prebid.cache.bids.cacheId`. The Universal Creative needs the **cache UUID**,
not the bid ID. The cache host and path are also not forwarded today.

**Effect:** GAM receives a wrong UUID, fetches nothing, and the slot renders empty.

---

## 2. Root Cause — Two Extraction Gaps

### Gap 1: Wrong `hb_adid` source

`prebid.rs` extracts:
```rust
let ad_id = bid_obj
    .get("adid")
    .or_else(|| bid_obj.get("id"))   // ← falls back to impression ID
    .and_then(|v| v.as_str())
    .map(String::from);
```

Real PBS response has (in `ext.prebid.cache.bids`):
```json
{
  "url": "https://openads.adsrvr.org/cache?uuid=f47447a0-b759-4f2f-9887-af458b79b570",
  "cacheId": "f47447a0-b759-4f2f-9887-af458b79b570"
}
```

`bid.id` = `"ad-header-0-_R_4uapbsnql8alb_"` — the impression ID, useless to the
creative renderer.

### Gap 2: Cache host and path not forwarded

`build_bid_map` in `publisher.rs` emits `hb_pb`, `hb_bidder`, `hb_adid`, `nurl`,
`burl`. It does not emit `hb_cache_host` or `hb_cache_path`. The Prebid Universal
Creative needs both to construct the fetch URL.

---

## 3. Non-Goals

- APS creative rendering — APS does not use PBS Cache. APS creative delivery is
  Amazon-owned and not addressed here.
- APS win detection over-fire — separate known limitation, separate issue.
- Dual bootstrap sync risk — separate maintenance issue.
- Slim-Prebid bundle — out of scope for Phase 1.

---

## 4. Design

### 4.1 New Fields on `Bid` (types.rs)

Add three fields to `Bid` to carry the PBS Cache coordinates extracted from the bid
response:

```rust
/// Prebid Cache UUID for this bid. Populated from
/// `ext.prebid.cache.bids.cacheId` in the PBS response.
/// Used as `hb_adid` targeting value in `window._ts.bids`.
/// None for non-PBS providers (e.g., APS) and PBS bids without cache enabled.
pub cache_id: Option<String>,

/// Prebid Cache host (e.g., `"openads.adsrvr.org"`). Populated from
/// the host component of `ext.prebid.cache.bids.url`.
/// Used as `hb_cache_host` targeting value.
pub cache_host: Option<String>,

/// Prebid Cache path (e.g., `"/cache"`). Populated from
/// the path component of `ext.prebid.cache.bids.url`.
/// Used as `hb_cache_path` targeting value.
pub cache_path: Option<String>,
```

### 4.2 Extraction in `prebid.rs`

In `parse_bid_object`, after extracting `nurl`/`burl`, extract the cache fields from
`ext.prebid.cache.bids`:

```rust
// Extract PBS Cache coordinates from ext.prebid.cache.bids
let cache_entry = bid_obj
    .get("ext")
    .and_then(|e| e.get("prebid"))
    .and_then(|p| p.get("cache"))
    .and_then(|c| c.get("bids"));

let cache_id = cache_entry
    .and_then(|c| c.get("cacheId"))
    .and_then(|v| v.as_str())
    .map(String::from);

let (cache_host, cache_path) = cache_entry
    .and_then(|c| c.get("url"))
    .and_then(|v| v.as_str())
    .and_then(|url_str| {
        url::Url::parse(url_str)
            .map_err(|e| log::debug!("PBS cache URL parse failed: {}", e))
            .ok()
    })
    .map(|u| {
        let host = u.host_str().map(String::from);
        // path() returns "/" for root — only use if non-trivial
        let path = u.path().to_string();
        let path = if path.is_empty() || path == "/" { None } else { Some(path) };
        (host, path)
    })
    .unwrap_or((None, None));
```

Note: `url` crate is already a workspace dependency. If not, parse host/path manually
by splitting on the first `/` after the scheme.

The `ad_id` field (from `bid.adid` / `bid.id`) is **kept** — it maps to the OpenRTB
`adid` / `id` field that APS and other non-PBS providers may use. The cache fields are
**in addition**, not replacing `ad_id`.

Populate all three fields on `AuctionBid`:
```rust
Ok(AuctionBid {
    ...,
    ad_id,
    cache_id,
    cache_host,
    cache_path,
    ...
})
```

### 4.3 `build_bid_map` in `publisher.rs`

Priority for `hb_adid`: use `cache_id` when present (PBS path), fall back to `ad_id`
(APS / other providers, backward compat):

```rust
// hb_adid: cache UUID when available (PBS), bid adid otherwise (APS/other)
let hb_adid = bid.cache_id.as_deref().or(bid.ad_id.as_deref());
if let Some(id) = hb_adid {
    obj.insert("hb_adid".to_string(), serde_json::Value::String(id.to_string()));
}

// Cache coordinates — only present for PBS bids with Prebid Cache enabled
if let Some(ref host) = bid.cache_host {
    obj.insert("hb_cache_host".to_string(), serde_json::Value::String(host.clone()));
}
if let Some(ref path) = bid.cache_path {
    obj.insert("hb_cache_path".to_string(), serde_json::Value::String(path.clone()));
}
```

### 4.4 What `window._ts.bids` looks like after the fix

```json
{
  "atf_sidebar_ad": {
    "hb_pb": "0.01",
    "hb_bidder": "thetradedesk",
    "hb_adid": "f47447a0-b759-4f2f-9887-af458b79b570",
    "hb_cache_host": "openads.adsrvr.org",
    "hb_cache_path": "/cache",
    "nurl": "https://...",
    "burl": "https://..."
  }
}
```

### 4.5 Win detection — no change required

`slotRenderEnded` checks:
```js
event.slot.getTargeting('hb_adid')[0] === bid.hb_adid
```

`adInit()` calls `setTargeting('hb_adid', cacheId)` with the cache UUID.  
`event.slot.getTargeting('hb_adid')[0]` returns that same cache UUID.  
`bid.hb_adid` is now also the cache UUID.  
Match holds. No change to the win detection logic.

### 4.6 GAM line item creative requirement (publisher action — not TS code)

This is a **hard dependency outside the TS codebase**. The publisher must configure
GAM line items with a server-side compatible Prebid creative. The standard
client-side Universal Creative calls `pbjs.renderAd()` which requires Prebid.js to be
loaded — it will not be at first render (slim-Prebid loads post-`window.load`).

The server-side compatible creative uses the `hb_cache_*` macros to fetch the markup
directly from PBS Cache:

```html
<script>
(function () {
  var host = '%%PATTERN:hb_cache_host%%';
  var path = '%%PATTERN:hb_cache_path%%';
  var uuid = '%%PATTERN:hb_adid%%';
  if (!host || !uuid) return;
  var url = 'https://' + host + path + '?uuid=' + encodeURIComponent(uuid);
  var xhr = new XMLHttpRequest();
  xhr.open('GET', url);
  xhr.onload = function () {
    if (xhr.status === 200) document.write(xhr.responseText);
  };
  xhr.send();
})();
</script>
```

Alternatively, publishers using the Prebid Universal Creative package can use:
```html
<script src="https://cdn.jsdelivr.net/npm/prebid-universal-creative/dist/creative.js"></script>
<script>
  var $sf = window.$sf;
  pbuc.renderAd({
    adId: '%%PATTERN:hb_adid%%',
    cacheHost: '%%PATTERN:hb_cache_host%%',
    cachePath: '%%PATTERN:hb_cache_path%%'
  });
</script>
```

> **This creative configuration is a publisher/ad ops action, not a TS code change.**
> Document it in the integration guide and verify during onboarding.

---

## 5. APS — Out of Scope

APS does not use PBS Cache. APS bids will have `cache_id = None`, `cache_host = None`,
`cache_path = None`. The existing `ad_id` fallback path remains for APS. APS creative
rendering depends on Amazon's own GAM creative tag — separate from the Prebid path.

APS win detection over-fires on the `!!bid.hb_bidder` fallback remain a known
limitation tracked separately.

---

## 6. Files Changed

| File | Change |
|---|---|
| `crates/trusted-server-core/src/auction/types.rs` | Add `cache_id`, `cache_host`, `cache_path` to `Bid` struct |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Extract `ext.prebid.cache.bids.{cacheId,url}` in `parse_bid_object` |
| `crates/trusted-server-core/src/publisher.rs` | `build_bid_map`: use `cache_id` for `hb_adid`, emit `hb_cache_host`/`hb_cache_path` |

Test files:
| File | Change |
|---|---|
| `crates/trusted-server-core/src/integrations/prebid.rs` tests | Add test: PBS response with cache entry → correct `hb_adid`, `hb_cache_host`, `hb_cache_path` injected |
| `crates/trusted-server-core/src/publisher.rs` tests | Add test: `build_bid_map` emits cache fields when present; falls back to `ad_id` when absent |

---

## 7. Testing

**Unit tests:**

1. `prebid.rs`: bid with `ext.prebid.cache.bids.cacheId` → `bid.cache_id = Some(uuid)`, `bid.cache_host = Some("openads.adsrvr.org")`, `bid.cache_path = Some("/cache")`
2. `prebid.rs`: bid without `ext.prebid.cache` → `bid.cache_id = None`, `bid.cache_host = None`, `bid.cache_path = None`
3. `prebid.rs`: bid with only `adid` (no cache) → `bid.ad_id = Some(...)`, `bid.cache_id = None`
4. `prebid.rs`: bid with malformed cache URL → `cache_host = None`, `cache_path = None`, no panic
5. `publisher.rs` `build_bid_map`: bid with `cache_id` → `hb_adid` uses `cache_id`, `hb_cache_host`/`hb_cache_path` emitted
6. `publisher.rs` `build_bid_map`: bid with no `cache_id` but has `ad_id` → `hb_adid` falls back to `ad_id`, no cache keys emitted
7. `publisher.rs` `build_bid_map`: APS bid (no `cache_id`, no `ad_id`) → no `hb_adid` emitted
8. `types.rs`: `Bid` with all three cache fields round-trips through `serde_json::to_string` / `from_str`

> **Note for implementer:** `make_bid()` or equivalent `Bid` construction helpers in test modules
> must be updated to initialise `cache_id`, `cache_host`, `cache_path` to `None`
> (they will fail to compile otherwise once the fields are added to the struct).

**Integration verification (manual):**

After deploying, verify `window._ts.bids` in browser devtools shows `hb_cache_host`
and `hb_cache_path` present. Verify `hb_adid` matches the UUID in
`ext.prebid.cache.bids.cacheId` from the raw PBS response.

---

## 8. Rollout Dependency Checklist

Before this fix has end-to-end effect:

- [ ] TS: this PR merged and deployed
- [ ] GAM: publisher ad ops updates all Prebid line item creatives to the server-side
      cache-fetch variant (see §4.6)
- [ ] PBS: Prebid Cache enabled and populated (confirmed from real response — already
      working)
- [ ] Verify: `window._ts.bids` shows correct cache UUID in `hb_adid` after deploy

---

## 9. Known Remaining Gaps (not in scope)

| Gap | Severity | Tracking |
|---|---|---|
| APS win detection over-fires nurl/burl | P1 | Separate issue |
| Dual bootstrap (`gpt_bootstrap.js` + `installTsAdInit`) sync risk | P2 | Separate issue |
| Slim-Prebid bundle not yet built | Phase 2 | §9.8 of design doc |
