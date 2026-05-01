# Server-Side Ad Templates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable the Fastly edge to fire a full header-bidding auction (PBS + APS) in parallel with the origin fetch, injecting `window.__ts_ad_slots` and `window.__ts_request_id` into `<head>`, then serving cached bid results via a new `/ts-bids` endpoint so the client can drive GPT directly — without Prebid.js and without blocking page rendering.

**Architecture:** A new `creative-opportunities.toml` holds per-URL slot templates. At request time, the publisher path matches the URL, mints a UUID (`request_id`), and fires the auction + origin fetch concurrently via `send_async()`. Only `window.__ts_ad_slots` and `window.__ts_request_id` are injected at `<head>` open — **`</head>` flushes immediately with no auction wait**. When the auction completes, results are stored in a new in-process `BidCache` keyed by `request_id`. The browser's tsjs bundle fetches `GET /ts-bids?rid=<request_id>` to retrieve bid targeting; this endpoint long-polls until the auction completes or the deadline fires.

**Tech Stack:** Rust 2024, `lol_html` 2.7.2 (existing), `glob` crate (new workspace dep), `serde`/`toml` (existing), `uuid` v4 (existing workspace dep), `std::sync::Mutex` + `std::time::Instant` for in-process cache (30s TTL), `AuctionOrchestrator::run_auction` (existing `async fn`), TypeScript for GPT shim extension.

---

## File Map

### New files

| File                                                       | Responsibility                                                                              |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `creative-opportunities.toml`                              | Slot template definitions (page patterns, formats, floor prices, per-provider params)       |
| `crates/trusted-server-core/src/creative_opportunities.rs` | Config types, TOML parsing, URL glob matching, slot→`AdSlot` conversion, startup validation |
| `crates/trusted-server-core/src/price_bucket.rs`           | Prebid price granularity tables; converts `f64` CPM to `hb_pb` string                       |
| `crates/trusted-server-core/src/bid_cache.rs`              | In-process auction result cache keyed by `request_id`; 30s TTL; long-poll via blocking poll |

### Modified files

| File                                                    | Change summary                                                                                                                                                                                                          |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                                            | Add `glob = "0.3"` to `[workspace.dependencies]`                                                                                                                                                                        |
| `crates/trusted-server-core/Cargo.toml`                 | Add `glob = { workspace = true }`                                                                                                                                                                                       |
| `crates/trusted-server-core/src/auction/types.rs`       | Add `MediaType::banner()` constructor; add `ad_id: Option<String>` to `Bid`                                                                                                                                             |
| `crates/trusted-server-core/src/settings.rs`            | Add `creative_opportunities: Option<CreativeOpportunitiesConfig>` to `Settings`                                                                                                                                         |
| `trusted-server.toml`                                   | Add `[creative_opportunities]` section                                                                                                                                                                                  |
| `crates/trusted-server-core/build.rs`                   | Validate slot IDs at build time using inline regex (no module import)                                                                                                                                                   |
| `crates/trusted-server-core/src/html_processor.rs`      | Add `ad_slots_script: Option<String>` (contains both `__ts_ad_slots` + `__ts_request_id`) to `HtmlProcessorConfig`; inject at head-open only — **no `</head>` hold**                                                    |
| `crates/trusted-server-core/src/publisher.rs`           | Convert `handle_publisher_request` to `async fn`; add `orchestrator` + `bid_cache` params; fire auction + origin concurrently; write result to `bid_cache`; inject head globals; set `Cache-Control` when slots matched |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Await the now-async handler; pass orchestrator + bid_cache references; add `/ts-bids` route handler (long-poll, returns bid JSON from `bid_cache`)                                                                      |
| `crates/trusted-server-core/src/integrations/gpt.rs`    | Extend `head_inserts()` to emit `__tsAdInit` that fetches `/ts-bids?rid=<request_id>` and applies bid targeting after resolution                                                                                        |
| `crates/js/lib/src/integrations/gpt/index.ts`           | Add `installTsAdInit` with `/ts-bids` fetch + `bidsPromise` pattern + `slotRenderEnded` burl-firing logic                                                                                                               |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Add `fire_nurl_at_edge` config key; fire nurl fire-and-forget after auction                                                                                                                                             |

---

## Task 1: Add `glob` workspace dependency

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/trusted-server-core/Cargo.toml`

- [ ] **Step 1: Write a failing compile test**

  In `crates/trusted-server-core/src/lib.rs`, temporarily add:

  ```rust
  // Compilation test — remove after Step 4
  use glob::Pattern as _;
  ```

  Run: `cargo check --package trusted-server-core`
  Expected: error `use of undeclared crate or module 'glob'`

- [ ] **Step 2: Add glob to workspace `Cargo.toml`**

  Under `[workspace.dependencies]`, add:

  ```toml
  glob = "0.3"
  ```

- [ ] **Step 3: Add glob to core crate**

  In `crates/trusted-server-core/Cargo.toml` under `[dependencies]`, add:

  ```toml
  glob = { workspace = true }
  ```

- [ ] **Step 4: Remove temp import, verify compile**

  Remove the temp `use glob::Pattern as _;` from `lib.rs`.
  Run: `cargo check --package trusted-server-core`
  Expected: clean compile

- [ ] **Step 5: Commit**

  ```bash
  git add Cargo.toml crates/trusted-server-core/Cargo.toml
  git commit -m "Add glob workspace dependency for URL pattern matching"
  ```

---

## Task 2: `price_bucket.rs` — Prebid price granularity

**Files:**

- Create: `crates/trusted-server-core/src/price_bucket.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

The `hb_pb` value in bid responses is a discretized bucket string from Prebid's granularity tables. "Dense" is the default used in most Prebid deployments.

- [ ] **Step 1: Write failing tests**

  Create `crates/trusted-server-core/src/price_bucket.rs` with only the tests:

  ```rust
  //! Prebid price granularity bucketing.

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn dense_below_3_increments_by_0_01() {
          assert_eq!(price_bucket(0.0, PriceGranularity::Dense), "0.00");
          assert_eq!(price_bucket(0.01, PriceGranularity::Dense), "0.01");
          assert_eq!(price_bucket(0.015, PriceGranularity::Dense), "0.01");
          assert_eq!(price_bucket(1.23, PriceGranularity::Dense), "1.23");
          assert_eq!(price_bucket(2.99, PriceGranularity::Dense), "2.99");
      }

      #[test]
      fn dense_3_to_8_increments_by_0_05() {
          assert_eq!(price_bucket(3.00, PriceGranularity::Dense), "3.00");
          assert_eq!(price_bucket(3.03, PriceGranularity::Dense), "3.00");
          assert_eq!(price_bucket(3.05, PriceGranularity::Dense), "3.05");
          assert_eq!(price_bucket(7.99, PriceGranularity::Dense), "7.95");
      }

      #[test]
      fn dense_8_to_20_increments_by_0_50() {
          assert_eq!(price_bucket(8.00, PriceGranularity::Dense), "8.00");
          assert_eq!(price_bucket(8.49, PriceGranularity::Dense), "8.00");
          assert_eq!(price_bucket(8.50, PriceGranularity::Dense), "8.50");
          assert_eq!(price_bucket(19.99, PriceGranularity::Dense), "19.50");
      }

      #[test]
      fn dense_above_20_caps_at_20() {
          assert_eq!(price_bucket(20.00, PriceGranularity::Dense), "20.00");
          assert_eq!(price_bucket(50.00, PriceGranularity::Dense), "20.00");
      }

      #[test]
      fn low_increments_by_0_50_capped_at_5() {
          assert_eq!(price_bucket(0.49, PriceGranularity::Low), "0.00");
          assert_eq!(price_bucket(0.50, PriceGranularity::Low), "0.50");
          assert_eq!(price_bucket(5.01, PriceGranularity::Low), "5.00");
      }

      #[test]
      fn medium_increments_by_0_10_capped_at_20() {
          assert_eq!(price_bucket(1.05, PriceGranularity::Medium), "1.00");
          assert_eq!(price_bucket(1.10, PriceGranularity::Medium), "1.10");
          assert_eq!(price_bucket(20.5, PriceGranularity::Medium), "20.00");
      }

      #[test]
      fn high_increments_by_0_01_capped_at_20() {
          assert_eq!(price_bucket(1.234, PriceGranularity::High), "1.23");
          assert_eq!(price_bucket(20.5, PriceGranularity::High), "20.00");
      }

      #[test]
      fn auto_routes_through_dense() {
          assert_eq!(
              price_bucket(2.53, PriceGranularity::Auto),
              price_bucket(2.53, PriceGranularity::Dense)
          );
      }
  }
  ```

  Run: `cargo test -p trusted-server-core price_bucket`
  Expected: compile error (module not yet exported from lib.rs)

- [ ] **Step 2: Implement price_bucket.rs**

  ```rust
  //! Prebid price granularity bucketing.
  //!
  //! Converts a raw CPM to the `hb_pb` price bucket string sent to GAM as targeting.
  //! Mirrors Prebid.js built-in granularity tables exactly.

  use serde::{Deserialize, Serialize};

  /// Prebid price granularity setting.
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
  #[serde(rename_all = "lowercase")]
  pub enum PriceGranularity {
      Low,
      Medium,
      #[default]
      Dense,
      High,
      Auto,
  }

  impl PriceGranularity {
      /// Returns the `Dense` variant — used as a `#[serde(default = ...)]` fn pointer.
      pub fn dense() -> Self {
          Self::Dense
      }
  }

  /// Convert a raw CPM (`f64`) to the `hb_pb` price bucket string.
  pub fn price_bucket(cpm: f64, granularity: PriceGranularity) -> String {
      if cpm <= 0.0 {
          return "0.00".to_string();
      }
      match granularity {
          PriceGranularity::Low => {
              let capped = cpm.min(5.0);
              format!("{:.2}", (capped / 0.50).floor() * 0.50)
          }
          PriceGranularity::Medium => {
              let capped = cpm.min(20.0);
              format!("{:.2}", (capped / 0.10).floor() * 0.10)
          }
          PriceGranularity::High => {
              let capped = cpm.min(20.0);
              format!("{:.2}", (capped / 0.01).floor() * 0.01)
          }
          PriceGranularity::Dense | PriceGranularity::Auto => dense_bucket(cpm),
      }
  }

  fn dense_bucket(cpm: f64) -> String {
      if cpm >= 20.0 {
          return "20.00".to_string();
      }
      if cpm >= 8.0 {
          return format!("{:.2}", (cpm / 0.50).floor() * 0.50);
      }
      if cpm >= 3.0 {
          return format!("{:.2}", (cpm / 0.05).floor() * 0.05);
      }
      format!("{:.2}", (cpm / 0.01).floor() * 0.01)
  }
  ```

- [ ] **Step 3: Export from lib.rs**

  ```rust
  pub mod price_bucket;
  ```

- [ ] **Step 4: Run tests**

  Run: `cargo test -p trusted-server-core price_bucket`
  Expected: all tests pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/price_bucket.rs \
          crates/trusted-server-core/src/lib.rs
  git commit -m "Add Prebid price granularity bucketing (dense default, auto = dense)"
  ```

