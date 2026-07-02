# Server-Side Ad Templates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable the Fastly edge to fire a full header-bidding auction (PBS + APS) in parallel with the origin fetch, inject `window.__ts_ad_slots` into `<head>`, then inject `window.__ts_bids` inline before `</body>` so the client drives GPT directly — without Prebid.js and without blocking FCP.

**Architecture:** A new `creative-opportunities.toml` holds per-URL slot templates. At request time the publisher path matches the URL and fires the auction + origin fetch concurrently via `send_async()`. `window.__ts_ad_slots` is injected at `<head>` open with no auction wait. The `</body>` close tag is held — bounded by `A_deadline` — until the auction completes or times out; `window.__ts_bids` is then injected inline before the close tag so bids and HTML travel together in a single response. The client's `__tsAdInit` reads `window.__ts_bids` synchronously (no fetch, no Promise) and drives GPT. Both `nurl` and `burl` fire client-side from `slotRenderEnded` to avoid billing inflation. A slim-Prebid bundle lazy-loads post-`window.load` for refresh auctions and identity warm-up.

**Tech Stack:** Rust 2024, `lol_html` 2.7.2 (existing), `glob` crate (new workspace dep), `serde`/`toml` (existing), `std::sync::{Arc, RwLock}` for within-request shared auction state, `AuctionOrchestrator::run_auction` (existing `async fn`), TypeScript for GPT shim extension.

> **Phase 1 streaming note:** The spec describes true streaming where body content above `</body>` paints before the auction completes. Implementing this with lol_html's synchronous callback model requires a complex outer streaming loop (emit chunks as they arrive from origin; hold only the `</body>` chunk until auction resolves). Phase 1 uses a simpler approach: await the auction (or `A_deadline`) before processing origin HTML through lol_html, then send the fully assembled response. This still delivers the server-side auction benefit and achieves the same ad-visible latency target (~870ms cache hit). The FCP claim (~80ms) of the spec requires the streaming approach and is tracked as a Phase 2 optimization. The shared `Arc<RwLock<Option<String>>>` is the correct coordination primitive either way — the Phase 2 upgrade path only changes when the auction is awaited relative to lol_html processing.

---

## File Map

### New files

| File                                                       | Responsibility                                                                              |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `creative-opportunities.toml`                              | Slot template definitions (page patterns, formats, floor prices, per-provider params)       |
| `crates/trusted-server-core/src/creative_opportunities.rs` | Config types, TOML parsing, URL glob matching, slot→`AdSlot` conversion, startup validation |
| `crates/trusted-server-core/src/price_bucket.rs`           | Prebid price granularity tables; converts `f64` CPM to `hb_pb` string                       |

### Modified files

| File                                                    | Change summary                                                                                                                                                                                                       |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                                            | Add `glob = "0.3"` to `[workspace.dependencies]`                                                                                                                                                                     |
| `crates/trusted-server-core/Cargo.toml`                 | Add `glob = { workspace = true }`                                                                                                                                                                                    |
| `crates/trusted-server-core/Cargo.toml` (build-deps)    | Verify `regex` is listed under `[build-dependencies]` (needed for slot-ID validation in `build.rs`)                                                                                                                  |
| `crates/trusted-server-core/src/auction/types.rs`       | Add `MediaType::banner()` constructor; add `ad_id: Option<String>` to `Bid`                                                                                                                                          |
| `crates/trusted-server-core/src/settings.rs`            | Add `creative_opportunities: Option<CreativeOpportunitiesConfig>` to `Settings`                                                                                                                                      |
| `trusted-server.toml`                                   | Add `[creative_opportunities]` section                                                                                                                                                                               |
| `crates/trusted-server-core/build.rs`                   | Validate slot IDs at build time using inline TOML parse; `rerun-if-changed` for `creative-opportunities.toml`                                                                                                        |
| `crates/trusted-server-core/src/html_processor.rs`      | Add `ad_slots_script: Option<String>` (head) and `ad_bids_script: Arc<RwLock<Option<String>>>` (body) to `HtmlProcessorConfig`; inject at `<head>` open and via `el.on_end_tag()` on body element                    |
| `crates/trusted-server-core/src/publisher.rs`           | Convert to `async fn`; add eligibility gates (bot UA, prefetch, HEAD); EID decoration; shared `AuctionBidState`; inject `__ts_bids` before `</body>`; `Cache-Control: private, max-age=0`; strip `Surrogate-Control` |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Await the now-async handler; pass orchestrator reference; no `/ts-bids` route                                                                                                                                        |
| `crates/trusted-server-core/src/integrations/gpt.rs`    | Extend `head_inserts()` to emit `__tsAdInit` that reads `window.__ts_bids` synchronously; `ts_initial=1` sentinel; nurl+burl from `slotRenderEnded`                                                                  |
| `crates/js/lib/src/integrations/gpt/index.ts`           | Synchronous `__tsAdInit` with `window.__ts_bids` read; nurl+burl `sendBeacon`; lazy slim-Prebid loader post-`window.load`                                                                                            |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Add `suppress_nurl: bool` config (default `false`) as per-bidder escape hatch; remove server-side nurl firing                                                                                                        |

### Deleted (relative to prior revision of this spec)

