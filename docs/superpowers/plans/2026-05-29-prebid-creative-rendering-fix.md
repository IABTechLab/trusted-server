# Prebid Creative Rendering Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `hb_adid` to carry the PBS Cache UUID (not the OpenRTB bid ID) so the Prebid Universal Creative in GAM can fetch and render the correct creative markup.

**Architecture:** Three-file change: add `cache_id`/`cache_host`/`cache_path` fields to the shared `Bid` struct in `types.rs`, extract these from `ext.prebid.cache.bids` in `prebid.rs`'s `parse_bid`, then emit them as `hb_adid`/`hb_cache_host`/`hb_cache_path` in `publisher.rs`'s `build_bid_map`. `AuctionBid` in `prebid.rs` is a type alias for `Bid` (`use ... Bid as AuctionBid`), so only one struct needs the new fields.

**Tech Stack:** Rust 2024, `serde`, `url` crate (already in workspace deps at v2.5.8), `cargo test --workspace`

---

## Context for all tasks

- **Branch:** `fix/server-side-ad-template-entrypoint` (already checked out)
- **Spec:** `docs/superpowers/specs/2026-05-29-prebid-creative-rendering-fix.md`
- **Error handling:** `error-stack` (`Report<E>`), not anyhow. Use `expect("should ...")` not `unwrap()`.
- **No `println!`/`eprintln!`** — use `log::` macros.
- **All public items must have doc comments.**
- CI gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`

---

## Task 1: Add cache fields to `Bid` struct and fix all construction sites

**What:** Add three new `Option<String>` fields to `Bid`. Since Rust struct literals are exhaustive, every place that constructs a `Bid { ... }` in the codebase will fail to compile until the new fields are added. Fix all of them with `None` defaults (except the APS provider which constructs a real `Bid` — also `None` since APS doesn't use PBS Cache).

**Files:**
- Modify: `crates/trusted-server-core/src/auction/types.rs:200` (after `ad_id` field)
- Modify (test helpers/literals — add `None` fields):
  - `crates/trusted-server-core/src/auction/types.rs:314` (`make_bid` helper)
  - `crates/trusted-server-core/src/auction/types.rs:445` (inline `Bid` literal)
  - `crates/trusted-server-core/src/publisher.rs:2616` (`make_bid` helper)
  - `crates/trusted-server-core/src/publisher.rs:2714` (inline `Bid` literal)
  - `crates/trusted-server-core/src/auction/orchestrator.rs:1121,1138,1278,1325,1358` (test `Bid` literals)
  - `crates/trusted-server-core/src/integrations/aps.rs:442` (production `Bid` construction)

**Steps:**

- [ ] **Step 1: Add three fields to `Bid` struct in `types.rs`**

  In `crates/trusted-server-core/src/auction/types.rs`, after line 200 (`pub ad_id: Option<String>,`), add:

  ```rust
  /// Prebid Cache UUID for this bid.
  ///
  /// Populated from `ext.prebid.cache.bids.cacheId` in the PBS response.
  /// Used as `hb_adid` targeting value in `window._ts.bids`. `None` for
  /// non-PBS providers (e.g., APS) and PBS bids without Prebid Cache enabled.
  pub cache_id: Option<String>,
  /// Prebid Cache host (e.g., `"openads.adsrvr.org"`).
  ///
  /// Populated from the host of `ext.prebid.cache.bids.url`. Used as
  /// `hb_cache_host` targeting value. `None` when cache is absent.
  pub cache_host: Option<String>,
  /// Prebid Cache path (e.g., `"/cache"`).
  ///
  /// Populated from the path of `ext.prebid.cache.bids.url`. Used as
  /// `hb_cache_path` targeting value. `None` when cache is absent.
  pub cache_path: Option<String>,
  ```

- [ ] **Step 2: Verify compile fails as expected**

  ```bash
  cargo check --package trusted-server-core 2>&1 | grep "missing field"
  ```

  Expected: multiple errors about missing `cache_id`, `cache_host`, `cache_path` in `Bid` struct literals. This confirms every construction site will be found.

- [ ] **Step 3: Fix `make_bid` helper in `types.rs` (line ~314)**

  Add three `None` fields to the `Bid {}` literal inside the `make_bid` test helper:

  ```rust
  fn make_bid(bidder: &str) -> Bid {
      Bid {
          slot_id: "slot-1".to_string(),
          price: Some(1.0),
          currency: "USD".to_string(),
          creative: None,
          adomain: None,
          bidder: bidder.to_string(),
          width: 300,
          height: 250,
          nurl: None,
          burl: None,
          ad_id: None,
          cache_id: None,
          cache_host: None,
          cache_path: None,
          metadata: HashMap::new(),
      }
  }
  ```

- [ ] **Step 4: Fix inline `Bid` literal in `types.rs` (line ~445)**

  Find the `Bid {` literal around line 445 in the test section of `types.rs`. Add:
  ```rust
  cache_id: None,
  cache_host: None,
  cache_path: None,
  ```

- [ ] **Step 5: Fix `make_bid` helper in `publisher.rs` (line ~2616)**

  In the `make_bid` test helper function in `publisher.rs`, add to the `Bid {}` literal:
  ```rust
  cache_id: None,
  cache_host: None,
  cache_path: None,
  ```

- [ ] **Step 6: Fix inline `Bid` literal in `publisher.rs` (line ~2714)**

  Find the `Bid {` literal around line 2714 in `publisher.rs` tests. Add:
  ```rust
  cache_id: None,
  cache_host: None,
  cache_path: None,
  ```

- [ ] **Step 7: Fix five `Bid` literals in `orchestrator.rs` (lines ~1121,1138,1278,1325,1358)**

  Add to each of the five `Bid {}` literals in the test section of `orchestrator.rs`:
  ```rust
  cache_id: None,
  cache_host: None,
  cache_path: None,
  ```

- [ ] **Step 8: Fix APS production `Bid` construction in `aps.rs` (line ~442)**

  In `aps.rs`, inside `parse_aps_response` (or wherever the `Ok(Bid { ... })` is around line 442), add:
  ```rust
  cache_id: None,
  cache_host: None,
  cache_path: None,
  ```

  APS does not use PBS Cache — these fields are intentionally `None` for APS bids.

- [ ] **Step 9: Verify compile succeeds**

  ```bash
  cargo check --package trusted-server-core 2>&1 | grep -E "^error"
  ```

  Expected: no output (clean compile).

- [ ] **Step 10: Run tests to confirm nothing regressed**

  ```bash
  cargo test --workspace 2>&1 | tail -5
  ```

  Expected: all tests pass.

- [ ] **Step 11: Run clippy and fmt**

  ```bash
  cargo fmt --all
  cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
  ```

  Expected: clean.

- [ ] **Step 12: Commit**

  ```bash
  git add crates/trusted-server-core/src/auction/types.rs \
          crates/trusted-server-core/src/publisher.rs \
          crates/trusted-server-core/src/auction/orchestrator.rs \
          crates/trusted-server-core/src/integrations/aps.rs
  git commit -m "Add cache_id, cache_host, cache_path fields to Bid struct"
  ```

---

## Task 2: Extract PBS Cache fields in `prebid.rs` `parse_bid` + tests

**What:** After extracting `ad_id` in `parse_bid`, extract `ext.prebid.cache.bids.cacheId` as `cache_id` and split `ext.prebid.cache.bids.url` into `cache_host` + `cache_path`. Populate all three new fields on the returned `AuctionBid`. Add TDD tests first.

**Files:**
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:1362–1391` (extraction + struct literal)
- Test: `crates/trusted-server-core/src/integrations/prebid.rs` (test module near bottom)

**Steps:**

- [ ] **Step 1: Write the failing tests**

  Find the `#[cfg(test)]` module in `prebid.rs`. Add these tests (they will fail because extraction doesn't exist yet):

  ```rust
  #[test]
  fn parse_bid_extracts_cache_id_from_ext_prebid_cache_bids() {
      // Real PBS response shape from auction_response.json
      let bid_json = serde_json::json!({
          "id": "bid-id-123",
          "impid": "atf_sidebar_ad",
          "price": 1.50,
          "adm": "<div>ad</div>",
          "w": 300,
          "h": 250,
          "ext": {
              "prebid": {
                  "cache": {
                      "bids": {
                          "url": "https://openads.adsrvr.org/cache?uuid=f47447a0-b759-4f2f-9887-af458b79b570",
                          "cacheId": "f47447a0-b759-4f2f-9887-af458b79b570"
                      }
                  }
              }
          }
      });
      let provider = PrebidAuctionProvider::new(base_config());
      let bid = provider
          .parse_bid(&bid_json, "thetradedesk")
          .expect("should parse bid");
      assert_eq!(
          bid.cache_id.as_deref(),
          Some("f47447a0-b759-4f2f-9887-af458b79b570"),
          "should extract cacheId as cache_id"
      );
      assert_eq!(
          bid.cache_host.as_deref(),
          Some("openads.adsrvr.org"),
          "should extract host from cache URL"
      );
      assert_eq!(
          bid.cache_path.as_deref(),
          Some("/cache"),
          "should extract path from cache URL"
      );
  }

  #[test]
  fn parse_bid_sets_cache_fields_to_none_when_no_cache_entry() {
      let bid_json = serde_json::json!({
          "id": "bid-id-456",
          "impid": "atf_sidebar_ad",
          "price": 0.50,
          "w": 300,
          "h": 250
          // no ext.prebid.cache
      });
      let provider = PrebidAuctionProvider::new(base_config());
      let bid = provider
          .parse_bid(&bid_json, "appnexus")
          .expect("should parse bid");
      assert!(bid.cache_id.is_none(), "should be None when cache absent");
      assert!(bid.cache_host.is_none(), "should be None when cache absent");
      assert!(bid.cache_path.is_none(), "should be None when cache absent");
  }

  #[test]
  fn parse_bid_handles_malformed_cache_url_gracefully() {
      let bid_json = serde_json::json!({
          "id": "bid-id-789",
          "impid": "atf_sidebar_ad",
          "price": 0.50,
          "w": 300,
          "h": 250,
          "ext": {
              "prebid": {
                  "cache": {
                      "bids": {
                          "url": "not-a-valid-url",
                          "cacheId": "some-uuid"
                      }
                  }
              }
          }
      });
      let provider = PrebidAuctionProvider::new(base_config());
      let bid = provider
          .parse_bid(&bid_json, "appnexus")
          .expect("should parse bid without panicking");
      assert_eq!(
          bid.cache_id.as_deref(),
          Some("some-uuid"),
          "should still extract cacheId even if URL is malformed"
      );
      assert!(bid.cache_host.is_none(), "should be None when URL parse fails");
      assert!(bid.cache_path.is_none(), "should be None when URL parse fails");
  }

  #[test]
  fn parse_bid_preserves_ad_id_alongside_cache_id() {
      let bid_json = serde_json::json!({
          "id": "bid-impression-id",
          "impid": "atf_sidebar_ad",
          "adid": "bidder-ad-id-abc",
          "price": 1.0,
          "w": 300,
          "h": 250,
          "ext": {
              "prebid": {
                  "cache": {
                      "bids": {
                          "url": "https://cache.example.com/cache",
                          "cacheId": "cache-uuid-xyz"
                      }
                  }
              }
          }
      });
      let provider = PrebidAuctionProvider::new(base_config());
      let bid = provider
          .parse_bid(&bid_json, "appnexus")
          .expect("should parse bid");
      assert_eq!(
          bid.ad_id.as_deref(),
          Some("bidder-ad-id-abc"),
          "should keep ad_id from adid field"
      );
      assert_eq!(
          bid.cache_id.as_deref(),
          Some("cache-uuid-xyz"),
          "should extract cache UUID separately"
      );
  }
  ```

  Note: `base_config()` and `PrebidAuctionProvider::new()` are the standard test construction pattern used throughout the existing `prebid.rs` test module. `parse_bid` is a private method but is accessible from the `#[cfg(test)]` module in the same file.

- [ ] **Step 2: Run tests to verify they fail**

  ```bash
  cargo test --package trusted-server-core parse_bid_extracts_cache_id 2>&1 | tail -15
  ```

  Expected: compile error (`no field 'cache_id' on type 'Bid'`) or test failure. Either confirms the extraction code is missing.

- [ ] **Step 3: Add cache extraction to `parse_bid` in `prebid.rs`**

  In `parse_bid` (around line 1362), after the `ad_id` extraction block and before the `Ok(AuctionBid { ... })`, add:

  ```rust
  // Extract PBS Cache coordinates from ext.prebid.cache.bids.
  // The Prebid Universal Creative uses cacheId as hb_adid and the host/path
  // to construct the fetch URL: https://<host><path>?uuid=<cacheId>
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
              .map_err(|e| log::debug!("PBS cache URL parse failed: {e}"))
              .ok()
      })
      .map(|u| {
          let host = u.host_str().map(String::from);
          let path = u.path().to_string();
          let path = if path.is_empty() || path == "/" {
              None
          } else {
              Some(path)
          };
          (host, path)
      })
      .unwrap_or((None, None));

  if cache_id.is_some() && cache_host.is_none() {
      log::warn!(
          "PBS bid has cache UUID but cache URL could not be parsed — \
           creative will fail to render for slot '{slot_id}'"
      );
  }
  ```

  Then add the three fields to the `Ok(AuctionBid { ... })` struct literal (around line 1377):

  ```rust
  Ok(AuctionBid {
      slot_id,
      price: Some(price),
      currency: DEFAULT_CURRENCY.to_string(),
      creative,
      adomain,
      bidder: seat.to_string(),
      width,
      height,
      nurl,
      burl,
      ad_id,
      cache_id,
      cache_host,
      cache_path,
      metadata: std::collections::HashMap::new(),
  })
  ```

- [ ] **Step 4: Run tests to verify they pass**

  ```bash
  cargo test --package trusted-server-core parse_bid 2>&1 | tail -20
  ```

  Expected: all 4 new tests pass.

- [ ] **Step 5: Run full test suite**

  ```bash
  cargo test --workspace 2>&1 | tail -5
  ```

  Expected: all tests pass.

- [ ] **Step 6: Run clippy and fmt**

  ```bash
  cargo fmt --all
  cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
  ```

  Expected: clean. If clippy warns about the `log::debug!` return value being unused inside `map_err`, suppress with `let _ = ...` or restructure.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-core/src/integrations/prebid.rs
  git commit -m "Extract PBS Cache UUID and endpoint from bid ext into Bid fields"
  ```

---

## Task 3: Emit cache fields in `build_bid_map` + update tests

**What:** Change `build_bid_map` to use `bid.cache_id` for `hb_adid` (falling back to `bid.ad_id` for APS/other providers), and emit `hb_cache_host`/`hb_cache_path` when present. Update the existing `bid_map_includes_nurl_and_burl` test (which currently passes `"abc123"` as `ad_id` and asserts `hb_adid = "abc123"`) to use a cache-based bid. Add new tests covering cache fields and fallback path.

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs:1311–1342` (`build_bid_map`)
- Modify: `crates/trusted-server-core/src/publisher.rs:2608–2630` (`make_bid` helper — add cache params)
- Modify: `crates/trusted-server-core/src/publisher.rs:2666–2707` (existing `bid_map_includes_nurl_and_burl` test)
- Test: `crates/trusted-server-core/src/publisher.rs` (new tests in the existing test module)

**Steps:**

- [ ] **Step 1: Write new failing tests for cache field emission**

  Add these tests to the `#[cfg(test)]` module in `publisher.rs`, near the existing `bid_map_includes_nurl_and_burl` test:

  ```rust
  #[test]
  fn bid_map_uses_cache_id_for_hb_adid_when_present() {
      let mut winning_bids = HashMap::new();
      winning_bids.insert(
          "atf_sidebar_ad".to_string(),
          Bid {
              slot_id: "atf_sidebar_ad".to_string(),
              price: Some(1.50),
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "thetradedesk".to_string(),
              width: 300,
              height: 250,
              nurl: None,
              burl: None,
              ad_id: Some("bid-impression-id".to_string()),
              cache_id: Some("f47447a0-b759-4f2f-9887-af458b79b570".to_string()),
              cache_host: Some("openads.adsrvr.org".to_string()),
              cache_path: Some("/cache".to_string()),
              metadata: Default::default(),
          },
      );
      let map = build_bid_map(&winning_bids, PriceGranularity::Dense);
      let obj = map
          .get("atf_sidebar_ad")
          .expect("should have entry")
          .as_object()
          .expect("should be object");

      assert_eq!(
          obj.get("hb_adid").and_then(|v| v.as_str()),
          Some("f47447a0-b759-4f2f-9887-af458b79b570"),
          "should use cache_id for hb_adid, not ad_id"
      );
      assert_eq!(
          obj.get("hb_cache_host").and_then(|v| v.as_str()),
          Some("openads.adsrvr.org"),
          "should emit hb_cache_host"
      );
      assert_eq!(
          obj.get("hb_cache_path").and_then(|v| v.as_str()),
          Some("/cache"),
          "should emit hb_cache_path"
      );
  }

  #[test]
  fn bid_map_falls_back_to_ad_id_when_cache_id_absent() {
      let mut winning_bids = HashMap::new();
      winning_bids.insert(
          "atf_sidebar_ad".to_string(),
          Bid {
              slot_id: "atf_sidebar_ad".to_string(),
              price: Some(0.50),
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "aps-amazon".to_string(),
              width: 300,
              height: 250,
              nurl: None,
              burl: None,
              ad_id: Some("aps-bid-token".to_string()),
              cache_id: None,
              cache_host: None,
              cache_path: None,
              metadata: Default::default(),
          },
      );
      let map = build_bid_map(&winning_bids, PriceGranularity::Dense);
      let obj = map
          .get("atf_sidebar_ad")
          .expect("should have entry")
          .as_object()
          .expect("should be object");

      assert_eq!(
          obj.get("hb_adid").and_then(|v| v.as_str()),
          Some("aps-bid-token"),
          "should fall back to ad_id when cache_id absent"
      );
      assert!(
          obj.get("hb_cache_host").is_none(),
          "should not emit hb_cache_host when absent"
      );
      assert!(
          obj.get("hb_cache_path").is_none(),
          "should not emit hb_cache_path when absent"
      );
  }

  #[test]
  fn bid_map_omits_hb_adid_when_both_cache_id_and_ad_id_absent() {
      let mut winning_bids = HashMap::new();
      winning_bids.insert(
          "atf_sidebar_ad".to_string(),
          Bid {
              slot_id: "atf_sidebar_ad".to_string(),
              price: Some(0.50),
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "amazon-aps".to_string(),
              width: 300,
              height: 250,
              nurl: None,
              burl: None,
              ad_id: None,
              cache_id: None,
              cache_host: None,
              cache_path: None,
              metadata: Default::default(),
          },
      );
      let map = build_bid_map(&winning_bids, PriceGranularity::Dense);
      let obj = map
          .get("atf_sidebar_ad")
          .expect("should have entry")
          .as_object()
          .expect("should be object");

      assert!(
          obj.get("hb_adid").is_none(),
          "should omit hb_adid when no cache_id and no ad_id"
      );
  }
  ```

- [ ] **Step 2: Run tests to verify they fail**

  ```bash
  cargo test --package trusted-server-core bid_map_uses_cache_id 2>&1 | tail -15
  ```

  Expected: test fails — `hb_adid` returns `"bid-impression-id"` (the wrong value) instead of the cache UUID, and `hb_cache_host`/`hb_cache_path` are not emitted.

- [ ] **Step 3: Update `build_bid_map` in `publisher.rs`**

  Replace the current `hb_adid` emission block (lines ~1326–1331) and the `nurl`/`burl` block with:

  ```rust
  // hb_adid: PBS Cache UUID when present (Prebid Universal Creative uses this
  // as the cache lookup key). Falls back to ad_id for APS and other non-PBS
  // providers. Note: ad_id (OpenRTB bid ID) is NOT the same as the cache UUID.
  let hb_adid = bid.cache_id.as_deref().or(bid.ad_id.as_deref());
  if let Some(id) = hb_adid {
      obj.insert(
          "hb_adid".to_string(),
          serde_json::Value::String(id.to_string()),
      );
  }

  // Cache endpoint coordinates — only present for PBS bids with Prebid Cache.
  // The Prebid Universal Creative constructs:
  //   https://<hb_cache_host><hb_cache_path>?uuid=<hb_adid>
  if let Some(ref host) = bid.cache_host {
      obj.insert(
          "hb_cache_host".to_string(),
          serde_json::Value::String(host.clone()),
      );
  }
  if let Some(ref path) = bid.cache_path {
      obj.insert(
          "hb_cache_path".to_string(),
          serde_json::Value::String(path.clone()),
      );
  }

  if let Some(ref nurl) = bid.nurl {
      obj.insert("nurl".to_string(), serde_json::Value::String(nurl.clone()));
  }
  if let Some(ref burl) = bid.burl {
      obj.insert("burl".to_string(), serde_json::Value::String(burl.clone()));
  }
  ```

- [ ] **Step 4: Update the existing `bid_map_includes_nurl_and_burl` test**

  The existing test at line ~2666 constructs a bid via `make_bid("atf_sidebar_ad", 1.50, "kargo", "abc123", ...)` and asserts `hb_adid = "abc123"`. Update `make_bid` to accept optional `cache_id`, `cache_host`, `cache_path`, OR create a separate variant. The simplest fix: update the assertion in the existing test to reflect the new priority logic.

  The test currently passes `ad_id = "abc123"` and `cache_id = None`. After the fix, `hb_adid` should still be `"abc123"` (fallback path). So the existing assertion is correct — just verify it still passes. No change needed to that test body. Just update `make_bid` to set the new fields to `None`:

  ```rust
  fn make_bid(
      slot_id: &str,
      price: f64,
      bidder: &str,
      ad_id: &str,
      nurl: &str,
      burl: &str,
  ) -> Bid {
      Bid {
          slot_id: slot_id.to_string(),
          price: Some(price),
          currency: "USD".to_string(),
          creative: None,
          adomain: None,
          bidder: bidder.to_string(),
          width: 300,
          height: 250,
          nurl: Some(nurl.to_string()),
          burl: Some(burl.to_string()),
          ad_id: Some(ad_id.to_string()),
          cache_id: None,
          cache_host: None,
          cache_path: None,
          metadata: Default::default(),
      }
  }
  ```

  Also update the assertion comment at line ~2694 from `"should include ad_id"` to `"should fall back to ad_id when no cache_id"`.

- [ ] **Step 5: Run all new tests**

  ```bash
  cargo test --package trusted-server-core bid_map 2>&1 | tail -20
  ```

  Expected: all `bid_map_*` tests pass, including both new and existing.

- [ ] **Step 6: Add round-trip serialization test for `Bid`**

  Add this test to the `#[cfg(test)]` module in `types.rs`:

  ```rust
  #[test]
  fn bid_with_cache_fields_round_trips_through_json() {
      let bid = Bid {
          slot_id: "atf".to_string(),
          price: Some(1.50),
          currency: "USD".to_string(),
          creative: None,
          adomain: None,
          bidder: "thetradedesk".to_string(),
          width: 300,
          height: 250,
          nurl: None,
          burl: None,
          ad_id: Some("bid-id".to_string()),
          cache_id: Some("cache-uuid".to_string()),
          cache_host: Some("cache.example.com".to_string()),
          cache_path: Some("/pbc/v1/cache".to_string()),
          metadata: HashMap::new(),
      };
      let json = serde_json::to_string(&bid).expect("should serialize Bid");
      let restored: Bid = serde_json::from_str(&json).expect("should deserialize Bid");
      assert_eq!(restored.cache_id.as_deref(), Some("cache-uuid"), "should round-trip cache_id");
      assert_eq!(restored.cache_host.as_deref(), Some("cache.example.com"), "should round-trip cache_host");
      assert_eq!(restored.cache_path.as_deref(), Some("/pbc/v1/cache"), "should round-trip cache_path");
  }
  ```

  Run:
  ```bash
  cargo test --package trusted-server-core bid_with_cache_fields_round_trips 2>&1 | tail -5
  ```
  Expected: PASS.

- [ ] **Step 7: Run full CI suite**

  ```bash
  cargo test --workspace 2>&1 | tail -5
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
  ```

  Expected: all pass, no warnings.

- [ ] **Step 8: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs \
          crates/trusted-server-core/src/auction/types.rs
  git commit -m "Emit hb_adid from PBS Cache UUID and add hb_cache_host/hb_cache_path to bid map"
  ```

---

## Final verification

- [ ] Run `cargo test --workspace` — all pass
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` — clean
- [ ] Run `cargo fmt --all -- --check` — clean
- [ ] In browser devtools after deploy: `window._ts.bids` shows `hb_cache_host`, `hb_cache_path`, and `hb_adid` matching the UUID in `ext.prebid.cache.bids.cacheId` from the raw PBS response

---

## Rollout reminder (from spec §8)

1. TS: this branch deployed
2. GAM: ad ops updates Prebid line item creatives to server-side cache-fetch variant (see spec §4.6)
3. PBS: Prebid Cache already enabled (confirmed from real response)
4. Verify in devtools