---

## Task 3: Extend `auction::types` — `MediaType::banner()` and `Bid::ad_id`

**Files:**

- Modify: `crates/trusted-server-core/src/auction/types.rs`

`CreativeOpportunityFormat` uses `#[serde(default = "MediaType::banner")]` which requires a free function. Add `ad_id: Option<String>` to `Bid` for `hb_adid` targeting.

- [ ] **Step 1: Write failing tests**

  In `auction/types.rs` test module:

  ```rust
  #[test]
  fn media_type_banner_fn_returns_banner() {
      assert_eq!(MediaType::banner(), MediaType::Banner);
  }

  #[test]
  fn bid_has_ad_id_field() {
      let bid = Bid {
          slot_id: "s".to_string(),
          price: Some(1.0),
          currency: "USD".to_string(),
          creative: None,
          adomain: None,
          bidder: "kargo".to_string(),
          width: 300,
          height: 250,
          nurl: None,
          burl: None,
          ad_id: Some("prebid-ad-id-abc".to_string()),
          metadata: Default::default(),
      };
      assert_eq!(bid.ad_id.as_deref(), Some("prebid-ad-id-abc"));
  }
  ```

  Run: `cargo test -p trusted-server-core auction::types::tests`
  Expected: compile error (`MediaType::banner` not found, `ad_id` field missing)

- [ ] **Step 2: Add `MediaType::banner()` and `Bid::ad_id`**

  In `auction/types.rs`, add to `MediaType`:

  ```rust
  impl MediaType {
      /// Returns `Banner` — used as a `#[serde(default = ...)]` fn pointer.
      pub fn banner() -> Self {
          Self::Banner
      }
  }
  ```

  Add `ad_id: Option<String>` field to `Bid`. Update the `make_bid` test helper to include `ad_id: None`.

- [ ] **Step 3: Run tests**

  Run: `cargo test -p trusted-server-core auction`
  Expected: all tests pass

- [ ] **Step 4: Update prebid.rs to populate `ad_id`**

  In `crates/trusted-server-core/src/integrations/prebid.rs`, in the `Bid` construction, find where `nurl` and `burl` are set and add:

  ```rust
  ad_id: bid_obj.get("adid")
      .or_else(|| bid_obj.get("id"))
      .and_then(|v| v.as_str())
      .map(String::from),
  ```

  (Prebid Server uses lowercase `adid` in bid objects, not `adId`. Fall back to `id` if absent.)

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/auction/types.rs \
          crates/trusted-server-core/src/integrations/prebid.rs
  git commit -m "Add MediaType::banner() constructor and Bid::ad_id for hb_adid targeting"
  ```

---

## Task 4: `creative_opportunities.rs` — Config types and URL matching

**Files:**

- Create: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

- [ ] **Step 1: Write failing tests**

  Create `crates/trusted-server-core/src/creative_opportunities.rs` with only tests:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      fn make_slot(id: &str, patterns: Vec<&str>) -> CreativeOpportunitySlot {
          CreativeOpportunitySlot {
              id: id.to_string(),
              gam_unit_path: None,
              div_id: None,
              page_patterns: patterns.into_iter().map(String::from).collect(),
              formats: vec![CreativeOpportunityFormat {
                  width: 300,
                  height: 250,
                  media_type: crate::auction::types::MediaType::Banner,
              }],
              floor_price: Some(0.50),
              targeting: Default::default(),
              providers: Default::default(),
          }
      }

      #[test]
      fn glob_matches_article_path() {
          let slot = make_slot("atf", vec!["/20**"]);
          assert!(slot.matches_path("/2024/01/my-article/"), "should match article path");
          assert!(!slot.matches_path("/"), "should not match root");
      }

      #[test]
      fn exact_match_homepage() {
          let slot = make_slot("home", vec!["/"]);
          assert!(slot.matches_path("/"), "should match root");
          assert!(!slot.matches_path("/about"), "should not match /about");
      }

      #[test]
      fn slot_id_validates_alphanumeric() {
          assert!(validate_slot_id("atf_sidebar_ad").is_ok());
          assert!(validate_slot_id("below-content-0").is_ok());
          assert!(validate_slot_id("").is_err(), "empty id should fail");
          assert!(validate_slot_id("xss<script>").is_err(), "html in id should fail");
          assert!(validate_slot_id("has space").is_err(), "spaces should fail");
      }

      #[test]
      fn resolved_gam_unit_path_uses_default_when_absent() {
          let slot = make_slot("atf", vec!["/"]);
          assert_eq!(
              slot.resolved_gam_unit_path("21765378893"),
              "/21765378893/atf"
          );
      }

      #[test]
      fn resolved_gam_unit_path_uses_override_when_set() {
          let mut slot = make_slot("atf", vec!["/"]);
          slot.gam_unit_path = Some("/21765378893/publisher/atf-sidebar".to_string());
          assert_eq!(
              slot.resolved_gam_unit_path("21765378893"),
              "/21765378893/publisher/atf-sidebar"
          );
      }

      #[test]
      fn resolved_div_id_defaults_to_slot_id() {
          let slot = make_slot("atf", vec!["/"]);
          assert_eq!(slot.resolved_div_id(), "atf");
      }

      #[test]
      fn to_ad_slot_wires_aps_params_into_bidders() {
          let mut slot = make_slot("atf", vec!["/"]);
          slot.providers.aps = Some(ApsSlotParams { slot_id: "aps-slot-atf".to_string() });
          let ad_slot = slot.to_ad_slot("21765378893");
          let aps_params = ad_slot.bidders.get("aps").expect("should have aps bidder");
          assert_eq!(
              aps_params.get("slotID").and_then(|v| v.as_str()),
              Some("aps-slot-atf"),
              "should wire APS slot ID into bidders"
          );
      }

      #[test]
      fn to_ad_slot_sets_floor_price_and_formats() {
          let slot = make_slot("atf", vec!["/"]);
          let ad_slot = slot.to_ad_slot("21765378893");
          assert_eq!(ad_slot.id, "atf");
          assert_eq!(ad_slot.floor_price, Some(0.50));
          assert_eq!(ad_slot.formats.len(), 1);
      }
  }
  ```

  Run: `cargo test -p trusted-server-core creative_opportunities`
  Expected: compile error (module not yet exported)

- [ ] **Step 2: Implement creative_opportunities.rs**

  ```rust
  //! Creative opportunities config — slot templates, URL matching, AdSlot conversion.

  use std::collections::HashMap;

  use glob::Pattern;
  use serde::{Deserialize, Serialize};

  use crate::auction::types::{AdFormat, AdSlot, MediaType};
  use crate::price_bucket::PriceGranularity;

  /// Top-level `[creative_opportunities]` block in `trusted-server.toml`.
  #[derive(Debug, Clone, Deserialize, Serialize)]
  pub struct CreativeOpportunitiesConfig {
      pub gam_network_id: String,
      #[serde(default)]
      pub auction_timeout_ms: Option<u32>,
      #[serde(default = "PriceGranularity::dense")]
      pub price_granularity: PriceGranularity,
  }

  /// One entry from `[[slot]]` in `creative-opportunities.toml`.
  #[derive(Debug, Clone, Deserialize)]
  pub struct CreativeOpportunitySlot {
      pub id: String,
      pub gam_unit_path: Option<String>,
      pub div_id: Option<String>,
      pub page_patterns: Vec<String>,
      pub formats: Vec<CreativeOpportunityFormat>,
      pub floor_price: Option<f64>,
      #[serde(default)]
      pub targeting: HashMap<String, String>,
      #[serde(default)]
      pub providers: SlotProviders,
  }

  impl CreativeOpportunitySlot {
      /// Returns true when `path` matches any of this slot's `page_patterns`.
      pub fn matches_path(&self, path: &str) -> bool {
          self.page_patterns.iter().any(|pattern| {
              Pattern::new(pattern)
                  .map(|p| p.matches(path))
                  .unwrap_or(false)
          })
      }

      /// Resolved GAM ad-unit path: override if set, else `/{gam_network_id}/{id}`.
      pub fn resolved_gam_unit_path(&self, gam_network_id: &str) -> String {
          self.gam_unit_path
              .clone()
              .unwrap_or_else(|| format!("/{}/{}", gam_network_id, self.id))
      }

      /// Resolved DOM div ID: override if set, else the slot `id`.
      pub fn resolved_div_id(&self) -> &str {
          self.div_id.as_deref().unwrap_or(&self.id)
      }

      /// Convert to an `AdSlot` for the orchestrator.
      pub fn to_ad_slot(&self, gam_network_id: &str) -> AdSlot {
          let mut bidders: HashMap<String, serde_json::Value> = HashMap::new();

          if let Some(ref aps) = self.providers.aps {
              bidders.insert(
                  "aps".to_string(),
                  serde_json::json!({ "slotID": aps.slot_id }),
              );
          }

          AdSlot {
              id: self.id.clone(),
              formats: self.formats.iter().map(CreativeOpportunityFormat::to_ad_format).collect(),
              floor_price: self.floor_price,
              targeting: self.targeting.iter()
                  .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                  .collect(),
              bidders,
          }
      }
  }

  /// Separate from `auction::AdFormat` — `media_type` defaults to `Banner`.
  #[derive(Debug, Clone, Deserialize)]
  pub struct CreativeOpportunityFormat {
      pub width: u32,
      pub height: u32,
      #[serde(default = "MediaType::banner")]
      pub media_type: MediaType,
  }

  impl CreativeOpportunityFormat {
      fn to_ad_format(&self) -> AdFormat {
          AdFormat {
              media_type: self.media_type.clone(),
              width: self.width,
              height: self.height,
          }
      }
  }

  #[derive(Debug, Clone, Default, Deserialize)]
  pub struct SlotProviders {
      pub aps: Option<ApsSlotParams>,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct ApsSlotParams {
      pub slot_id: String,
  }

  /// Top-level of `creative-opportunities.toml`.
  #[derive(Debug, Clone, Deserialize, Default)]
  pub struct CreativeOpportunitiesFile {
      #[serde(rename = "slot", default)]
      pub slots: Vec<CreativeOpportunitySlot>,
  }

  /// Validate a slot ID against the `[A-Za-z0-9_-]+` allowlist.
  pub fn validate_slot_id(id: &str) -> Result<(), String> {
      if id.is_empty() {
          return Err("slot id must not be empty".to_string());
      }
      if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
          Ok(())
      } else {
          Err(format!(
              "slot id '{id}' contains invalid characters; only [A-Za-z0-9_-] allowed"
          ))
      }
  }

  /// Find all slots matching the given URL path.
  pub fn match_slots<'a>(
      slots: &'a [CreativeOpportunitySlot],
      path: &str,
  ) -> Vec<&'a CreativeOpportunitySlot> {
      slots.iter().filter(|s| s.matches_path(path)).collect()
  }
  ```

- [ ] **Step 3: Export from lib.rs**

  ```rust
  pub mod creative_opportunities;
  ```

- [ ] **Step 4: Run tests**

  Run: `cargo test -p trusted-server-core creative_opportunities`
  Expected: all tests pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/creative_opportunities.rs \
          crates/trusted-server-core/src/lib.rs
  git commit -m "Add creative_opportunities config types, URL glob matching, APS params wiring"
  ```