- `crates/trusted-server-core/src/bid_cache.rs` — never created; in-process cache rejected because Fastly Compute per-request Wasm isolates are not pinned across requests
- `/ts-bids` endpoint — never created; body-injection replaces the fetch pattern

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
  use serde::{Deserialize, Serialize};

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
      pub fn dense() -> Self {
          Self::Dense
      }
  }

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
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`

`CreativeOpportunityFormat` uses `#[serde(default = "MediaType::banner")]` which requires a free function. Add `ad_id: Option<String>` to `Bid` for `hb_adid` targeting. The `suppress_nurl` config escape hatch is also added here to `PrebidConfig`.

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
      pub fn banner() -> Self {
          Self::Banner
      }
  }
  ```

  Add `ad_id: Option<String>` field to `Bid`. Update the `make_bid` test helper to include `ad_id: None`.

- [ ] **Step 3: Run tests**

  Run: `cargo test -p trusted-server-core auction`
  Expected: all tests pass

- [ ] **Step 4: Populate `ad_id` in prebid.rs**

  In `crates/trusted-server-core/src/integrations/prebid.rs`, in the `Bid` construction, find where `nurl` and `burl` are set and add:

  ```rust
  ad_id: bid_obj.get("adid")
      .or_else(|| bid_obj.get("id"))
      .and_then(|v| v.as_str())
      .map(String::from),
  ```

  (Prebid Server uses lowercase `adid`. Fall back to `id` if absent.)

- [ ] **Step 5: Add `suppress_nurl` to `PrebidConfig`**

  In `prebid.rs`, add to the `PrebidConfig` struct:

  ```rust
  #[serde(default)]
  pub suppress_nurl: bool,
  ```

  Write a test:

  ```rust
  #[test]
  fn prebid_config_suppress_nurl_defaults_to_false() {
      let config = PrebidConfig::default();
      assert!(!config.suppress_nurl, "should not suppress nurl by default");
  }
  ```

  Run: `cargo test -p trusted-server-core integrations::prebid`
  Expected: passes

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/auction/types.rs \
          crates/trusted-server-core/src/integrations/prebid.rs
  git commit -m "Add MediaType::banner(), Bid::ad_id, and suppress_nurl config to PrebidConfig"
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
          assert_eq!(slot.resolved_gam_unit_path("21765378893"), "/21765378893/atf");
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
  use std::collections::HashMap;

  use glob::Pattern;
  use serde::{Deserialize, Serialize};

  use crate::auction::types::{AdFormat, AdSlot, MediaType};
  use crate::price_bucket::PriceGranularity;

  #[derive(Debug, Clone, Deserialize, Serialize)]
  pub struct CreativeOpportunitiesConfig {
      pub gam_network_id: String,
      #[serde(default)]
      pub auction_timeout_ms: Option<u32>,
      #[serde(default = "PriceGranularity::dense")]
      pub price_granularity: PriceGranularity,
  }

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
      pub fn matches_path(&self, path: &str) -> bool {
          self.page_patterns.iter().any(|pattern| {
              Pattern::new(pattern)
                  .map(|p| p.matches(path))
                  .unwrap_or(false)
          })
      }

      pub fn resolved_gam_unit_path(&self, gam_network_id: &str) -> String {
          self.gam_unit_path
              .clone()
              .unwrap_or_else(|| format!("/{}/{}", gam_network_id, self.id))
      }

      pub fn resolved_div_id(&self) -> &str {
          self.div_id.as_deref().unwrap_or(&self.id)
      }

      pub fn to_ad_slot(&self, gam_network_id: &str) -> AdSlot {
          let mut bidders: HashMap<String, serde_json::Value> = HashMap::new();
          if let Some(ref aps) = self.providers.aps {
              bidders.insert("aps".to_string(), serde_json::json!({ "slotID": aps.slot_id }));
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

  #[derive(Debug, Clone, Deserialize)]
  pub struct CreativeOpportunityFormat {
      pub width: u32,
      pub height: u32,
      #[serde(default = "MediaType::banner")]
      pub media_type: MediaType,
  }

  impl CreativeOpportunityFormat {
      fn to_ad_format(&self) -> AdFormat {
          AdFormat { media_type: self.media_type.clone(), width: self.width, height: self.height }
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

  #[derive(Debug, Clone, Deserialize, Default)]
  pub struct CreativeOpportunitiesFile {
      #[serde(rename = "slot", default)]
      pub slots: Vec<CreativeOpportunitySlot>,
  }

  pub fn validate_slot_id(id: &str) -> Result<(), String> {
      if id.is_empty() {
          return Err("slot id must not be empty".to_string());
      }
      if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
          Ok(())
      } else {
          Err(format!("slot id '{id}' contains invalid characters; only [A-Za-z0-9_-] allowed"))
      }
  }

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
- Modify (or create): `crates/trusted-server-adapter-fastly/build.rs`

- [ ] **Step 1: Add slot-ID validation to `build.rs`**

  In `crates/trusted-server-core/build.rs`, add after the existing settings validation.

  > **Note:** `build.rs` runs in a separate compilation context and cannot import `creative_opportunities::validate_slot_id`. Reimplement the same `[A-Za-z0-9_-]+` check inline using the `regex` crate (build-dependency only — not in the wasm binary). This is an intentional duplication boundary.

  ```rust
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

  In `crates/trusted-server-core/Cargo.toml`, under `[build-dependencies]`, ensure `regex` is listed:

  ```toml
  [build-dependencies]
  regex = "1"
  # ... other build deps
  ```

  Add if absent. This is a build-only dependency — does not enter the WASM binary.

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

- [ ] **Step 5: Add `rerun-if-changed` for adapter crate**

  Check whether `crates/trusted-server-adapter-fastly/build.rs` exists. If it does, add:

  ```rust
  println!("cargo:rerun-if-changed=../../../creative-opportunities.toml");
  ```

  If no `build.rs` exists in the adapter crate, create one:

  ```rust
  fn main() {
      println!("cargo:rerun-if-changed=../../../creative-opportunities.toml");
  }
  ```

  Run: `cargo build --package trusted-server-adapter-fastly`
  Expected: clean build.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/build.rs \
          crates/trusted-server-adapter-fastly/build.rs
  git commit -m "Validate creative-opportunities.toml slot IDs at build time using inline TOML parse"
  ```

---

## Task 7: HTML processor — head injection and body-end injection

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`

TS injects two `<script>` blocks:

1. `window.__ts_ad_slots` — at `<head>` open, no wait, via existing `el.prepend()`.
2. `window.__ts_bids` — before `</body>`, via `el.on_end_tag()` on the body element. The handler reads from a shared `Arc<RwLock<Option<String>>>` written by the auction task (Task 9). By the time `</body>` is reached in the HTML stream, Task 9 has already awaited the auction (or its deadline), so the shared state is always populated.

> **Critical design constraint:** `</head>` must never be held. Only `</body>` is held (by the outer streaming mechanism in Task 9). `HtmlProcessorConfig` receives the ready-to-inject script strings; it does not decide when to wait.

- [ ] **Step 1: Write failing tests**

  In `html_processor.rs` tests:

  ```rust
  #[test]
  fn injects_ad_slots_at_head_open() {
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: Some(
              r#"<script>window.__ts_ad_slots=JSON.parse("[]");</script>"#.to_string()
          ),
          ad_bids_state: std::sync::Arc::new(std::sync::RwLock::new(None)),
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head><title>T</title></head><body>content</body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("window.__ts_ad_slots"), "should inject ad slots at head-open");
      assert!(!html.contains("__ts_request_id"), "must NOT inject request_id — body-injection arch has no request_id");
  }

  #[test]
  fn injects_ts_bids_before_body_close() {
      let bids_script = r#"<script>window.__ts_bids=JSON.parse("{\"atf\":{\"hb_pb\":\"1.00\"}}");</script>"#;
      let state = std::sync::Arc::new(std::sync::RwLock::new(
          Some(bids_script.to_string())
      ));
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: None,
          ad_bids_state: state,
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head></head><body>content</body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("window.__ts_bids"), "should inject bids before </body>");
      let bids_pos = html.find("window.__ts_bids").expect("bids should be in output");
      let body_close_pos = html.find("</body>").expect("</body> should be in output");
      assert!(bids_pos < body_close_pos, "bids must appear before </body>");
  }

  #[test]
  fn injects_empty_ts_bids_when_state_is_none() {
      let state = std::sync::Arc::new(std::sync::RwLock::new(None));
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: None,
          ad_bids_state: state,
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head></head><body>content</body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("__ts_bids=JSON.parse(\"{}\""), "should inject empty bids on None state");
  }
  ```

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: compile error (no `ad_bids_state` field yet)

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

- [ ] **Step 3: Update `HtmlProcessorConfig`**

  ```rust
  pub struct HtmlProcessorConfig {
      pub origin_host: String,
      pub request_host: String,
      pub request_scheme: String,
      pub integrations: IntegrationRegistry,
      /// Pre-computed `<script>window.__ts_ad_slots=...;</script>`.
      /// Injected at `<head>` open. `None` when no slots matched.
      pub ad_slots_script: Option<String>,
      /// Shared auction result script — written by the auction task before HTML processing
      /// begins. Handler reads this in `el.on_end_tag()` on the body element.
      /// `None` means no auction ran (consent denied, bot UA, no slot match, etc.);
      /// inject empty `__ts_bids = {}` as graceful fallback.
      pub ad_bids_state: std::sync::Arc<std::sync::RwLock<Option<String>>>,
  }
  ```

  Update `from_settings` (or wherever `HtmlProcessorConfig` is constructed) to initialize `ad_bids_state: Arc::new(RwLock::new(None))`.

- [ ] **Step 4: Inject `ad_slots_script` at head-open**

  In `create_html_processor`, within the existing `element!("head", ...)` handler:

  ```rust
  let ad_slots_script = config.ad_slots_script.clone();
  // existing captures...

  element!("head", |el| {
      let mut snippet = String::new();
      if let Some(ref slots_script) = ad_slots_script {
          snippet.push_str(slots_script);
      }
      // existing integration head inserts...
      if !snippet.is_empty() {
          el.prepend(&snippet, ContentType::Html);
      }
      // DO NOT register on_end_tag — </head> flushes immediately
      Ok(())
  })
  ```

- [ ] **Step 5: Inject `__ts_bids` before `</body>` via `el.on_end_tag()`**

  Add a new handler in `create_html_processor`. The shared state is already populated by the time lol_html reaches `</body>` (Task 9 awaits the auction before starting HTML processing):

  ```rust
  let ad_bids_state = config.ad_bids_state.clone();

  element!("body", |el| {
      let state = ad_bids_state.clone();
      el.on_end_tag(move |end_tag| {
          let script = state.read().expect("should read bid state");
          let bids_script = match &*script {
              Some(s) => s.clone(),
              None => {
                  r#"<script>window.__ts_bids=JSON.parse("{}");</script>"#.to_string()
              }
          };
          end_tag.before(&bids_script, ContentType::Html);
          Ok(())
      })?;
      Ok(())
  })
  ```

- [ ] **Step 6: Run tests**

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: all tests pass

- [ ] **Step 7: Run full suite**

  Run: `cargo test --workspace`
  Expected: clean