---

## Task 5: Settings integration and `creative-opportunities.toml`

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs`
- Create: `creative-opportunities.toml`
- Modify: `trusted-server.toml`

- [ ] **Step 1: Write failing test**

  In `settings.rs` test module:

  ```rust
  #[test]
  fn settings_parses_creative_opportunities_section() {
      let toml = r#"
  [publisher]
  domain = "example.com"
  cookie_domain = ".example.com"
  origin_url = "https://origin.example.com"
  proxy_secret = "secret"

  [creative_opportunities]
  gam_network_id = "21765378893"
  auction_timeout_ms = 500
  "#;
      let settings = Settings::from_toml(toml).expect("should parse");
      let co = settings.creative_opportunities.expect("should have creative_opportunities");
      assert_eq!(co.gam_network_id, "21765378893");
      assert_eq!(co.auction_timeout_ms, Some(500));
  }
  ```

  Run: `cargo test -p trusted-server-core settings_parses_creative_opportunities`
  Expected: compile error (`Settings` has no field `creative_opportunities`)

- [ ] **Step 2: Add to `Settings`**

  In `settings.rs`, add import:

  ```rust
  use crate::creative_opportunities::CreativeOpportunitiesConfig;
  ```

  In the `Settings` struct:

  ```rust
  #[serde(default)]
  pub creative_opportunities: Option<CreativeOpportunitiesConfig>,
  ```

- [ ] **Step 3: Run test**

  Run: `cargo test -p trusted-server-core settings_parses_creative_opportunities`
  Expected: passes

- [ ] **Step 4: Create `creative-opportunities.toml`**

  At repo root alongside `trusted-server.toml`:

  ```toml
  # creative-opportunities.toml
  # Slot templates for server-side ad auction.
  # Empty file = feature disabled (no auction fired, no globals injected).

  [[slot]]
  id = "atf_sidebar_ad"
  gam_unit_path = "/21765378893/publisher/atf-sidebar"
  div_id = "div-atf-sidebar"
  page_patterns = ["/20**"]
  formats = [{ width = 300, height = 250 }]
  floor_price = 0.50

  [slot.targeting]
  pos = "atf"
  zone = "atfSidebar"

  [slot.providers.aps]
  slot_id = "aps-slot-atf-sidebar"
  ```

- [ ] **Step 5: Add `[creative_opportunities]` to `trusted-server.toml`**

  ```toml
  [creative_opportunities]
  gam_network_id = "21765378893"
  auction_timeout_ms = 500
  price_granularity = "dense"
  ```

- [ ] **Step 6: Run full workspace tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-core/src/settings.rs \
          creative-opportunities.toml \
          trusted-server.toml
  git commit -m "Wire CreativeOpportunitiesConfig into Settings; add creative-opportunities.toml"
  ```

---

## Task 6: Build-time slot-ID validation

**Files:**

- Modify: `crates/trusted-server-core/build.rs`

- [ ] **Step 1: Add slot-ID validation to `build.rs`**

  In `build.rs`, add after the existing settings validation:

  ```rust
  // Validate slot IDs at build time. Parse TOML inline — cannot import creative_opportunities.rs
  // because crate::auction::types is unavailable in the build context.
  const CREATIVE_OPPORTUNITIES_PATH: &str = "../../creative-opportunities.toml";

  println!("cargo:rerun-if-changed={}", CREATIVE_OPPORTUNITIES_PATH);

  let co_path = std::path::Path::new(CREATIVE_OPPORTUNITIES_PATH);
  if co_path.exists() {
      let co_content = std::fs::read_to_string(co_path)
          .expect("should read creative-opportunities.toml");
      let co_value: toml::Value = toml::from_str(&co_content)
          .expect("creative-opportunities.toml: invalid TOML");
      let slot_id_re = regex::Regex::new(r"^[A-Za-z0-9_\-]+$").expect("should compile");
      if let Some(slots) = co_value.get("slot").and_then(|v| v.as_array()) {
          for slot in slots {
              let id = slot.get("id")
                  .and_then(|v| v.as_str())
                  .expect("creative-opportunities.toml: slot missing 'id' field");
              if !slot_id_re.is_match(id) {
                  panic!(
                      "creative-opportunities.toml: slot id '{}' is invalid; \
                       only [A-Za-z0-9_-] allowed",
                      id
                  );
              }
          }
          println!(
              "cargo:warning=creative-opportunities.toml: {} slot(s) validated",
              slots.len()
          );
      }
  }
  ```

- [ ] **Step 2: Verify `regex` is in build-dependencies**

  Check `crates/trusted-server-core/Cargo.toml` `[build-dependencies]` — `regex` must be listed. Add if absent.

- [ ] **Step 3: Run build to verify**

  Run: `cargo build --package trusted-server-core`
  Expected: builds, warning `creative-opportunities.toml: 1 slot(s) validated`

- [ ] **Step 4: Test the guard**

  Temporarily add to `creative-opportunities.toml`:

  ```toml
  [[slot]]
  id = "bad slot!"
  page_patterns = ["/"]
  formats = []
  ```

  Run: `cargo build --package trusted-server-core`
  Expected: panics with invalid slot ID message. Revert the change.

- [ ] **Step 5: Add `rerun-if-changed` for the adapter crate**

  The adapter's `main.rs` uses `include_str!("../../../creative-opportunities.toml")` (Task 9 Step 7).
  Without a `rerun-if-changed` directive in the adapter's build script, Cargo will not rebuild
  when `creative-opportunities.toml` changes.

  Check whether `crates/trusted-server-adapter-fastly/build.rs` exists. If it does, add:

  ```rust
  println!("cargo:rerun-if-changed=../../../creative-opportunities.toml");
  ```

  If there is no `build.rs` in the adapter crate, create one containing only:

  ```rust
  fn main() {
      println!("cargo:rerun-if-changed=../../../creative-opportunities.toml");
  }
  ```

  Run: `cargo build --package trusted-server-adapter-fastly`
  Expected: clean build. Modify `creative-opportunities.toml` and re-run — adapter must recompile.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/build.rs \
          crates/trusted-server-adapter-fastly/build.rs
  git commit -m "Validate creative-opportunities.toml slot IDs at build time using inline TOML parse"
  ```

---

## Task 7: HTML processor — head injection of `__ts_ad_slots` and `__ts_request_id`

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`

> **Critical design constraint:** The spec explicitly rejects holding `</head>` to inject bids — this blocks body parsing and destroys FCP. Only `window.__ts_ad_slots` and `window.__ts_request_id` are injected, both at `<head>` open. Bid results are delivered via `/ts-bids` (Task 10). There is NO `on_end_tag` handler, NO `ad_bids_script` field.

- [ ] **Step 1: Write failing tests for injection**

  In `html_processor.rs` tests:

  ```rust
  #[test]
  fn injects_ad_slots_and_request_id_at_head_open() {
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: Some(
              r#"<script>window.__ts_ad_slots=JSON.parse("[]");window.__ts_request_id="test-rid-123";</script>"#
                  .to_string()
          ),
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head><title>T</title></head><body></body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("window.__ts_ad_slots"), "should inject ad slots at head-open");
      assert!(html.contains("window.__ts_request_id"), "should inject request_id at head-open");
  }

  #[test]
  fn does_not_hold_end_of_head() {
      // Verify: no bid data appears before </head> — that hold was rejected by spec §4.3
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: None,
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head><title>T</title></head><body></body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(!html.contains("__ts_bids"), "must not inject bids into head");
  }
  ```

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: compile error (no `ad_slots_script` field, no `empty_for_tests()`)

- [ ] **Step 2: Add `empty_for_tests()` to `IntegrationRegistry`**

  In `registry.rs`, add:

  ```rust
  #[cfg(test)]
  impl IntegrationRegistry {
      pub fn empty_for_tests() -> Self {
          Self {
              inner: Arc::new(RegistryInner {
                  proxies: Default::default(),
                  attribute_rewriters: Default::default(),
                  script_rewriters: Vec::new(),
                  html_post_processors: Vec::new(),
                  head_injectors: Vec::new(),
                  metadata: Default::default(),
              })
          }
      }
  }
  ```

  (Adjust field names to match the actual `RegistryInner` struct.)

- [ ] **Step 3: Add single field to `HtmlProcessorConfig`**

  Replace any existing `ad_slots_script`/`ad_bids_script` fields with:

  ```rust
  pub struct HtmlProcessorConfig {
      pub origin_host: String,
      pub request_host: String,
      pub request_scheme: String,
      pub integrations: IntegrationRegistry,
      /// Pre-computed `<script>window.__ts_ad_slots=...;window.__ts_request_id="...";</script>`.
      /// Injected at `<head>` open, before integration head inserts. `None` when no slots matched.
      pub ad_slots_script: Option<String>,
  }
  ```

  Update `from_settings` (or wherever `HtmlProcessorConfig` is constructed) to initialize `ad_slots_script: None`.