- [ ] **Step 8: Commit**

  ```bash
  git add crates/trusted-server-core/src/html_processor.rs \
          crates/trusted-server-core/src/integrations/registry.rs
  git commit -m "Inject __ts_ad_slots at head-open and __ts_bids before </body> via shared auction state"
  ```

---

## Task 8: `handle_publisher_request` async restructuring

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

> **Key constraint from spec §4.3 and §3:** No `bid_cache`. No `/ts-bids`. No `request_id`. Bids travel inline with the HTML response via body injection. The `Arc<RwLock<Option<String>>>` is the coordination mechanism within a single request's lifetime — it is written before HTML processing and read by the lol_html `</body>` handler.

> **Eligibility gating (spec §4.3):** Auctions fire only for real GET requests from non-bot, non-prefetch clients with TCF Purpose 1 consent and at least one matching slot. All other requests proceed with no auction and no `__ts_bids` injection.

> **Cache-Control (spec §4.7):** Set `Cache-Control: private, max-age=0` (not `no-store`) to preserve BFCache eligibility. Strip `Surrogate-Control` and `Fastly-Surrogate-Control`.

- [ ] **Step 1: Update function signature**

  Change `handle_publisher_request` in `publisher.rs`:

  > **Existing context:** The existing `publisher.rs` function body already computes `consent_context`, `ec_id`, `request_info`, `origin_host`, and `backend_name` before the origin fetch. Steps below insert new logic between those existing computations and the origin fetch — they do not replace them.

  ```rust
  pub async fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      services: &RuntimeServices,
      orchestrator: &crate::auction::orchestrator::AuctionOrchestrator,
      slots_file: &crate::creative_opportunities::CreativeOpportunitiesFile,
      mut req: Request,
  ) -> Result<PublisherResponse, Report<TrustedServerError>>
  ```

  Add imports at top of file:

  ```rust
  use std::sync::{Arc, RwLock};
  use fastly::http::header;
  use crate::auction::orchestrator::AuctionOrchestrator;
  use crate::auction::types::{AuctionContext, AuctionRequest, PublisherInfo, UserInfo, SiteInfo};
  use crate::creative_opportunities::{CreativeOpportunitiesFile, match_slots};
  use crate::price_bucket::price_bucket;
  ```

  > **`send_async` return type:** `req.send_async()` returns `fastly::handle::PendingRequestHandle` (re-exported as `fastly::PendingRequest` in recent versions). Confirm the exact type from the `fastly` crate version in `Cargo.toml`; `.wait()` is the blocking resolve method on whichever type is returned.

- [ ] **Step 2: Apply auction-eligibility gates**

  At the top of the function body, before origin fetch:

  ```rust
  let request_path = req.get_path().to_string();
  let request_method = req.get_method().clone();

  // Gate 1: Only GET triggers auctions. HEAD skips everything.
  let is_get = request_method == fastly::http::Method::GET;

  // Gate 2: Skip prefetch hints (Sec-Purpose: prefetch or Purpose: prefetch).
  let is_prefetch = req.get_header_str("sec-purpose")
      .map_or(false, |v| v.contains("prefetch"))
      || req.get_header_str("purpose")
      .map_or(false, |v| v.contains("prefetch"));

  // Gate 3: Skip well-known crawler UAs (protects SSP QPS budget).
  let user_agent = req.get_header_str("user-agent").unwrap_or("");
  let is_bot = ["Googlebot", "Bingbot", "AhrefsBot", "SemrushBot", "DotBot"]
      .iter()
      .any(|bot| user_agent.contains(bot));

  // Gate 4: Slot match.
  let matched_slots: Vec<_> = if settings.creative_opportunities.is_some() && is_get {
      match_slots(&slots_file.slots, &request_path)
          .into_iter()
          .cloned()
          .collect()
  } else {
      Vec::new()
  };

  // Gate 5: TCF Purpose 1 consent.
  let consent_allows_auction = consent_context
      .tcf
      .as_ref()
      .map_or(false, |tcf| tcf.has_purpose_consent(1));

  let should_run_auction = is_get
      && !is_prefetch
      && !is_bot
      && !matched_slots.is_empty()
      && consent_allows_auction;

  let auction_timeout_ms = settings
      .creative_opportunities
      .as_ref()
      .and_then(|co| co.auction_timeout_ms)
      .unwrap_or(settings.auction.timeout_ms);
  ```

- [ ] **Step 3: Create shared bid state, fire origin + auction concurrently**

  ```rust
  // Shared state: auction task writes the ready-to-inject script; lol_html </body>
  // handler reads it. Both within the same request — no cross-request sharing.
  let ad_bids_state: Arc<RwLock<Option<String>>> = Arc::new(RwLock::new(None));

  restrict_accept_encoding(&mut req);
  req.set_header("host", &origin_host);

  // Fire origin immediately — both origin and auction SSP calls overlap on the network.
  let pending_origin = req
      .send_async(&backend_name)
      .change_context(TrustedServerError::Proxy {
          message: "Failed to dispatch async origin request".to_string(),
      })?;

  // Run auction. Internal SSP calls use send_async and overlap with origin fetch.
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

  // Write auction result to shared state before HTML processing begins.
  // The lol_html </body> handler reads this synchronously — it is always populated here.
  // `build_bid_map` returns `serde_json::Map<String, serde_json::Value>`.
  if should_run_auction {
      let co_config = settings.creative_opportunities.as_ref()
          .expect("should be present");
      let empty_bids: std::collections::HashMap<String, crate::auction::types::Bid> =
          std::collections::HashMap::new();
      let winning_bids = auction_result.as_ref()
          .map(|r| &r.winning_bids)
          .unwrap_or(&empty_bids);
      let bid_map = build_bid_map(winning_bids, co_config.price_granularity);
      let bids_script = build_bids_script(&bid_map);
      *ad_bids_state.write().expect("should write bid state") = Some(bids_script);
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
  let ad_slots_script = if let Some(co_config) = &settings.creative_opportunities {
      if !matched_slots.is_empty() {
          Some(build_ad_slots_script(&matched_slots, co_config))
      } else {
          None
      }
  } else {
      None
  };

  // Set cache headers when slots matched. private, max-age=0 (not no-store) preserves
  // BFCache eligibility — browser back/forward cache restores the already-rendered ad
  // without firing a new GAM call, which is the desired behavior.
  if ad_slots_script.is_some() {
      response.set_header(header::CACHE_CONTROL, "private, max-age=0");
      response.remove_header("surrogate-control");
      response.remove_header("fastly-surrogate-control");
  }

  // Force chunked encoding so </head> reaches the browser immediately as chunks arrive.
  // Sending both Content-Length and Transfer-Encoding is invalid HTTP/1.1.
  response.remove_header(header::CONTENT_LENGTH);
  response.set_header("transfer-encoding", "chunked");
  ```

- [ ] **Step 5: Thread shared state into `OwnedProcessResponseParams`**

  Update `OwnedProcessResponseParams`:

  ```rust
  pub struct OwnedProcessResponseParams {
      // existing fields...
      pub(crate) ad_slots_script: Option<String>,
      pub(crate) ad_bids_state: Arc<RwLock<Option<String>>>,
  }
  ```

  Pass both through to `create_html_stream_processor` and into `HtmlProcessorConfig`.

- [ ] **Step 6: Add `pub(crate)` helper functions**

  > **`BidMap` type:** Use `serde_json::Map<String, serde_json::Value>` directly — no separate module needed.

  Add helpers in this order (each function is used by the one below it, so define leaf functions first):

  ```rust
  /// HTML-escape a JSON string for safe inline `<script>` injection via `JSON.parse`.
  /// Unicode-escapes `<`, `>`, `&`, U+2028, U+2029 to prevent markup breakout.
  fn html_escape_for_script(json: &str) -> String {
      json.replace('&', "\\u0026")
          .replace('<', "\\u003c")
          .replace('>', "\\u003e")
          .replace('\u{2028}', "\\u2028")
          .replace('\u{2029}', "\\u2029")
  }

  /// Build the `BidMap` written to shared state and injected before `</body>`.
  /// Keyed by slot ID. Values: hb_pb, hb_bidder, hb_adid, nurl (optional), burl (optional).
  pub(crate) fn build_bid_map(
      winning_bids: &std::collections::HashMap<String, crate::auction::types::Bid>,
      price_granularity: crate::price_bucket::PriceGranularity,
  ) -> serde_json::Map<String, serde_json::Value> {
      winning_bids
          .iter()
          .filter_map(|(slot_id, bid)| {
              let cpm = bid.price?;
              let mut entry = serde_json::Map::new();
              entry.insert("hb_pb".to_string(), serde_json::Value::String(price_bucket(cpm, price_granularity)));
              entry.insert("hb_bidder".to_string(), serde_json::Value::String(bid.bidder.clone()));
              entry.insert("hb_adid".to_string(), serde_json::Value::String(
                  bid.ad_id.as_deref().unwrap_or("").to_string()
              ));
              if let Some(ref nurl) = bid.nurl {
                  entry.insert("nurl".to_string(), serde_json::Value::String(nurl.clone()));
              }
              if let Some(ref burl) = bid.burl {
                  entry.insert("burl".to_string(), serde_json::Value::String(burl.clone()));
              }
              Some((slot_id.clone(), serde_json::Value::Object(entry)))
          })
          .collect()
  }

  /// Build the `<script>` block injected before `</body>`.
  /// Contains `window.__ts_bids` keyed by slot ID.
  pub(crate) fn build_bids_script(bid_map: &serde_json::Map<String, serde_json::Value>) -> String {
      let bids_str = serde_json::to_string(bid_map).expect("should serialize bids");
      let escaped = html_escape_for_script(&bids_str);
      format!(r#"<script>window.__ts_bids=JSON.parse("{escaped}");</script>"#)
  }

  /// Build the `<script>` block injected at `<head>` open.
  /// Contains `window.__ts_ad_slots` only — no request ID, no bids.
  pub(crate) fn build_ad_slots_script(
      matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
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
      let slots_json_str = serde_json::to_string(&slots_json).expect("should serialize ad slots");
      let escaped = html_escape_for_script(&slots_json_str);
      format!(r#"<script>window.__ts_ad_slots=JSON.parse("{escaped}");</script>"#)
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

  > **Type note:** All helper signatures use `serde_json::Map<String, serde_json::Value>` directly. Do not create a `BidMap` type alias or `bid_types.rs` module.

- [ ] **Step 7: Update `main.rs` call site**

  In `crates/trusted-server-adapter-fastly/src/main.rs`:

  ```rust
  // At startup (top of main() / request handler setup, before the request dispatch loop).
  // include_str! embeds the file at compile time — no runtime file I/O.
  const CREATIVE_OPPORTUNITIES_TOML: &str =
      include_str!("../../../creative-opportunities.toml");

  let slots_file: trusted_server_core::creative_opportunities::CreativeOpportunitiesFile =
      toml::from_str(CREATIVE_OPPORTUNITIES_TOML)
          .expect("should parse creative-opportunities.toml");
  ```

  `slots_file` is a local in the startup/handler scope and passed by reference into `handle_publisher_request` on each request — no `Arc` needed since it's immutable and the handler borrows it.

  Update the call to `handle_publisher_request`:

  ```rust
  match handle_publisher_request(
      settings,
      integration_registry,
      &publisher_services,
      orchestrator,   // existing
      &slots_file,    // new
      req,
  ).await {
      // existing match arms unchanged
  }
  ```

  There is **no `/ts-bids` route** to add. The body injection is complete within `handle_publisher_request`.

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
  git commit -m "Convert handle_publisher_request to async; body-inject __ts_bids; eligibility gates; max-age=0"
  ```