- [ ] **Step 4: Inject `ad_slots_script` at head-open**

  In `create_html_processor`, within the EXISTING `element!("head", ...)` handler, build the full snippet string with `ad_slots_script` first (so it appears first in output — lol_html `prepend` inserts before children, with **last-prepend-wins** ordering, so we call `prepend` exactly once with the full combined string):

  ```rust
  let ad_slots_script = config.ad_slots_script.clone();
  // ... existing captures ...

  element!("head", |el| {
      let mut snippet = String::new();

      // ad_slots_script first so __ts_ad_slots + __ts_request_id appear before
      // integration inserts. DO NOT call prepend multiple times — lol_html stacks
      // prepend calls in reverse order, so a single prepend with the full string
      // guarantees correct ordering.
      if let Some(ref slots_script) = ad_slots_script {
          snippet.push_str(slots_script);
      }

      // ... existing: for insert in integrations.head_inserts(&ctx) { snippet.push_str(...) }

      if !snippet.is_empty() {
          el.prepend(&snippet, ContentType::Html);
      }
      // DO NOT register on_end_tag — </head> flushes immediately per spec §4.3
      Ok(())
  })
  ```

- [ ] **Step 5: Run tests**

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: all tests pass (including the new ones; no bids injection test must also pass)

- [ ] **Step 6: Run full suite**

  Run: `cargo test --workspace`
  Expected: clean

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-core/src/html_processor.rs \
          crates/trusted-server-core/src/integrations/registry.rs
  git commit -m "Add ad_slots_script injection to HtmlProcessorConfig at head-open; no </head> hold"
  ```

---

## Task 8: `bid_cache.rs` — In-process auction result cache

**Files:**

- Create: `crates/trusted-server-core/src/bid_cache.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

The `BidCache` stores auction results keyed by `request_id` with a 30-second TTL. It is shared across concurrent Fastly request handlers via `std::sync::Mutex`. The `/ts-bids` endpoint (Task 10) uses `wait_for()` to block-poll until results arrive or the deadline fires.

> **WASM note:** `std::time::Instant` and `std::thread::sleep` are both supported in Viceroy and Fastly Compute. The Mutex is uncontested in practice — requests are handled cooperatively with brief lock windows.

- [ ] **Step 1: Write failing tests**

  Create `crates/trusted-server-core/src/bid_cache.rs` with only the tests:

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use std::time::{Duration, Instant};

      fn make_bids() -> BidMap {
          let mut m = std::collections::HashMap::new();
          m.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));
          m
      }

      #[test]
      fn returns_not_found_for_unknown_rid() {
          let cache = BidCache::new(Duration::from_secs(30), 100);
          let result = cache.try_get("unknown-rid");
          assert!(matches!(result, CacheResult::NotFound), "should return NotFound");
      }

      #[test]
      fn returns_pending_before_put() {
          let cache = BidCache::new(Duration::from_secs(30), 100);
          let deadline = Instant::now() + Duration::from_secs(5);
          cache.mark_pending("rid-1", deadline);
          let result = cache.try_get("rid-1");
          assert!(matches!(result, CacheResult::Pending), "should be Pending");
      }

      #[test]
      fn returns_bids_after_put() {
          let cache = BidCache::new(Duration::from_secs(30), 100);
          let deadline = Instant::now() + Duration::from_secs(5);
          cache.mark_pending("rid-2", deadline);
          cache.put("rid-2", make_bids());
          match cache.try_get("rid-2") {
              CacheResult::Complete(bids) => {
                  assert!(bids.contains_key("atf"), "should contain atf bid");
              }
              other => panic!("expected Complete, got {:?}", other),
          }
      }

      #[test]
      fn returns_not_found_for_expired_entry() {
          let cache = BidCache::new(Duration::from_millis(1), 100);
          let deadline = Instant::now() + Duration::from_secs(5);
          cache.mark_pending("rid-3", deadline);
          cache.put("rid-3", make_bids());
          std::thread::sleep(Duration::from_millis(5));
          let result = cache.try_get("rid-3");
          assert!(matches!(result, CacheResult::NotFound), "should expire after TTL");
      }

      #[test]
      fn wait_for_returns_bids_immediately_when_complete() {
          let cache = BidCache::new(Duration::from_secs(30), 100);
          let deadline = Instant::now() + Duration::from_secs(5);
          cache.mark_pending("rid-4", deadline);
          cache.put("rid-4", make_bids());
          let result = cache.wait_for("rid-4", deadline);
          assert!(matches!(result, WaitResult::Bids(_)), "should return bids immediately");
      }

      #[test]
      fn wait_for_returns_not_found_for_unknown_rid() {
          let cache = BidCache::new(Duration::from_secs(30), 100);
          let deadline = Instant::now() + Duration::from_millis(50);
          let result = cache.wait_for("never-registered", deadline);
          assert!(matches!(result, WaitResult::NotFound), "should return NotFound");
      }
  }
  ```

  Run: `cargo test -p trusted-server-core bid_cache`
  Expected: compile error (module not exported yet)

- [ ] **Step 2: Implement bid_cache.rs**

  ```rust
  //! In-process auction result cache keyed by request ID.
  //!
  //! Shared across concurrent Fastly request handlers via a global `Mutex`.
  //! Entries expire after a configurable TTL (30 seconds by default).

  use std::collections::HashMap;
  use std::sync::Mutex;
  use std::time::{Duration, Instant};

  pub type BidMap = HashMap<String, serde_json::Value>;

  #[derive(Debug)]
  enum EntryState {
      Pending { auction_deadline: Instant },
      Complete { bids: BidMap },
  }

  struct CacheEntry {
      state: EntryState,
      inserted_at: Instant,
  }

  struct BidCacheInner {
      entries: HashMap<String, CacheEntry>,
      insertion_order: std::collections::VecDeque<String>,
      capacity: usize,
      ttl: Duration,
  }

  impl BidCacheInner {
      fn evict_expired(&mut self) {
          let now = Instant::now();
          self.insertion_order.retain(|rid| {
              self.entries.get(rid)
                  .map(|e| now.duration_since(e.inserted_at) < self.ttl)
                  .unwrap_or(false)
          });
          self.entries.retain(|_, e| now.duration_since(e.inserted_at) < self.ttl);
      }

      fn evict_oldest_if_full(&mut self) {
          while self.entries.len() >= self.capacity {
              if let Some(oldest) = self.insertion_order.pop_front() {
                  self.entries.remove(&oldest);
              } else {
                  break;
              }
          }
      }
  }

  /// Outcome of a non-blocking cache lookup.
  #[derive(Debug)]
  pub enum CacheResult {
      /// Auction complete; bids are ready.
      Complete(BidMap),
      /// Auction registered but not yet complete.
      Pending,
      /// Request ID never registered, or TTL expired.
      NotFound,
  }

  /// Outcome of a blocking `wait_for` call.
  #[derive(Debug)]
  pub enum WaitResult {
      /// Auction completed within the deadline.
      Bids(BidMap),
      /// Deadline passed; bids not available.
      Empty,
      /// Request ID never registered (caller should return 404).
      NotFound,
  }

  /// In-process cache for auction results, shared across request handlers.
  pub struct BidCache {
      inner: Mutex<BidCacheInner>,
  }

  impl BidCache {
      /// Create a new `BidCache`.
      ///
      /// # Arguments
      /// - `ttl`: how long to keep entries before expiry
      /// - `capacity`: max number of concurrent entries (oldest evicted when full)
      pub fn new(ttl: Duration, capacity: usize) -> Self {
          Self {
              inner: Mutex::new(BidCacheInner {
                  entries: HashMap::new(),
                  insertion_order: std::collections::VecDeque::new(),
                  capacity,
                  ttl,
              }),
          }
      }

      /// Register a request as in-flight. Call at auction start, before `run_auction`.
      pub fn mark_pending(&self, request_id: &str, auction_deadline: Instant) {
          let mut inner = self.inner.lock().expect("should lock bid_cache");
          inner.evict_expired();
          inner.evict_oldest_if_full();
          inner.entries.insert(request_id.to_string(), CacheEntry {
              state: EntryState::Pending { auction_deadline },
              inserted_at: Instant::now(),
          });
          inner.insertion_order.push_back(request_id.to_string());
      }

      /// Store completed auction results. Transitions entry from Pending → Complete.
      pub fn put(&self, request_id: &str, bids: BidMap) {
          let mut inner = self.inner.lock().expect("should lock bid_cache");
          if let Some(entry) = inner.entries.get_mut(request_id) {
              entry.state = EntryState::Complete { bids };
          }
      }

      /// Non-blocking lookup. Returns current state without sleeping.
      pub fn try_get(&self, request_id: &str) -> CacheResult {
          let inner = self.inner.lock().expect("should lock bid_cache");
          let now = Instant::now();
          match inner.entries.get(request_id) {
              None => CacheResult::NotFound,
              Some(entry) if now.duration_since(entry.inserted_at) >= inner.ttl => {
                  CacheResult::NotFound
              }
              Some(entry) => match &entry.state {
                  EntryState::Pending { .. } => CacheResult::Pending,
                  EntryState::Complete { bids } => CacheResult::Complete(bids.clone()),
              },
          }
      }

      /// Return the stored auction deadline for a pending entry (the `T₀ + auction_timeout_ms`
      /// value minted when the page request arrived). Used by `/ts-bids` to enforce the correct
      /// deadline rather than minting a fresh `Instant::now() + timeout`.
      ///
      /// Returns `None` if the entry is unknown, expired, or already complete.
      pub fn get_auction_deadline(&self, request_id: &str) -> Option<Instant> {
          let inner = self.inner.lock().expect("should lock bid_cache");
          let now = Instant::now();
          inner.entries.get(request_id).and_then(|entry| {
              if now.duration_since(entry.inserted_at) >= inner.ttl {
                  return None;
              }
              match entry.state {
                  EntryState::Pending { auction_deadline } => Some(auction_deadline),
                  EntryState::Complete { .. } => None,
              }
          })
      }

      /// Block until bids are available for `request_id` or `deadline` passes.
      ///
      /// Polls every 50ms. Returns `NotFound` immediately if `request_id` was never registered.
      /// Returns `Empty` if deadline fires before auction completes.
      pub fn wait_for(&self, request_id: &str, deadline: Instant) -> WaitResult {
          loop {
              match self.try_get(request_id) {
                  CacheResult::Complete(bids) => return WaitResult::Bids(bids),
                  CacheResult::NotFound => return WaitResult::NotFound,
                  CacheResult::Pending => {
                      if Instant::now() >= deadline {
                          return WaitResult::Empty;
                      }
                      std::thread::sleep(Duration::from_millis(50));
                  }
              }
          }
      }
  }
  ```

- [ ] **Step 3: Export from lib.rs**

  ```rust
  pub mod bid_cache;
  ```

- [ ] **Step 4: Run tests**

  Run: `cargo test -p trusted-server-core bid_cache`
  Expected: all tests pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/bid_cache.rs \
          crates/trusted-server-core/src/lib.rs
  git commit -m "Add BidCache with 30s TTL, pending/complete states, and blocking wait_for"
  ```