---

## Task 9: GPT head injector — emit `__tsAdInit` with synchronous bid read

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`

> **Critical:** `__tsAdInit` reads `window.__ts_bids` **synchronously** — no fetch, no Promise. `window.__ts_bids` is already on the page (injected before `</body>`) when `__tsAdInit` runs (it executes post-DCL, after `</body>` is received). Both `nurl` and `burl` fire client-side from `slotRenderEnded`; neither is fired server-side.

- [ ] **Step 1: Write failing test**

  ```rust
  #[test]
  fn head_inserts_includes_ts_ad_init_with_synchronous_bids_read() {
      let config = test_config();
      let integration = GptIntegration::new(config);
      let ctx = make_test_context();
      let inserts = integration.head_inserts(&ctx);
      let combined = inserts.join("");
      assert!(combined.contains("__tsAdInit"), "should define __tsAdInit");
      assert!(combined.contains("window.__ts_bids"), "should read window.__ts_bids synchronously");
      assert!(combined.contains("ts_initial"), "should set ts_initial sentinel");
      assert!(combined.contains("slotRenderEnded"), "should register slotRenderEnded");
      assert!(combined.contains("sendBeacon"), "should fire nurl and burl via sendBeacon");
      assert!(combined.contains("nurl"), "should fire nurl on confirmed render");
      assert!(!combined.contains("/ts-bids"), "must NOT fetch /ts-bids — bids are inline on the page");
      assert!(!combined.contains("bidsPromise"), "must NOT use bidsPromise — bids are synchronous");
      assert!(!combined.contains("__ts_request_id"), "must NOT reference request_id — no longer used");
  }
  ```

  Run: `cargo test -p trusted-server-core integrations::gpt`
  Expected: FAIL

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
              // __tsAdInit: reads window.__ts_bids synchronously (injected before </body>).
              // No fetch, no Promise. Executes post-DCL when </body> has already arrived.
              // Both nurl and burl fire client-side from slotRenderEnded — never server-side.
              // Note: window.__tsjs_installGptShim above is an EXISTING function in the
              // tsjs-core bundle that stubs googletag.cmd before the real GPT loads.
              concat!(
                  "<script>",
                  "window.__tsAdInit=function(){",
                    "var slots=window.__ts_ad_slots||[];",
                    "var bids=window.__ts_bids||{};",
                    "googletag.cmd.push(function(){",
                      "var gptSlots=slots.map(function(slot){",
                        "var s=googletag.defineSlot(slot.gam_unit_path,slot.formats,slot.div_id);",
                        "if(!s)return null;",
                        "s.addService(googletag.pubads());",
                        "Object.entries(slot.targeting||{}).forEach(function(e){s.setTargeting(e[0],e[1]);});",
                        "var b=bids[slot.id]||{};",
                        "[\"hb_pb\",\"hb_bidder\",\"hb_adid\"].forEach(function(k){if(b[k])s.setTargeting(k,b[k]);});",
                        "s.setTargeting(\"ts_initial\",\"1\");",
                        "return{id:slot.id,gptSlot:s};",
                      "}).filter(Boolean);",
                      "googletag.pubads().enableSingleRequest();",
                      "googletag.enableServices();",
                      "googletag.pubads().addEventListener(\"slotRenderEnded\",function(ev){",
                        "var id=ev.slot.getSlotElementId();",
                        "var b=bids[id]||{};",
                        "var ourBidWon=!ev.isEmpty&&b.hb_adid&&ev.slot.getTargeting(\"hb_adid\")[0]===b.hb_adid;",
                        "if(ourBidWon){",
                          "if(b.nurl)navigator.sendBeacon(b.nurl);",
                          "if(b.burl)navigator.sendBeacon(b.burl);",
                        "}",
                      "});",
                      "googletag.pubads().refresh();",
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
  git commit -m "Emit __tsAdInit with synchronous window.__ts_bids read; nurl+burl from slotRenderEnded"
  ```

---

## Task 10: `gpt/index.ts` — TypeScript `__tsAdInit` with slim-Prebid lazy loader

**Files:**

- Modify: `crates/js/lib/src/integrations/gpt/index.ts`

The TypeScript version mirrors the Rust inline string from Task 9 and adds the lazy slim-Prebid loader. Slim-Prebid loads post-`window.load` and handles two things: refresh auctions (via existing GPT refresh triggers) and userID module warm-up to enrich the EC graph for the next request.

- [ ] **Step 1: Write failing tests**

  In `crates/js/lib/src/integrations/gpt/index.test.ts`:

  ```typescript
  import { describe, it, expect, vi, beforeEach } from 'vitest'

  describe('installTsAdInit', () => {
    beforeEach(() => {
      delete (window as any).__ts_ad_slots
      delete (window as any).__ts_bids
      delete (window as any).__tsAdInit
    })

    it('reads window.__ts_bids synchronously and applies bid targeting before refresh', async () => {
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue('atf'),
        getTargeting: vi.fn().mockReturnValue(['abc']),
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
      ;(window as any).__ts_bids = {
        atf: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      }

      const fetchSpy = vi.spyOn(global, 'fetch')

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      ;(window as any).__tsAdInit()

      expect(fetchSpy).not.toHaveBeenCalled()
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00')
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo')
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('ts_initial', '1')
      expect(mockPubads.refresh).toHaveBeenCalled()

      fetchSpy.mockRestore()
    })

    it('fires both nurl and burl via sendBeacon on slotRenderEnded when our bid won', async () => {
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
      ;(window as any).__ts_bids = {
        atf: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      }

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      ;(window as any).__tsAdInit()

      expect(capturedListener).toBeDefined()
      capturedListener!({ isEmpty: false, slot: mockSlot })

      expect(beaconSpy).toHaveBeenCalledWith('https://ssp/win')
      expect(beaconSpy).toHaveBeenCalledWith('https://ssp/bill')
      beaconSpy.mockRestore()
    })

    it('does not fire nurl/burl when bid did not win GAM line item', async () => {
      const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      let capturedListener: ((e: any) => void) | undefined

      const mockSlotNoMatch = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
        getSlotElementId: vi.fn().mockReturnValue('atf'),
        getTargeting: vi.fn().mockReturnValue(['OTHER_BID_ID']),
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
        defineSlot: vi.fn().mockReturnValue(mockSlotNoMatch),
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
      ;(window as any).__ts_bids = {
        atf: {
          hb_pb: '1.00',
          hb_bidder: 'kargo',
          hb_adid: 'abc',
          nurl: 'https://ssp/win',
          burl: 'https://ssp/bill',
        },
      }

      const { installTsAdInit } = await import('./index')
      installTsAdInit()
      ;(window as any).__tsAdInit()
      capturedListener!({ isEmpty: false, slot: mockSlotNoMatch })

      expect(beaconSpy).not.toHaveBeenCalled()
      beaconSpy.mockRestore()
    })

    it('calls refresh even when __ts_bids is empty (graceful fallback)', () => {
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
      ;(window as any).__ts_bids = {}

      const { installTsAdInit } = require('./index')
      installTsAdInit()
      ;(window as any).__tsAdInit()

      expect(mockPubads.refresh).toHaveBeenCalled()
    })
  })
  ```

  Run: `cd crates/js/lib && npx vitest run`
  Expected: FAIL — `installTsAdInit` not defined or assertions fail