---

## Task 9: `handle_publisher_request` async restructuring

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

> **Key constraint from spec §4.3:** Page rendering is never held for the auction. The auction and origin fetch run concurrently via Fastly's `send_async()` model — origin is dispatched first (non-blocking), then the auction runs its own `send_async` calls, so both overlap on the network. Bid results go to `bid_cache` only — they are NOT injected into the HTML. `Cache-Control: private, no-store` is set whenever slots matched (not just when bids arrived).

- [ ] **Step 1: Update function signature**

  Change `handle_publisher_request` in `publisher.rs`:

  ```rust
  pub async fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      services: &RuntimeServices,
      orchestrator: &crate::auction::orchestrator::AuctionOrchestrator,
      slots_file: &crate::creative_opportunities::CreativeOpportunitiesFile,
      bid_cache: &crate::bid_cache::BidCache,
      mut req: Request,
  ) -> Result<PublisherResponse, Report<TrustedServerError>>
  ```

  Add imports:

  ```rust
  use crate::auction::orchestrator::AuctionOrchestrator;
  use crate::auction::types::{AuctionContext, AuctionRequest, PublisherInfo, UserInfo, SiteInfo};
  use crate::bid_cache::{BidCache, BidMap};
  use crate::creative_opportunities::{CreativeOpportunitiesFile, match_slots};
  use crate::price_bucket::price_bucket;
  ```

- [ ] **Step 2: Mint `request_id`, match URL, check consent**

  At the top of the function body, before the origin fetch:

  ```rust
  // Mint per-request UUID — included in head injection and /ts-bids lookup key.
  let request_id = uuid::Uuid::new_v4().to_string();

  let request_path = req.get_path().to_string();
  let matched_slots: Vec<_> = if settings.creative_opportunities.is_some() {
      match_slots(&slots_file.slots, &request_path)
          .into_iter()
          .cloned()
          .collect()
  } else {
      Vec::new()
  };

  let consent_allows_auction = consent_context
      .tcf
      .as_ref()
      .map_or(false, |tcf| tcf.has_purpose_consent(1));
  let should_run_auction = !matched_slots.is_empty() && consent_allows_auction;

  let auction_timeout_ms = settings
      .creative_opportunities
      .as_ref()
      .and_then(|co| co.auction_timeout_ms)
      .unwrap_or(settings.auction.timeout_ms);
  ```

- [ ] **Step 3: Register pending in bid_cache, fire origin + auction concurrently**

  ```rust
  // Mint T₀ auction deadline. Stored in bid_cache so /ts-bids uses the same deadline,
  // not a freshly-minted one when the browser's fetch arrives.
  let auction_deadline = std::time::Instant::now()
      + std::time::Duration::from_millis(u64::from(auction_timeout_ms));

  // Register request as in-flight so /ts-bids can long-poll for it.
  if should_run_auction {
      bid_cache.mark_pending(&request_id, auction_deadline);
  }

  restrict_accept_encoding(&mut req);
  req.set_header("host", &origin_host);

  // Fire origin request immediately — Fastly's send_async dispatches the HTTP request
  // to the network without blocking. The origin fetch is in-flight from this point.
  // The auction below also uses send_async internally, so both origin SSP requests
  // overlap on the network. This is Fastly's concurrency model — no join! needed.
  let pending_origin = req
      .send_async(&backend_name)
      .change_context(TrustedServerError::Proxy {
          message: "Failed to dispatch async origin request".to_string(),
      })?;

  // Run auction (internal send_async calls overlap with origin fetch on the network).
  let auction_result = if should_run_auction {
      let co_config = settings.creative_opportunities.as_ref()
          .expect("should be present when should_run_auction is true");
      let auction_request = build_auction_request(
          &matched_slots,
          &ec_id,
          &consent_context,
          &request_info,
          co_config,
      );
      let placeholder_req = fastly::Request::new();
      let auction_context = AuctionContext {
          settings,
          request: &placeholder_req,
          client_info: &services.client_info,
          timeout_ms: auction_timeout_ms,
          provider_responses: None,
      };
      match orchestrator.run_auction(&auction_request, &auction_context, services).await {
          Ok(result) => Some(result),
          Err(e) => {
              log::warn!("server-side auction failed, proceeding without bids: {e:?}");
              None
          }
      }
  } else {
      None
  };

  // Write auction results to bid_cache — /ts-bids will serve them.
  if should_run_auction {
      let co_config = settings.creative_opportunities.as_ref()
          .expect("should be present");
      // Bind empty map to a local to avoid &Default::default() referencing a temporary.
      let empty_bids = std::collections::HashMap::new();
      let winning_bids = auction_result.as_ref()
          .map(|r| &r.winning_bids)
          .unwrap_or(&empty_bids);
      let bid_map = build_bid_map(winning_bids, co_config.price_granularity);
      bid_cache.put(&request_id, bid_map);
  }

  // Await origin response (may already be buffered since we started it before the auction).
  let mut response = pending_origin
      .wait()
      .change_context(TrustedServerError::Proxy {
          message: "Failed to await origin response".to_string(),
      })?;
  ```

- [ ] **Step 4: Build head injection script, set cache headers, force chunked encoding**

  After acquiring `response`:

  ```rust
  // Build head injection script: __ts_ad_slots + __ts_request_id (never bids).
  let ad_slots_script = if let Some(co_config) = &settings.creative_opportunities {
      if !matched_slots.is_empty() {
          Some(build_head_globals_script(&matched_slots, &request_id, co_config))
      } else {
          None
      }
  } else {
      None
  };

  // When slots matched: prevent browser/CDN caching of the per-user assembled HTML.
  // Spec §4.4: set regardless of whether bids arrived — the request_id is now in the page.
  if ad_slots_script.is_some() {
      response.set_header(header::CACHE_CONTROL, "private, no-store");
      response.remove_header("surrogate-control");
      response.remove_header("fastly-surrogate-control");
  }

  // Spec §4.3/§4.7: Force chunked encoding on every origin response so that </head>
  // reaches the browser immediately as chunks arrive — regardless of whether origin
  // sent a buffered response (WordPress, Drupal) or a streaming one (NextJS 16).
  // Removing Content-Length is required; sending both headers is invalid HTTP/1.1.
  response.remove_header(header::CONTENT_LENGTH);
  response.set_header("transfer-encoding", "chunked");
  ```

- [ ] **Step 5: Add `pub(crate)` helper functions**

  ```rust
  /// Build the `<script>` block injected at `<head>` open.
  ///
  /// Contains both `window.__ts_ad_slots` (slot config from creative-opportunities.toml)
  /// and `window.__ts_request_id` (the per-request UUID for /ts-bids lookup).
  /// Neither bids nor auction results are injected here — they arrive via /ts-bids.
  pub(crate) fn build_head_globals_script(
      matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
      request_id: &str,
      co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
  ) -> String {
      let slots_json: Vec<_> = matched_slots.iter().map(|slot| {
          serde_json::json!({
              "id": slot.id,
              "gam_unit_path": slot.resolved_gam_unit_path(&co_config.gam_network_id),
              "div_id": slot.resolved_div_id(),
              "formats": slot.formats.iter()
                  .map(|f| serde_json::json!([f.width, f.height]))
                  .collect::<Vec<_>>(),
              "targeting": slot.targeting,
          })
      }).collect();
      let slots_json_str = serde_json::to_string(&slots_json)
          .expect("should serialize ad slots");
      let escaped_slots = html_escape_for_script(&slots_json_str);
      // request_id is a UUID (hex + hyphens only) — safe to embed without escaping.
      format!(
          r#"<script>window.__ts_ad_slots=JSON.parse("{escaped_slots}");window.__ts_request_id="{request_id}";</script>"#
      )
  }

  /// Build the `BidMap` stored in `bid_cache` and returned by `/ts-bids`.
  ///
  /// Keyed by slot ID. Values contain `hb_pb`, `hb_bidder`, `hb_adid`, `burl`.
  pub(crate) fn build_bid_map(
      winning_bids: &std::collections::HashMap<String, crate::auction::types::Bid>,
      price_granularity: crate::price_bucket::PriceGranularity,
  ) -> crate::bid_cache::BidMap {
      winning_bids
          .iter()
          .filter_map(|(slot_id, bid)| {
              let cpm = bid.price?;
              let entry: std::collections::HashMap<String, serde_json::Value> = [
                  ("hb_pb".to_string(), serde_json::Value::String(price_bucket(cpm, price_granularity))),
                  ("hb_bidder".to_string(), serde_json::Value::String(bid.bidder.clone())),
                  ("hb_adid".to_string(), serde_json::Value::String(
                      bid.ad_id.as_deref().unwrap_or("").to_string()
                  )),
                  ("burl".to_string(), bid.burl.as_deref()
                      .map(serde_json::Value::from)
                      .unwrap_or(serde_json::Value::Null)),
              ].into_iter().collect();
              Some((slot_id.clone(), entry.into_iter()
                  .map(|(k, v)| (k, v))
                  .collect::<serde_json::Map<_, _>>()
                  .into()))
          })
          .collect()
  }

  /// HTML-escape a JSON string for safe inline `<script>` injection.
  ///
  /// JSON is embedded in a double-quoted JS string literal (via `JSON.parse`).
  /// Unicode-escapes `<`, `>`, `&`, U+2028, U+2029 to prevent script injection.
  fn html_escape_for_script(json: &str) -> String {
      json.replace('&', "\\u0026")
          .replace('<', "\\u003c")
          .replace('>', "\\u003e")
          .replace('\u{2028}', "\\u2028")
          .replace('\u{2029}', "\\u2029")
  }

  fn build_auction_request(
      matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
      ec_id: &str,
      consent_context: &crate::consent::ConsentContext,
      request_info: &crate::http_util::RequestInfo,
      co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
  ) -> AuctionRequest {
      AuctionRequest {
          id: uuid::Uuid::new_v4().to_string(),
          slots: matched_slots.iter()
              .map(|s| s.to_ad_slot(&co_config.gam_network_id))
              .collect(),
          publisher: PublisherInfo {
              domain: request_info.host.clone(),
              page_url: Some(format!("{}://{}", request_info.scheme, request_info.host)),
          },
          user: UserInfo {
              id: ec_id.to_string(),
              fresh_id: uuid::Uuid::new_v4().to_string(),
              consent: Some(consent_context.clone()),
          },
          device: None,
          site: Some(SiteInfo {
              domain: request_info.host.clone(),
              page: request_info.host.clone(),
          }),
          context: Default::default(),
      }
  }
  ```