- [ ] **Step 2: Implement `installTsAdInit` in `index.ts`**

  Replace the old `/ts-bids` fetch implementation with:

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
    nurl?: string
    burl?: string
  }

  type TsWindow = Window & {
    __ts_ad_slots?: TsAdSlot[]
    __ts_bids?: Record<string, TsBidData>
    __tsAdInit?: () => void
  }

  /**
   * Install `window.__tsAdInit`.
   *
   * Reads `window.__ts_ad_slots` (injected at head-open) and `window.__ts_bids`
   * (injected before </body>) synchronously — no fetch, no Promise. Applies bid
   * targeting to GPT slots, sets the `ts_initial` sentinel, registers
   * `slotRenderEnded` to fire both nurl and burl via sendBeacon when our
   * specific Prebid bid wins the GAM line item match, then calls refresh().
   */
  export function installTsAdInit(): void {
    const w = window as TsWindow
    w.__tsAdInit = function () {
      const slots = w.__ts_ad_slots ?? []
      const bids = w.__ts_bids ?? {}
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
            const bid = bids[slot.id] ?? {}
            ;(['hb_pb', 'hb_bidder', 'hb_adid'] as const).forEach((key) => {
              if (bid[key]) gptSlot.setTargeting(key, bid[key]!)
            })
            gptSlot.setTargeting('ts_initial', '1')
            return { id: slot.id, gptSlot }
          })
          .filter(Boolean) as Array<{
          id: string
          gptSlot: NonNullable<ReturnType<typeof g.defineSlot>>
        }>

        g.pubads().enableSingleRequest()
        g.enableServices()

        g.pubads().addEventListener?.('slotRenderEnded', (event: any) => {
          const slotId: string = event.slot?.getSlotElementId?.() ?? ''
          const bid = bids[slotId] ?? {}
          const ourBidWon =
            !event.isEmpty &&
            bid.hb_adid &&
            event.slot?.getTargeting?.('hb_adid')?.[0] === bid.hb_adid
          if (ourBidWon) {
            if (bid.nurl) navigator.sendBeacon(bid.nurl)
            if (bid.burl) navigator.sendBeacon(bid.burl)
          }
        })

        g.pubads().refresh()
      })
    }
  }
  ```

- [ ] **Step 3: Add lazy slim-Prebid loader (post-`window.load`)**

  After `installTsAdInit`, add:

  ```typescript
  /**
   * Register the slim-Prebid lazy loader. Fires after window.load — off the
   * critical path. slim-Prebid handles refresh auctions and userID module
   * warm-up (ID5, sharedID, LiveRamp ATS, Lockr). It skips initial-render slots
   * (ts_initial=1) and registers as the GPT refresh handler for scroll/sticky auctions.
   *
   * Phase 1: no-op unless window.__tsjs_slim_prebid_url is set (it won't be until
   * the slim-Prebid bundle build target ships in a later phase).
   */
  export function installSlimPrebidLoader(): void {
    const url = (window as any).__tsjs_slim_prebid_url as string | undefined
    if (!url) return
    window.addEventListener('load', () => {
      const script = document.createElement('script')
      script.src = url
      script.defer = true
      document.head.appendChild(script)
    })
  }
  ```

  Call `installTsAdInit()` from the integration's existing initialization path — wherever the module's init function runs at page load (look for the existing `init()` or module-level call that sets up the GPT integration). Add:

  ```typescript
  // In the integration's init / module entry point:
  installTsAdInit()
  ```

  `window.__tsAdInit()` itself is called by `__tsAdInit` being invoked from the `<script>` block at the bottom of `</body>` (the same script block that calls `window.__tsAdInit()` after the browser receives it).

- [ ] **Step 4: Run JS tests**

  Run: `cd crates/js/lib && npx vitest run`
  Expected: new tests pass

- [ ] **Step 5: Build JS bundle**

  Run: `cd crates/js/lib && node build-all.mjs`
  Expected: clean build

- [ ] **Step 6: Commit**

  ```bash
  git add crates/js/lib/src/integrations/gpt/
  git commit -m "Add synchronous __tsAdInit (reads window.__ts_bids inline); nurl+burl from slotRenderEnded; slim-Prebid lazy loader"
  ```

---

## Task 11: End-to-end integration tests

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (test module)

Tests use `pub(crate)` helpers from Task 8 directly.

- [ ] **Step 1: Write tests**

  In `publisher.rs` test module:

  ```rust
  #[cfg(test)]
  mod creative_opportunities_tests {
      use super::{build_ad_slots_script, build_bids_script, build_bid_map, html_escape_for_script};
      use crate::creative_opportunities::{
          CreativeOpportunitiesConfig, CreativeOpportunitySlot, CreativeOpportunityFormat,
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
      fn ad_slots_script_contains_slot_data() {
          let slots = vec![make_slot()];
          let config = make_config();
          let script = build_ad_slots_script(&slots, &config);
          assert!(script.contains("window.__ts_ad_slots=JSON.parse"), "should use JSON.parse");
          assert!(script.contains("atf_sidebar_ad"), "should include slot id");
          assert!(!script.contains("__ts_bids"), "must NOT contain bids");
          assert!(!script.contains("__ts_request_id"), "must NOT contain request_id");
      }

      #[test]
      fn ad_slots_script_is_xss_safe() {
          let slots = vec![make_slot()];
          let config = make_config();
          let script = build_ad_slots_script(&slots, &config);
          let inner = script
              .trim_start_matches("<script>")
              .trim_end_matches("</script>");
          assert!(!inner.contains('<'), "no unescaped < in script content");
          assert!(!inner.contains('>'), "no unescaped > in script content");
      }

      #[test]
      fn bid_map_includes_nurl_and_burl() {
          let mut winning_bids = HashMap::new();
          winning_bids.insert("atf_sidebar_ad".to_string(), Bid {
              slot_id: "atf_sidebar_ad".to_string(),
              price: Some(2.53),
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "kargo".to_string(),
              width: 300,
              height: 250,
              nurl: Some("https://ssp/win".to_string()),
              burl: Some("https://ssp/bill".to_string()),
              ad_id: Some("abc123".to_string()),
              metadata: Default::default(),
          });
          let map = build_bid_map(&winning_bids, PriceGranularity::Dense);
          let entry = map.get("atf_sidebar_ad").expect("should have bid entry");
          assert_eq!(entry.get("hb_pb").and_then(|v| v.as_str()), Some("2.50"));
          assert_eq!(entry.get("hb_bidder").and_then(|v| v.as_str()), Some("kargo"));
          assert_eq!(entry.get("hb_adid").and_then(|v| v.as_str()), Some("abc123"));
          assert_eq!(entry.get("nurl").and_then(|v| v.as_str()), Some("https://ssp/win"));
          assert_eq!(entry.get("burl").and_then(|v| v.as_str()), Some("https://ssp/bill"));
      }

      #[test]
      fn bid_map_excludes_slot_when_price_is_none() {
          let mut winning_bids = HashMap::new();
          winning_bids.insert("no-price-slot".to_string(), Bid {
              slot_id: "no-price-slot".to_string(),
              price: None,
              currency: "USD".to_string(),
              creative: None,
              adomain: None,
              bidder: "kargo".to_string(),
              width: 300,
              height: 250,
              nurl: None,
              burl: None,
              ad_id: None,
              metadata: Default::default(),
          });
          let map = build_bid_map(&winning_bids, PriceGranularity::Dense);
          assert!(map.is_empty(), "slot with no price should be excluded from bid map");
      }

      #[test]
      fn bids_script_is_xss_safe() {
          let mut map = serde_json::Map::new();
          map.insert("atf".to_string(), serde_json::json!({"hb_pb": "1.00"}));
          let script = build_bids_script(&map);
          let inner = script
              .trim_start_matches("<script>")
              .trim_end_matches("</script>");
          assert!(!inner.contains('<'), "no unescaped < in bids script");
          assert!(!inner.contains('>'), "no unescaped > in bids script");
      }

      #[test]
      fn html_escape_encodes_special_chars() {
          assert_eq!(html_escape_for_script("<script>"), "\\u003cscript\\u003e");
          assert_eq!(html_escape_for_script("&"), "\\u0026");
          assert_eq!(html_escape_for_script("\u{2028}"), "\\u2028");
      }
  }
  ```

- [ ] **Step 2: Run tests**

  Run: `cargo test -p trusted-server-core creative_opportunities_tests`
  Expected: all pass

- [ ] **Step 3: Run full workspace tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 4: Run JS tests**

  Run: `cd crates/js/lib && npx vitest run`
  Expected: all pass

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Add end-to-end publisher helper tests for body-injection architecture"
  ```