- [ ] **Step 6: Thread `ad_slots_script` into `OwnedProcessResponseParams`**

  Update `OwnedProcessResponseParams`:

  ```rust
  pub struct OwnedProcessResponseParams {
      // existing fields...
      pub(crate) ad_slots_script: Option<String>,
  }
  ```

  Pass `ad_slots_script` through to `create_html_stream_processor` and into `HtmlProcessorConfig`.

- [ ] **Step 7: Update `main.rs` call site**

  In `crates/trusted-server-adapter-fastly/src/main.rs`:

  ```rust
  // At startup — load creative-opportunities.toml and initialize bid_cache.
  const CREATIVE_OPPORTUNITIES_TOML: &str =
      include_str!("../../../creative-opportunities.toml");

  let slots_file: creative_opportunities::CreativeOpportunitiesFile =
      toml::from_str(CREATIVE_OPPORTUNITIES_TOML)
          .expect("should parse creative-opportunities.toml");

  // BidCache: 30s TTL, capacity 1000 entries (each entry is a few KB).
  let bid_cache = crate::bid_cache::BidCache::new(
      std::time::Duration::from_secs(30),
      1000,
  );
  ```

  Update the call to `handle_publisher_request`:

  ```rust
  match handle_publisher_request(
      settings,
      integration_registry,
      &publisher_services,
      orchestrator,    // existing
      &slots_file,     // new
      &bid_cache,      // new
      req,
  ).await {
      // existing match arms unchanged
  }
  ```

- [ ] **Step 8: Compile check**

  Run: `cargo check --workspace`
  Expected: clean compile

- [ ] **Step 9: Run full tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 10: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs \
          crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Convert handle_publisher_request to async; auction writes to bid_cache; inject head globals only"
  ```

---

## Task 10: `/ts-bids` endpoint

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

The `/ts-bids` endpoint is the client's fetch target for bid results. It long-polls until the auction completes or the deadline fires, then returns JSON. Bid results were already stored in `bid_cache` by Task 9.

- [ ] **Step 1: Write failing test (integration-style)**

  In `main.rs` test module (or a new `tests/ts_bids.rs`):

  ```rust
  #[test]
  fn ts_bids_response_structure() {
      use crate::bid_cache::{BidCache, WaitResult};
      use std::time::{Duration, Instant};

      let cache = BidCache::new(Duration::from_secs(30), 100);
      let rid = "test-rid-abc";
      let deadline = Instant::now() + Duration::from_secs(5);
      cache.mark_pending(rid, deadline);
      let mut bids = std::collections::HashMap::new();
      bids.insert("atf".to_string(), serde_json::json!({
          "hb_pb": "1.00", "hb_bidder": "kargo", "hb_adid": "abc", "burl": null,
      }));
      cache.put(rid, bids);

      match cache.wait_for(rid, deadline) {
          WaitResult::Bids(b) => {
              assert!(b.contains_key("atf"), "should contain atf slot bids");
          }
          other => panic!("expected Bids, got {:?}", other),
      }
  }
  ```

  Run: `cargo test -p trusted-server-adapter-fastly ts_bids`
  Expected: compile error (no handler yet, or pass since it's testing bid_cache directly)

- [ ] **Step 2: Add `/ts-bids` route handler in `main.rs`**

  In the request routing section, before the publisher fallback, add:

  ```rust
  if req.get_path() == "/ts-bids" && req.get_method() == fastly::http::Method::GET {
      return handle_ts_bids_request(req, &bid_cache, settings);
  }
  ```

  Add the handler function:

  ```rust
  fn handle_ts_bids_request(
      req: fastly::Request,
      bid_cache: &crate::bid_cache::BidCache,
      settings: &Settings,
  ) -> fastly::Response {
      // Parse `rid` query param.
      let rid = req.get_query_parameter("rid").map(String::from);
      let rid = match rid {
          Some(r) if !r.is_empty() => r,
          _ => {
              return fastly::Response::from_status(fastly::http::StatusCode::BAD_REQUEST)
                  .with_body_text_plain("missing rid parameter");
          }
      };

      // Use the stored T₀ auction deadline from bid_cache — not a freshly-minted
      // Instant::now() + timeout, which would extend the window past the original A_deadline.
      // Spec §4.4: "/ts-bids blocks until auction completion or A_deadline" where A_deadline
      // = T₀ + auction_timeout_ms (minted at page request receipt, stored in bid_cache entry).
      let deadline = bid_cache.get_auction_deadline(&rid)
          .unwrap_or_else(|| {
              // Fallback: rid is unknown or already complete. wait_for returns immediately.
              std::time::Instant::now()
          });

      let result = bid_cache.wait_for(&rid, deadline);

      match result {
          crate::bid_cache::WaitResult::Bids(bids) => {
              let body = serde_json::to_string(&bids)
                  .unwrap_or_else(|_| "{}".to_string());
              fastly::Response::from_status(fastly::http::StatusCode::OK)
                  .with_header(fastly::http::header::CONTENT_TYPE, "application/json")
                  .with_header(fastly::http::header::CACHE_CONTROL, "private, no-store")
                  .with_body(body)
          }
          crate::bid_cache::WaitResult::Empty => {
              fastly::Response::from_status(fastly::http::StatusCode::OK)
                  .with_header(fastly::http::header::CONTENT_TYPE, "application/json")
                  .with_header(fastly::http::header::CACHE_CONTROL, "private, no-store")
                  .with_body("{}")
          }
          crate::bid_cache::WaitResult::NotFound => {
              fastly::Response::from_status(fastly::http::StatusCode::NOT_FOUND)
                  .with_header(fastly::http::header::CACHE_CONTROL, "private, no-store")
                  .with_body_text_plain("unknown request id")
          }
      }
  }
  ```

- [ ] **Step 3: Compile check**

  Run: `cargo check --workspace`
  Expected: clean

- [ ] **Step 4: Run tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Add /ts-bids endpoint with long-poll semantics; serves bid_cache results by request_id"
  ```

---

## Task 11: GPT head injector — emit `__tsAdInit` with `/ts-bids` fetch

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`

> **Critical:** The `__tsAdInit` function MUST fetch `/ts-bids?rid=<request_id>` — it must NOT read from `window.__ts_bids` (which is never set). The `window.__ts_request_id` global (injected at head-open by Task 9) supplies the RID.

- [ ] **Step 1: Write failing test**

  ```rust
  #[test]
  fn head_inserts_includes_ts_ad_init_with_ts_bids_fetch() {
      let config = test_config();
      let integration = GptIntegration::new(config);
      let ctx = make_test_context();
      let inserts = integration.head_inserts(&ctx);
      let combined = inserts.join("");
      assert!(combined.contains("__tsAdInit"), "should define __tsAdInit");
      assert!(combined.contains("/ts-bids"), "should fetch from /ts-bids endpoint");
      assert!(combined.contains("__ts_request_id"), "should use __ts_request_id for rid");
      assert!(combined.contains("bidsPromise"), "should use bidsPromise pattern");
      assert!(combined.contains("slotRenderEnded"), "should register slotRenderEnded");
      assert!(combined.contains("sendBeacon"), "should fire burl via sendBeacon");
      assert!(!combined.contains("__ts_bids"), "must NOT read window.__ts_bids — bids come from /ts-bids fetch");
  }
  ```

  Run: `cargo test -p trusted-server-core integrations::gpt`
  Expected: FAIL — `__tsAdInit` not defined / assertion on `/ts-bids` string fails if old version present

- [ ] **Step 2: Replace `head_inserts()` in gpt.rs**

  ```rust
  impl IntegrationHeadInjector for GptIntegration {
      fn integration_id(&self) -> &'static str {
          GPT_INTEGRATION_ID
      }

      fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
          vec![
              "<script>window.__tsjs_gpt_enabled=true;\
               window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>"
                  .to_string(),
              // __tsAdInit: fetches /ts-bids for bid targeting, then drives GPT.
              // window.__ts_ad_slots and window.__ts_request_id are injected at head-open by TS.
              // bidsPromise resolves concurrently with page rendering — never blocks FCP.
              concat!(
                  "<script>",
                  "window.__tsAdInit=function(){",
                    "var slots=window.__ts_ad_slots||[];",
                    "var rid=window.__ts_request_id;",
                    "var bidsPromise=rid",
                      "?fetch('/ts-bids?rid='+encodeURIComponent(rid),{credentials:'omit'})",
                          ".then(function(r){return r.ok?r.json():{};}).catch(function(){return{};})",
                      ":Promise.resolve({});",
                    "googletag.cmd.push(function(){",
                      "var gptSlots=slots.map(function(slot){",
                        "var s=googletag.defineSlot(slot.gam_unit_path,slot.formats,slot.div_id);",
                        "if(!s)return null;",
                        "s.addService(googletag.pubads());",
                        "Object.entries(slot.targeting||{}).forEach(function(e){s.setTargeting(e[0],e[1]);});",
                        "return{id:slot.id,gptSlot:s};",
                      "}).filter(Boolean);",
                      "googletag.pubads().enableSingleRequest();",
                      "googletag.enableServices();",
                      "bidsPromise.then(function(bids){",
                        "gptSlots.forEach(function(entry){",
                          "var b=bids[entry.id]||{};",
                          "[\"hb_pb\",\"hb_bidder\",\"hb_adid\"].forEach(function(k){if(b[k])entry.gptSlot.setTargeting(k,b[k]);});",
                        "});",
                        "googletag.pubads().addEventListener(\"slotRenderEnded\",function(ev){",
                          "var id=ev.slot.getSlotElementId();",
                          "var b=bids[id]||{};",
                          "if(!ev.isEmpty&&b.burl&&ev.slot.getTargeting(\"hb_adid\")[0]===b.hb_adid){",
                            "navigator.sendBeacon(b.burl);",
                          "}",
                        "});",
                        "googletag.pubads().refresh();",
                      "});",
                    "});",
                  "};",
                  "</script>"
              ).to_string(),
          ]
      }
  }
  ```

- [ ] **Step 3: Run tests**

  Run: `cargo test -p trusted-server-core integrations::gpt`
  Expected: all pass including new test

- [ ] **Step 4: Commit**

  ```bash
  git add crates/trusted-server-core/src/integrations/gpt.rs
  git commit -m "Emit __tsAdInit with /ts-bids fetch pattern from GPT head injector"
  ```

---

## Task 12: `gpt/index.ts` — TypeScript `__tsAdInit` with `/ts-bids` fetch

**Files:**

- Modify: `crates/js/lib/src/integrations/gpt/index.ts`

The TypeScript version mirrors the Rust inline string from Task 11. It uses the `bidsPromise` pattern — fetching `/ts-bids` concurrently with GPT slot definition.

- [ ] **Step 1: Write failing tests**

  In `crates/js/lib/src/integrations/gpt/index.test.ts`:

  ```typescript
  import { describe, it, expect, vi, beforeEach } from 'vitest'

  describe('installTsAdInit', () => {
    beforeEach(() => {
      delete (window as any).__ts_ad_slots
      delete (window as any).__ts_request_id
      delete (window as any).__tsAdInit
    })

    it('fetches /ts-bids with request_id and applies bid targeting before refresh', async () => {
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue('atf'),
        getTargeting: vi.fn().mockReturnValue([]),
      }
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
      }
      ;(window as any).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: vi.fn().mockReturnValue(mockSlot),
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      }
      ;(window as any).__ts_ad_slots = [
        {
          id: 'atf',
          gam_unit_path: '/123/atf',
          div_id: 'atf',
          formats: [[300, 250]],
          targeting: { pos: 'atf' },
        },
      ]
      ;(window as any).__ts_request_id = 'test-rid-123'

      const fetchSpy = vi.spyOn(global, 'fetch').mockResolvedValue({
        ok: true,
        json: async () => ({
          atf: {
            hb_pb: '1.00',
            hb_bidder: 'kargo',
            hb_adid: 'abc',
            burl: 'https://ssp/bill',
          },
        }),
      } as Response)

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      await (window as any).__tsAdInit()

      expect(fetchSpy).toHaveBeenCalledWith(
        expect.stringContaining('/ts-bids?rid=test-rid-123'),
        expect.objectContaining({ credentials: 'omit' })
      )
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00')
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo')
      expect(mockPubads.refresh).toHaveBeenCalled()

      fetchSpy.mockRestore()
    })

    it('calls refresh with empty bids when fetch fails', async () => {
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
      }
      ;(window as any).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: vi.fn().mockReturnValue({
          addService: vi.fn().mockReturnThis(),
          setTargeting: vi.fn().mockReturnThis(),
        }),
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      }
      ;(window as any).__ts_ad_slots = []
      ;(window as any).__ts_request_id = 'rid-fail'

      vi.spyOn(global, 'fetch').mockRejectedValue(new Error('network error'))

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      await (window as any).__tsAdInit()

      expect(mockPubads.refresh).toHaveBeenCalled()
    })

    it('fires burl via sendBeacon on slotRenderEnded when our bid won', async () => {
      const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      let capturedListener: ((e: any) => void) | undefined

      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue('atf'),
        getTargeting: vi.fn().mockReturnValue(['abc']),
      }
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        refresh: vi.fn(),
        addEventListener: vi.fn((event: string, fn: (e: any) => void) => {
          if (event === 'slotRenderEnded') capturedListener = fn
        }),
      }
      ;(window as any).googletag = {
        cmd: { push: vi.fn((fn: () => void) => fn()) },
        defineSlot: vi.fn().mockReturnValue(mockSlot),
        pubads: vi.fn().mockReturnValue(mockPubads),
        enableServices: vi.fn(),
      }
      ;(window as any).__ts_ad_slots = [
        {
          id: 'atf',
          gam_unit_path: '/123/atf',
          div_id: 'atf',
          formats: [[300, 250]],
          targeting: {},
        },
      ]
      ;(window as any).__ts_request_id = 'rid-burl-test'

      vi.spyOn(global, 'fetch').mockResolvedValue({
        ok: true,
        json: async () => ({
          atf: {
            hb_pb: '1.00',
            hb_bidder: 'kargo',
            hb_adid: 'abc',
            burl: 'https://ssp/bill',
          },
        }),
      } as Response)

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      await (window as any).__tsAdInit()

      // Trigger slotRenderEnded — slot has our winning hb_adid
      expect(capturedListener).toBeDefined()
      capturedListener!({
        isEmpty: false,
        slot: mockSlot,
      })

      expect(beaconSpy).toHaveBeenCalledWith('https://ssp/bill')
      beaconSpy.mockRestore()
    })
  })
  ```

  Run: `cd crates/js/lib && npx vitest run`
  Expected: FAIL — `installTsAdInit` not exported or fetches wrong endpoint

- [ ] **Step 2: Add `installTsAdInit` to `index.ts`**

  Add to `crates/js/lib/src/integrations/gpt/index.ts`:

  ```typescript
  interface TsAdSlot {
    id: string
    gam_unit_path: string
    div_id: string
    formats: Array<number[]>
    targeting: Record<string, string>
  }

  interface TsBidData {
    hb_pb?: string
    hb_bidder?: string
    hb_adid?: string
    burl?: string
  }

  type TsWindow = Window & {
    __ts_ad_slots?: TsAdSlot[]
    __ts_request_id?: string
    __tsAdInit?: () => void
  }

  /**
   * Install `window.__tsAdInit`.
   *
   * Reads `window.__ts_ad_slots` and `window.__ts_request_id` (both injected by
   * the edge at `<head>` open). Fetches bid results from `/ts-bids?rid=<request_id>`
   * concurrently with GPT slot definition. Applies targeting and calls `refresh()`
   * after the fetch resolves. Registers `slotRenderEnded` to fire `burl` via
   * `sendBeacon` when our specific Prebid bid wins the GAM line item match.
   */
  export function installTsAdInit(): void {
    const w = window as TsWindow
    w.__tsAdInit = function () {
      const slots = w.__ts_ad_slots ?? []
      const rid = w.__ts_request_id

      const bidsPromise: Promise<Record<string, TsBidData>> = rid
        ? fetch(`/ts-bids?rid=${encodeURIComponent(rid)}`, {
            credentials: 'omit',
          })
            .then((r) => (r.ok ? r.json() : {}))
            .catch(() => ({}))
        : Promise.resolve({})

      const g = (window as GptWindow).googletag
      if (!g) return

      g.cmd.push(() => {
        const gptSlots = slots
          .map((slot) => {
            const gptSlot = g.defineSlot?.(
              slot.gam_unit_path,
              slot.formats,
              slot.div_id
            )
            if (!gptSlot) return null
            gptSlot.addService(g.pubads())
            Object.entries(slot.targeting ?? {}).forEach(([k, v]) =>
              gptSlot.setTargeting(k, v)
            )
            return { id: slot.id, gptSlot }
          })
          .filter(Boolean) as Array<{
          id: string
          gptSlot: NonNullable<ReturnType<typeof g.defineSlot>>
        }>

        g.pubads().enableSingleRequest()
        g.enableServices()

        bidsPromise.then((bids) => {
          gptSlots.forEach(({ id, gptSlot }) => {
            const bid = bids[id] ?? {}
            ;(['hb_pb', 'hb_bidder', 'hb_adid'] as const).forEach((key) => {
              if (bid[key]) gptSlot.setTargeting(key, bid[key]!)
            })
          })

          g.pubads().addEventListener?.('slotRenderEnded', (event: any) => {
            const slotId: string = event.slot?.getSlotElementId?.() ?? ''
            const bid = bids[slotId] ?? {}
            if (
              !event.isEmpty &&
              bid.burl &&
              event.slot?.getTargeting?.('hb_adid')?.[0] === bid.hb_adid
            ) {
              navigator.sendBeacon(bid.burl)
            }
          })

          g.pubads().refresh()
        })
      })
    }
  }
  ```

  Call `installTsAdInit()` from the integration's initialization path.

- [ ] **Step 3: Run JS tests**

  Run: `cd crates/js/lib && npx vitest run`
  Expected: new tests pass

- [ ] **Step 4: Build JS bundle**

  Run: `cd crates/js/lib && node build-all.mjs`
  Expected: clean build

- [ ] **Step 5: Commit**

  ```bash
  git add crates/js/lib/src/integrations/gpt/
  git commit -m "Add installTsAdInit with /ts-bids fetch pattern and slotRenderEnded burl firing"
  ```

---

## Task 13: `nurl` fire-and-forget

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write failing test**

  ```rust
  #[test]
  fn prebid_config_fire_nurl_defaults_to_true() {
      let config = PrebidConfig::default();
      assert!(config.fire_nurl_at_edge, "should fire nurl at edge by default");
  }
  ```

  Run: `cargo test -p trusted-server-core integrations::prebid`
  Expected: FAIL

- [ ] **Step 2: Add `fire_nurl_at_edge` to `PrebidConfig`**

  ```rust
  #[serde(default = "default_fire_nurl_at_edge")]
  pub fire_nurl_at_edge: bool,
  ```

  ```rust
  fn default_fire_nurl_at_edge() -> bool { true }
  ```

- [ ] **Step 3: Fire nurls in publisher.rs after bid_cache.put()**

  After the `bid_cache.put(...)` call (Task 9 Step 3), add:

  ```rust
  if let Some(ref result) = auction_result {
      fire_winning_nurls(result, settings);
  }
  ```

  Add helper:

  ```rust
  fn fire_winning_nurls(
      result: &crate::auction::orchestrator::OrchestrationResult,
      settings: &Settings,
  ) {
      use crate::backend::BackendConfig;

      let fire_nurl = settings
          .integrations
          .get_typed::<crate::integrations::prebid::PrebidConfig>("prebid")
          .map(|c| c.fire_nurl_at_edge)
          .unwrap_or(true);

      if !fire_nurl {
          return;
      }

      for bid in result.winning_bids.values() {
          let Some(ref nurl) = bid.nurl else { continue };
          let backend_name = match BackendConfig::from_url(nurl, false) {
              Ok(name) => name,
              Err(e) => {
                  log::warn!("nurl: cannot create backend for {nurl}: {e:?}");
                  continue;
              }
          };
          match fastly::Request::get(nurl).send_async(&backend_name) {
              Ok(_) => log::debug!("nurl: fired for slot {}", bid.slot_id),
              Err(e) => log::warn!("nurl: failed for slot {}: {e}", bid.slot_id),
          }
      }
  }
  ```

- [ ] **Step 4: Run tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/integrations/prebid.rs \
          crates/trusted-server-core/src/publisher.rs
  git commit -m "Fire winning bid nurl fire-and-forget from edge; add fire_nurl_at_edge config"
  ```

---

## Task 14: End-to-end integration tests

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (test module)

Tests use `pub(crate)` helpers from Task 9 directly.

- [ ] **Step 1: Write tests**

  In `publisher.rs` test module:

  ```rust
  #[cfg(test)]
  mod creative_opportunities_tests {
      use super::{build_head_globals_script, build_bid_map, html_escape_for_script};
      use crate::creative_opportunities::{
          CreativeOpportunitiesConfig, CreativeOpportunitySlot, CreativeOpportunityFormat,
          CreativeOpportunitiesFile, match_slots,
      };
      use crate::auction::types::{Bid, MediaType};
      use crate::price_bucket::PriceGranularity;
      use std::collections::HashMap;

      fn make_config() -> CreativeOpportunitiesConfig {
          CreativeOpportunitiesConfig {
              gam_network_id: "21765378893".to_string(),
              auction_timeout_ms: Some(500),
              price_granularity: PriceGranularity::Dense,
          }
      }

      fn make_slot() -> CreativeOpportunitySlot {
          CreativeOpportunitySlot {
              id: "atf_sidebar_ad".to_string(),
              gam_unit_path: Some("/21765378893/publisher/atf-sidebar".to_string()),
              div_id: Some("div-atf-sidebar".to_string()),
              page_patterns: vec!["/20**".to_string()],
              formats: vec![CreativeOpportunityFormat {
                  width: 300, height: 250, media_type: MediaType::Banner,
              }],
              floor_price: Some(0.50),
              targeting: [("pos".to_string(), "atf".to_string())].into_iter().collect(),
              providers: Default::default(),
          }
      }

      #[test]
      fn head_globals_script_contains_ad_slots_and_request_id() {
          let slots = vec![make_slot()];
          let config = make_config();
          let rid = "550e8400-e29b-41d4-a716-446655440000";
          let script = build_head_globals_script(&slots, rid, &config);
          assert!(script.contains("window.__ts_ad_slots=JSON.parse"), "should use JSON.parse for slots");
          assert!(script.contains("atf_sidebar_ad"), "should include slot id");
          assert!(script.contains(&format!("window.__ts_request_id=\"{rid}\"")), "should include request_id");
          assert!(!script.contains("__ts_bids"), "must NOT contain bids — bids come from /ts-bids");
      }

      #[test]
      fn head_globals_script_is_xss_safe() {
          let slots = vec![make_slot()];
          let config = make_config();
          let script = build_head_globals_script(&slots, "safe-rid", &config);
          // Strip outer <script> tags to inspect the content
          let inner = script
              .trim_start_matches("<script>")
              .trim_end_matches("</script>");
          assert!(!inner.contains('<'), "no unescaped < in script content");
          assert!(!inner.contains('>'), "no unescaped > in script content");
      }

      #[test]
      fn bid_map_uses_price_bucket_and_ad_id() {
          let mut winning_bids = HashMap::new();
          winning_bids.insert("atf_sidebar_ad".to_string(), Bid {
              slot_id: "atf_sidebar_ad".to_string(),
              price: Some(2.53),
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "kargo".to_string(),
              width: 300, height: 250,
              nurl: None,
              burl: Some("https://ssp.example/billing?id=abc123".to_string()),
              ad_id: Some("prebid-uuid-abc123".to_string()),
              metadata: HashMap::new(),
          });
          let bid_map = build_bid_map(&winning_bids, PriceGranularity::Dense);
          let slot_bids = bid_map.get("atf_sidebar_ad").expect("should have slot bids");
          assert_eq!(
              slot_bids.get("hb_pb").and_then(|v| v.as_str()),
              Some("2.53"),
              "should bucket 2.53 as 2.53 (dense)"
          );
          assert_eq!(
              slot_bids.get("hb_bidder").and_then(|v| v.as_str()),
              Some("kargo"),
              "should include bidder"
          );
          assert_eq!(
              slot_bids.get("hb_adid").and_then(|v| v.as_str()),
              Some("prebid-uuid-abc123"),
              "should use ad_id not creative markup"
          );
      }

      #[test]
      fn html_escape_neutralizes_xss_in_json() {
          let malicious = r#"{"zone":"</script><script>alert(1)//"}"#;
          let escaped = html_escape_for_script(malicious);
          assert!(!escaped.contains("</script>"), "should escape </script>");
          assert!(escaped.contains("\\u003c"), "should unicode-escape <");
          assert!(escaped.contains("\\u003e"), "should unicode-escape >");
      }

      #[test]
      fn url_matching_end_to_end() {
          let file = CreativeOpportunitiesFile { slots: vec![make_slot()] };
          assert_eq!(match_slots(&file.slots, "/2024/01/my-article").len(), 1, "should match article");
          assert_eq!(match_slots(&file.slots, "/about").len(), 0, "should not match /about");
          assert_eq!(match_slots(&file.slots, "/").len(), 0, "should not match root");
      }
  }
  ```

- [ ] **Step 2: Run tests**

  Run: `cargo test -p trusted-server-core creative_opportunities_tests`
  Expected: all pass

- [ ] **Step 3: Run full suite + CI gates**

  ```bash
  cargo test --workspace
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo fmt --all -- --check
  cd crates/js/lib && npx vitest run
  cd crates/js/lib && npm run format
  cd docs && npm run format
  ```

  Expected: all clean

- [ ] **Step 4: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Add integration tests for creative opportunities pipeline (head globals, bid map, XSS)"
  ```

---

## Manual Verification Checklist

Run `fastly compute serve` and verify:

- [ ] **No match:** Request `/about` — no `__ts_ad_slots`, no `__ts_request_id` in response HTML; no `Cache-Control: private, no-store`
- [ ] **Match:** Request `/2024/01/article` — `window.__ts_ad_slots` and `window.__ts_request_id` in `<head>`; `Cache-Control: private, no-store`; **no `__ts_bids` in HTML**
- [ ] **`/ts-bids` cache hit:** Request `/2024/01/article`, then `GET /ts-bids?rid=<rid-from-page>` — returns JSON within 30ms; `Content-Type: application/json`; `Cache-Control: private, no-store`
- [ ] **`/ts-bids` unknown rid:** `GET /ts-bids?rid=not-a-real-id` — returns 404
- [ ] **`/ts-bids` missing rid:** `GET /ts-bids` — returns 400
- [ ] **Empty file kill-switch:** Empty `creative-opportunities.toml` → no globals injected on any URL; no cache headers
- [ ] **Auction timeout:** Set `auction_timeout_ms = 1` → `/ts-bids` returns `{}` promptly
- [ ] **XSS check:** Add `targeting = { zone = "</script><script>alert(1)//" }` to a slot → verify `<` and `>` in HTML source; no unescaped `<` or `>`
- [ ] **Cache-Control absent when no slots match:** Confirm `Cache-Control: private, no-store` is NOT set for URLs with no slot match (preserves origin cache directives)

---

## Known Limitations

| Item                                                | Notes                                                                                                                                                                                                                                                                                                                                                                             |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AuctionContext.request` placeholder                | After `send_async` consumes `req`, a blank `fastly::Request::new()` is passed. Providers reading User-Agent/IP/cookies from this field will degrade silently. Resolve before production: clone needed fields before `send_async` or remove the `request` field from `AuctionContext` in favor of `client_info`.                                                                   |
| `nurl` dynamic backend allowlist                    | `fire_winning_nurls` uses `BackendConfig::from_url()`. The SSP domain must be in `fastly.toml`'s dynamic backend origins allowlist, or `allow_dynamic_backends = true`. Confirm with ops before enabling.                                                                                                                                                                         |
| `bid_cache` not shared across Fastly edge instances | `BidCache` is per-process. If the browser's `/ts-bids` request routes to a different edge instance than the page request, it will receive 404 and GPT falls back gracefully. Acceptable for Phase 1; Phase 2 can use Fastly KV for cross-instance sharing.                                                                                                                        |
| `burl` absent for APS                               | APS bids have no `burl`. The field is `null` in `/ts-bids` response for APS slots — the `slotRenderEnded` check correctly short-circuits on `!bid.burl`.                                                                                                                                                                                                                          |
| `bid_cache` `Mutex` contention                      | In sustained high-traffic scenarios, the `Mutex` may become a contention point. Phase 1 workload (one lock per request, 50ms poll interval for `/ts-bids`) is well within Fastly's concurrency model. Revisit if profiling shows contention.                                                                                                                                      |
| Long-poll sleep in `/ts-bids`                       | `std::thread::sleep(50ms)` is used inside `wait_for`. This cooperates well with Fastly's execution model in practice, but may affect throughput under extreme concurrency. Validated via load testing before production rollout.                                                                                                                                                  |
| Chunked encoding and buffered origins               | Spec §4.3 requires stripping `Content-Length` and setting `Transfer-Encoding: chunked` on all responses (including buffered WordPress/Drupal origins). Task 9 Step 4 implements this, but the Fastly runtime may handle chunked re-encoding transparently for some origin types. Verify behavior with a known buffered origin (e.g., a static file server) during manual testing. |
| Empty-file kill-switch behavior                     | Deploying `creative-opportunities.toml` with zero `[[slot]]` entries disables the feature entirely — no auction fires, no globals injected, no `Cache-Control: private, no-store` set. This is intentional and tested in the Manual Verification Checklist but has no automated unit test — add one to Task 4 or Task 14 if this path is operationally critical.                  |
