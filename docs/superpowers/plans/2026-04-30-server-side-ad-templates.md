# Server-Side Ad Templates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable the Fastly edge to fire a full header-bidding auction (PBS + APS) in parallel with the origin fetch, injecting `window.__ts_ad_slots` and `window.__ts_bids` into `<head>` so the browser can drive GPT directly without downloading Prebid.js.

**Architecture:** A new `creative-opportunities.toml` file holds per-URL slot templates. At request time, the publisher path matches the URL, fires the auction and origin fetch in parallel via `send_async()`, then injects two `<script>` globals into `<head>` — one from config at head-open and one from auction results just before `</head>` via `lol_html`'s `el.on_end_tag()` (registered inside the single existing `element!("head", ...)` handler).

**Tech Stack:** Rust 2024, `lol_html` 2.7.2 (existing dep), `glob` crate (new workspace dep), `serde`/`toml` (existing), `AuctionOrchestrator::run_auction` (existing `async fn`), TypeScript for GPT shim extension.

---

## File Map

### New files

| File                                                       | Responsibility                                                                              |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| `creative-opportunities.toml`                              | Slot template definitions (page patterns, formats, floor prices, per-provider params)       |
| `crates/trusted-server-core/src/creative_opportunities.rs` | Config types, TOML parsing, URL glob matching, slot→`AdSlot` conversion, startup validation |
| `crates/trusted-server-core/src/price_bucket.rs`           | Prebid price granularity tables; converts `f64` CPM to `hb_pb` string                       |

### Modified files

| File                                                    | Change summary                                                                                                                                                      |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                                            | Add `glob = "0.3"` to `[workspace.dependencies]`                                                                                                                    |
| `crates/trusted-server-core/Cargo.toml`                 | Add `glob = { workspace = true }`                                                                                                                                   |
| `crates/trusted-server-core/src/auction/types.rs`       | Add `MediaType::banner()` constructor; add `ad_id: Option<String>` to `Bid`                                                                                         |
| `crates/trusted-server-core/src/settings.rs`            | Add `creative_opportunities: Option<CreativeOpportunitiesConfig>` to `Settings`                                                                                     |
| `trusted-server.toml`                                   | Add `[creative_opportunities]` section                                                                                                                              |
| `crates/trusted-server-core/build.rs`                   | Validate slot IDs at build time using inline regex (no module import)                                                                                               |
| `crates/trusted-server-core/src/html_processor.rs`      | Add `ad_slots_script`/`ad_bids_script` to `HtmlProcessorConfig`; inject at head-open and via `on_end_tag` before `</head>` (single `element!("head", ...)` handler) |
| `crates/trusted-server-core/src/publisher.rs`           | Convert `handle_publisher_request` to `async fn`; add `orchestrator` param; fire auction + origin in parallel; build injection scripts                              |
| `crates/trusted-server-adapter-fastly/src/main.rs`      | Await the now-async handler; pass orchestrator reference                                                                                                            |
| `crates/trusted-server-core/src/integrations/gpt.rs`    | Extend `head_inserts()` to emit `__tsAdInit` function definition                                                                                                    |
| `crates/js/lib/src/integrations/gpt/index.ts`           | Add `__tsAdInit` implementation and `slotRenderEnded` burl-firing logic                                                                                             |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Add `fire_nurl_at_edge` config key; fire nurl fire-and-forget after auction                                                                                         |

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

The `hb_pb` value in `__ts_bids` is a discretized bucket string from Prebid's granularity tables. "Dense" is the default used in most Prebid deployments.

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
          // Per Prebid spec, "auto" uses dense granularity (same table).
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

`CreativeOpportunityFormat` uses `#[serde(default = "MediaType::banner")]` which requires a free function `MediaType::banner() -> Self`. The `hb_adid` key in `__ts_bids` is Prebid's ad UUID — not the creative markup. Add `ad_id: Option<String>` to `Bid` so parsers can populate it from `bid.adId` in the OpenRTB response extension, and set `"hb_adid"` from this field.

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

  Add `ad_id: Option<String>` field to `Bid`:

  ```rust
  pub struct Bid {
      // ... existing fields ...
      /// Prebid-assigned ad identifier (e.g. `bid.adId` in OpenRTB extension).
      /// Used to populate `hb_adid` GAM targeting key.
      pub ad_id: Option<String>,
  }
  ```

  Update the `make_bid` test helper to include `ad_id: None`.

- [ ] **Step 3: Run tests**

  Run: `cargo test -p trusted-server-core auction`
  Expected: all tests pass

- [ ] **Step 4: Update prebid.rs to populate `ad_id`**

  In `crates/trusted-server-core/src/integrations/prebid.rs`, in the `Bid` construction around line 987, find where `nurl` and `burl` are set and add:

  ```rust
  ad_id: bid_obj.get("adId")
      .or_else(|| bid_obj.get("id"))
      .and_then(|v| v.as_str())
      .map(String::from),
  ```

  (Prebid Server returns `adId` in its extensions; fall back to `id` if absent.)

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
      ///
      /// APS bidder params from `[slot.providers.aps]` are wired into
      /// `AdSlot.bidders["aps"]` so the APS provider can read them.
      /// PBS resolves its bidder params from stored requests by slot ID —
      /// no `bidders["prebid"]` entry needed.
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
  ///
  /// # Errors
  ///
  /// Returns an error if the ID is empty or contains disallowed characters.
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

Build-time validation uses inline `regex` (already a build dep) rather than importing `creative_opportunities.rs` — importing that module in `build.rs` would require resolving its `crate::auction::types` deps, which don't exist in the build context.

- [ ] **Step 1: Add slot-ID validation to `build.rs`**

  In `build.rs`, add after the existing settings validation:

  ```rust
  // Validate creative-opportunities.toml slot IDs at build time.
  // We parse the TOML directly here rather than importing creative_opportunities.rs,
  // because that module depends on crate::auction::types which is unavailable in build.rs.
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

- [ ] **Step 2: Add `regex` to build-dependencies (if not already present)**

  Check `crates/trusted-server-core/Cargo.toml` `[build-dependencies]` section — `regex` is already listed. If not, add `regex = { workspace = true }`.

- [ ] **Step 3: Run build to verify**

  Run: `cargo build --package trusted-server-core`
  Expected: builds, warning `creative-opportunities.toml: 1 slot(s) validated`

- [ ] **Step 4: Test the guard**

  Temporarily add a bad slot to `creative-opportunities.toml`:

  ```toml
  [[slot]]
  id = "bad slot!"
  page_patterns = ["/"]
  formats = []
  ```

  Run: `cargo build --package trusted-server-core`
  Expected: panics with invalid slot ID message. Revert the change.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/build.rs
  git commit -m "Validate creative-opportunities.toml slot IDs at build time using inline TOML parse"
  ```

---

## Task 7: HTML processor — `HtmlProcessorConfig` fields (before publisher restructuring)

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`

Adding the two new fields to `HtmlProcessorConfig` and the injection logic is independent of the publisher async restructuring. Do this first so Task 8 can reference the completed fields.

- [ ] **Step 1: Write failing tests for injection**

  In `html_processor.rs` tests:

  ```rust
  #[test]
  fn injects_ad_slots_before_head_inserts() {
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: Some(
              "<script>window.__ts_ad_slots=JSON.parse(\"[]\");</script>".to_string()
          ),
          ad_bids_script: None,
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head><title>T</title></head><body></body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("window.__ts_ad_slots"), "should inject ad slots");
  }

  #[test]
  fn injects_bids_before_end_of_head() {
      let bids_script = "<script>window.__ts_bids=JSON.parse(\"{}\");</script>";
      let config = HtmlProcessorConfig {
          origin_host: "origin.example.com".to_string(),
          request_host: "example.com".to_string(),
          request_scheme: "https".to_string(),
          integrations: IntegrationRegistry::empty_for_tests(),
          ad_slots_script: None,
          ad_bids_script: Some(bids_script.to_string()),
      };
      let mut processor = create_html_processor(config);
      let output = processor
          .process_chunk(b"<html><head><title>T</title></head><body></body></html>", true)
          .expect("should process");
      let html = std::str::from_utf8(&output).expect("should be utf8");
      assert!(html.contains("window.__ts_bids"), "should inject bids");
      let bids_pos = html.find("window.__ts_bids").expect("should find bids");
      let end_head_pos = html.find("</head>").expect("should find </head>");
      assert!(bids_pos < end_head_pos, "bids script should appear before </head>");
  }
  ```

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: compile error (no `ad_slots_script`/`ad_bids_script` fields, no `empty_for_tests()`)

- [ ] **Step 2: Add `empty_for_tests()` to `IntegrationRegistry`**

  In `registry.rs`, add:

  ```rust
  #[cfg(test)]
  impl IntegrationRegistry {
      pub fn empty_for_tests() -> Self {
          // Minimal registry with no integrations for unit testing html_processor
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

- [ ] **Step 3: Add fields to `HtmlProcessorConfig`**

  ```rust
  pub struct HtmlProcessorConfig {
      pub origin_host: String,
      pub request_host: String,
      pub request_scheme: String,
      pub integrations: IntegrationRegistry,
      /// Pre-computed `<script>window.__ts_ad_slots=...</script>` for matched slots.
      /// Injected at <head> open, before integration head inserts. `None` when no slots matched.
      pub ad_slots_script: Option<String>,
      /// Pre-computed `<script>window.__ts_bids=...</script>` for winning bids.
      /// Injected immediately before </head> via on_end_tag(). `None` when auction not run.
      pub ad_bids_script: Option<String>,
  }
  ```

  Update `from_settings` to initialize `ad_slots_script: None, ad_bids_script: None`.

- [ ] **Step 4: Inject `__ts_ad_slots` at head-open AND register `on_end_tag` for `__ts_bids`**

  In `create_html_processor`, within the EXISTING single `element!("head", ...)` handler, make two changes:
  1. Prepend the ad slots script BEFORE the existing integration inserts:

     ```rust
     // NEW: inject __ts_ad_slots first
     if let Some(ref slots_script) = ad_slots_script {
         snippet.push_str(slots_script);
     }
     // ... existing: for insert in integrations.head_inserts(&ctx) { ... }
     ```

  2. After `el.prepend(...)`, register the end-tag handler for `__ts_bids`:
     ```rust
     // Register on_end_tag handler for __ts_bids injection before </head>
     if let Some(bids_script) = ad_bids_script.clone() {
         el.on_end_tag(move |end_tag| {
             end_tag.before(&bids_script, ContentType::Html);
             Ok(())
         })?;
     }
     ```

  Both changes live inside the same `element!("head", ...)` closure — no second handler needed.

  Capture `ad_slots_script` and `ad_bids_script` into the closure the same way as `injected_tsjs`:

  ```rust
  let ad_slots_script = config.ad_slots_script.clone();
  let ad_bids_script = config.ad_bids_script.clone();
  ```

  > **lol_html `on_end_tag` API note:** `Element::on_end_tag(handler)` is available in lol_html ≥2.0. The handler receives `&mut EndTag` and must return `Result<(), Box<dyn Error + Send + Sync>>`. Use `ContentType::Html` so the injected `<script>` block is parsed as HTML, not text. The handler must be registered in the opening-tag handler (i.e. inside `element!("head", ...)`).

- [ ] **Step 5: Run tests**

  Run: `cargo test -p trusted-server-core html_processor`
  Expected: all tests pass

- [ ] **Step 6: Run full suite**

  Run: `cargo test --workspace`
  Expected: clean

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-core/src/html_processor.rs \
          crates/trusted-server-core/src/integrations/registry.rs
  git commit -m "Add ad_slots/bids injection to HtmlProcessorConfig; register on_end_tag for bids"
  ```

---

## Task 8: `handle_publisher_request` async restructuring

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

The `HtmlProcessorConfig` fields now exist (Task 7). This task wires the auction into the publisher path and populates them.

- [ ] **Step 1: Update function signature**

  Change `handle_publisher_request` in `publisher.rs`:

  ```rust
  // Before:
  pub fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      services: &RuntimeServices,
      mut req: Request,
  ) -> Result<PublisherResponse, Report<TrustedServerError>>

  // After:
  pub async fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      services: &RuntimeServices,
      orchestrator: &crate::auction::orchestrator::AuctionOrchestrator,
      slots_file: &crate::creative_opportunities::CreativeOpportunitiesFile,
      mut req: Request,
  ) -> Result<PublisherResponse, Report<TrustedServerError>>
  ```

  Add imports:

  ```rust
  use crate::auction::orchestrator::AuctionOrchestrator;
  use crate::auction::types::{AuctionContext, AuctionRequest, PublisherInfo, UserInfo, SiteInfo};
  use crate::creative_opportunities::{CreativeOpportunitiesFile, match_slots};
  use crate::price_bucket::price_bucket;
  ```

- [ ] **Step 2: Match URL and fire async origin fetch**

  Replace the blocking `.send()` call with async fire + URL matching:

  ```rust
  // Match URL against slot templates
  let request_path = req.get_path().to_string();
  let matched_slots: Vec<_> = if settings.creative_opportunities.is_some() {
      match_slots(&slots_file.slots, &request_path)
          .into_iter()
          .cloned()
          .collect()
  } else {
      Vec::new()
  };

  // Consent gate: spec §4.3 uses TCF Purpose 1 (storage access).
  let consent_allows_auction = consent_context
      .tcf
      .as_ref()
      .map_or(false, |tcf| tcf.has_purpose_consent(1));
  let should_run_auction = !matched_slots.is_empty() && consent_allows_auction;

  // Determine effective auction timeout
  let auction_timeout_ms = settings
      .creative_opportunities
      .as_ref()
      .and_then(|co| co.auction_timeout_ms)
      .unwrap_or(settings.auction.timeout_ms);

  restrict_accept_encoding(&mut req);
  req.set_header("host", &origin_host);

  // Clone request data needed for auction before send_async consumes req.
  // Fastly's send_async takes ownership of the Request; providers that need
  // User-Agent/IP/geo should read from `services.client_info` (already present
  // in AuctionContext), not from AuctionContext.request.
  // NOTE: AuctionContext.request is currently a required reference. Pass a
  // minimal placeholder Request here. Providers must not rely on it for
  // correctness; see Known Limitations.
  let placeholder_req = fastly::Request::new();

  // Fire origin async — consumes req
  let pending_origin = req
      .send_async(&backend_name)
      .change_context(TrustedServerError::Proxy {
          message: "Failed to dispatch async origin request".to_string(),
      })?;

  // Fire auction in parallel (if applicable)
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

  // Await origin response
  let mut response = pending_origin
      .wait()
      .change_context(TrustedServerError::Proxy {
          message: "Failed to await origin response".to_string(),
      })?;
  ```

- [ ] **Step 3: Build injection scripts and set cache header**

  After acquiring `response`:

  ```rust
  let (ad_slots_script, ad_bids_script) = if let Some(co_config) = &settings.creative_opportunities {
      if !matched_slots.is_empty() {
          let slots_script = build_ad_slots_script(&matched_slots, co_config);
          let bids_script = auction_result.as_ref().map(|result| {
              build_ad_bids_script(&result.winning_bids, co_config.price_granularity)
          });
          (Some(slots_script), bids_script)
      } else {
          (None, None)
      }
  } else {
      (None, None)
  };

  // Cache: any response with bid data must not be cached by CDN or browser.
  if ad_bids_script.is_some() {
      response.set_header(header::CACHE_CONTROL, "private, no-store");
      response.remove_header("surrogate-control");
      response.remove_header("fastly-surrogate-control");
  }
  ```

- [ ] **Step 4: Add `pub(crate)` helper functions**

  These are `pub(crate)` so the integration tests in Task 10 can call them:

  ```rust
  pub(crate) fn build_ad_slots_script(
      matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
      co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
  ) -> String {
      let slots_json: Vec<_> = matched_slots.iter().map(|slot| {
          serde_json::json!({
              "id": slot.id,
              "gam_unit_path": slot.resolved_gam_unit_path(&co_config.gam_network_id),
              "div_id": slot.resolved_div_id(),
              "formats": slot.formats.iter().map(|f| serde_json::json!([f.width, f.height])).collect::<Vec<_>>(),
              "targeting": slot.targeting,
          })
      }).collect();
      let json = serde_json::to_string(&slots_json).expect("should serialize ad slots");
      let escaped = html_escape_for_script(&json);
      format!("<script>window.__ts_ad_slots=JSON.parse(\"{}\");</script>", escaped)
  }

  pub(crate) fn build_ad_bids_script(
      winning_bids: &std::collections::HashMap<String, crate::auction::types::Bid>,
      price_granularity: crate::price_bucket::PriceGranularity,
  ) -> String {
      let bids_map: serde_json::Map<String, serde_json::Value> = winning_bids
          .iter()
          .filter_map(|(slot_id, bid)| {
              let cpm = bid.price?;
              let entry = serde_json::json!({
                  "hb_pb": price_bucket(cpm, price_granularity),
                  "hb_bidder": bid.bidder,
                  "hb_adid": bid.ad_id.as_deref().unwrap_or(""),
                  "burl": bid.burl,
              });
              Some((slot_id.clone(), entry))
          })
          .collect();
      let json = serde_json::to_string(&serde_json::Value::Object(bids_map))
          .expect("should serialize bids");
      let escaped = html_escape_for_script(&json);
      format!("<script>window.__ts_bids=JSON.parse(\"{}\");</script>", escaped)
  }

  /// HTML-escape a JSON string for safe inline `<script>` injection.
  ///
  /// JSON is embedded in a double-quoted JS string literal.
  /// We escape `"` → `\"` (already done by serde_json), and Unicode-escape
  /// `<`, `>`, `&`, line separator (U+2028), paragraph separator (U+2029)
  /// to prevent script injection and HTML parse-level issues.
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

- [ ] **Step 5: Thread scripts into `OwnedProcessResponseParams`**

  Add fields to `OwnedProcessResponseParams`:

  ```rust
  pub struct OwnedProcessResponseParams {
      // existing fields...
      pub(crate) ad_slots_script: Option<String>,
      pub(crate) ad_bids_script: Option<String>,
  }
  ```

  In the `Stream` arm that constructs `OwnedProcessResponseParams`, pass the values through.

  Update `create_html_stream_processor` signature to accept and forward these:

  ```rust
  fn create_html_stream_processor(
      origin_host: &str,
      request_host: &str,
      request_scheme: &str,
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      ad_slots_script: Option<String>,  // NEW
      ad_bids_script: Option<String>,   // NEW
  ) -> Result<impl StreamProcessor, Report<TrustedServerError>>
  ```

  In `HtmlProcessorConfig::from_settings` (or constructing `HtmlProcessorConfig` directly), set:

  ```rust
  ad_slots_script,
  ad_bids_script,
  ```

- [ ] **Step 6: Update `main.rs` call site**

  In `crates/trusted-server-adapter-fastly/src/main.rs`, update the `handle_publisher_request` call:

  ```rust
  match handle_publisher_request(
      settings,
      integration_registry,
      &publisher_services,
      orchestrator,   // NEW — reference to AuctionOrchestrator
      slots_file,     // NEW — reference to CreativeOpportunitiesFile loaded at startup
      req,
  ).await {           // NEW — .await
      // ... existing match arms unchanged
  }
  ```

  **Loading `orchestrator` and `slots_file` at startup:** In `main.rs`, find where `AuctionOrchestrator::new()` is called (it's used already for the `/auction` endpoint). Pass the same instance here. For `slots_file`, add startup loading:

  ```rust
  // At startup, before the request-handling loop:
  const CREATIVE_OPPORTUNITIES_TOML: &str =
      include_str!("../../../creative-opportunities.toml");

  let slots_file: creative_opportunities::CreativeOpportunitiesFile =
      toml::from_str(CREATIVE_OPPORTUNITIES_TOML)
          .expect("should parse creative-opportunities.toml");
  ```

  The `include_str!()` path is relative to the adapter crate's `src/main.rs`. Adjust the path accordingly.

- [ ] **Step 7: Compile check**

  Run: `cargo check --workspace`
  Expected: clean compile

- [ ] **Step 8: Run full tests**

  Run: `cargo test --workspace`
  Expected: all pass

- [ ] **Step 9: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs \
          crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Convert handle_publisher_request to async; parallel auction + origin fetch"
  ```

---

## Task 9: GPT head injector — emit `__tsAdInit`

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`

- [ ] **Step 1: Write failing test**

  ```rust
  #[test]
  fn head_inserts_includes_ts_ad_init() {
      let config = test_config();
      let integration = GptIntegration::new(config);
      let ctx = make_test_context();
      let inserts = integration.head_inserts(&ctx);
      let combined = inserts.join("");
      assert!(combined.contains("__tsAdInit"), "should define __tsAdInit");
      assert!(combined.contains("googletag.cmd.push"), "should use googletag");
      assert!(combined.contains("slotRenderEnded"), "should register slotRenderEnded");
      assert!(combined.contains("sendBeacon"), "should fire burl via sendBeacon");
  }
  ```

  Run: `cargo test -p trusted-server-core integrations::gpt`
  Expected: FAIL — `__tsAdInit` not in output

- [ ] **Step 2: Extend `head_inserts()` in gpt.rs**

  ```rust
  impl IntegrationHeadInjector for GptIntegration {
      fn integration_id(&self) -> &'static str {
          GPT_INTEGRATION_ID
      }

      fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
          vec![
              // Enable flag and shim bootstrap (existing)
              "<script>window.__tsjs_gpt_enabled=true;\
               window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>"
                  .to_string(),
              // __tsAdInit definition — reads window.__ts_ad_slots / __ts_bids at call time.
              concat!(
                  "<script>",
                  "window.__tsAdInit=function(){",
                    "var slots=window.__ts_ad_slots||[];",
                    "var bids=window.__ts_bids||{};",
                    "googletag.cmd.push(function(){",
                      "slots.forEach(function(slot){",
                        "var s=googletag.defineSlot(slot.gam_unit_path,slot.formats,slot.div_id);",
                        "if(!s)return;",
                        "s.addService(googletag.pubads());",
                        "Object.entries(slot.targeting||{}).forEach(function(e){s.setTargeting(e[0],e[1]);});",
                        "var b=bids[slot.id]||{};",
                        "[\"hb_pb\",\"hb_bidder\",\"hb_adid\"].forEach(function(k){if(b[k])s.setTargeting(k,b[k]);});",
                      "});",
                      "googletag.pubads().enableSingleRequest();",
                      "googletag.enableServices();",
                      "googletag.pubads().addEventListener(\"slotRenderEnded\",function(ev){",
                        "var id=ev.slot.getSlotElementId();",
                        "var b=bids[id]||{};",
                        "if(!ev.isEmpty&&b.burl&&ev.slot.getTargeting(\"hb_adid\")[0]===b.hb_adid){",
                          "navigator.sendBeacon(b.burl);",
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
  git commit -m "Emit __tsAdInit function definition from GPT head injector"
  ```

---

## Task 10: `gpt/index.ts` — TypeScript `__tsAdInit`

**Files:**

- Modify: `crates/js/lib/src/integrations/gpt/index.ts`

The TypeScript version is the authoritative implementation; it must mirror the Rust inline string from Task 9 exactly.

- [ ] **Step 1: Write a failing test**

  In `crates/js/lib/src/integrations/gpt/index.test.ts`:

  ```typescript
  import { describe, it, expect, vi, beforeEach } from 'vitest'

  describe('installTsAdInit', () => {
    beforeEach(() => {
      delete (window as any).__ts_ad_slots
      delete (window as any).__ts_bids
      delete (window as any).__tsAdInit
    })

    it('defines googletag slots from __ts_ad_slots and calls refresh', () => {
      const mockSlot = {
        addService: vi.fn().mockReturnThis(),
        setTargeting: vi.fn().mockReturnThis(),
      }
      const mockPubads = {
        enableSingleRequest: vi.fn(),
        addEventListener: vi.fn(),
        refresh: vi.fn(),
        getTargeting: vi.fn().mockReturnValue([]),
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
          burl: 'https://ssp/bill',
        },
      }

      // Must import installTsAdInit from the module
      const { installTsAdInit } = require('./index')
      installTsAdInit()
      ;(window as any).__tsAdInit()

      expect((window as any).googletag.defineSlot).toHaveBeenCalledWith(
        '/123/atf',
        [[300, 250]],
        'atf'
      )
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_pb', '1.00')
      expect(mockSlot.setTargeting).toHaveBeenCalledWith('hb_bidder', 'kargo')
      expect(mockPubads.refresh).toHaveBeenCalled()
    })

    it('fires burl via sendBeacon on slotRenderEnded when our bid won', () => {
      const beaconSpy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      // ... setup and trigger slotRenderEnded event
      // Verify: navigator.sendBeacon called with burl
      beaconSpy.mockRestore()
    })
  })
  ```

  Run: `cd crates/js/lib && npx vitest run`
  Expected: FAIL — `installTsAdInit` not exported

- [ ] **Step 2: Add `installTsAdInit` to `index.ts`**

  Add to `crates/js/lib/src/integrations/gpt/index.ts` (bottom of file):

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
    __ts_bids?: Record<string, TsBidData>
    __tsAdInit?: () => void
  }

  /**
   * Install `window.__tsAdInit` — reads `window.__ts_ad_slots` and `window.__ts_bids`
   * (injected by the edge into <head>), defines GPT slots, applies pre-won bid targeting,
   * registers a `slotRenderEnded` listener to fire `burl` via `sendBeacon`, then calls
   * `refresh()`.
   */
  export function installTsAdInit(): void {
    const w = window as TsWindow
    w.__tsAdInit = function () {
      const slots = w.__ts_ad_slots ?? []
      const bids = w.__ts_bids ?? {}
      const g = (window as GptWindow).googletag
      if (!g) return
      g.cmd.push(() => {
        slots.forEach((slot) => {
          const gptSlot = g.defineSlot?.(
            slot.gam_unit_path,
            slot.formats,
            slot.div_id
          )
          if (!gptSlot) return
          gptSlot.addService(g.pubads())
          Object.entries(slot.targeting ?? {}).forEach(([k, v]) =>
            gptSlot.setTargeting(k, v)
          )
          const bid = bids[slot.id] ?? {}
          ;(['hb_pb', 'hb_bidder', 'hb_adid'] as const).forEach((key) => {
            if (bid[key]) gptSlot.setTargeting(key, bid[key]!)
          })
        })
        g.pubads().enableSingleRequest()
        g.enableServices()
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
    }
  }
  ```

  Call `installTsAdInit()` from the integration's initialization path so it's set up when the bundle loads.

- [ ] **Step 3: Run JS tests**

  Run: `cd crates/js/lib && npx vitest run`
  Expected: new tests pass

- [ ] **Step 4: Build JS bundle**

  Run: `cd crates/js/lib && node build-all.mjs`
  Expected: clean build

- [ ] **Step 5: Commit**

  ```bash
  git add crates/js/lib/src/integrations/gpt/
  git commit -m "Add __tsAdInit and slotRenderEnded burl firing to GPT integration"
  ```

---

## Task 11: `nurl` fire-and-forget

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

- [ ] **Step 3: Fire nurls in publisher.rs after auction**

  After `auction_result` is obtained, add:

  ```rust
  if let Some(ref result) = auction_result {
      fire_winning_nurls(result, settings);
  }
  ```

  Add helper (no `.await` — fire-and-forget):

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

## Task 12: End-to-end integration tests

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` (test module)

Tests use `pub(crate)` helpers from Task 8 directly.

- [ ] **Step 1: Write tests**

  In `publisher.rs` test module:

  ```rust
  #[cfg(test)]
  mod creative_opportunities_tests {
      use super::{build_ad_slots_script, build_ad_bids_script, html_escape_for_script};
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
      fn ad_slots_script_is_safe_and_parseable() {
          let slots = vec![make_slot()];
          let config = make_config();
          let script = build_ad_slots_script(&slots, &config);
          assert!(script.contains("window.__ts_ad_slots=JSON.parse"), "should use JSON.parse");
          assert!(script.contains("atf_sidebar_ad"), "should include slot id");
          // Verify no raw < or > that could break HTML parser
          let inner = script.trim_start_matches("<script>").trim_end_matches("</script>");
          assert!(!inner.contains('<'), "no unescaped < in script content");
          assert!(!inner.contains('>'), "no unescaped > in script content");
      }

      #[test]
      fn ad_bids_script_uses_price_bucket_and_ad_id() {
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
          let script = build_ad_bids_script(&winning_bids, PriceGranularity::Dense);
          assert!(script.contains("\"hb_pb\":\"2.53\""), "should bucket 2.53 as 2.53 (dense)");
          assert!(script.contains("\"hb_bidder\":\"kargo\""), "should include bidder");
          assert!(script.contains("\"hb_adid\":\"prebid-uuid-abc123\""), "should use ad_id not creative markup");
          assert!(script.contains("burl"), "should include burl for billing");
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
  git commit -m "Add integration tests for creative opportunities pipeline (slots, bids, XSS)"
  ```

---

## Manual Verification Checklist

Run `fastly compute serve` and verify:

- [ ] **No match:** Request `/about` — no `__ts_ad_slots` or `__ts_bids` in response HTML, no `Cache-Control: private, no-store`
- [ ] **Match:** Request `/2024/01/article` — both globals present in `<head>`, `Cache-Control: private, no-store` set
- [ ] **Empty file kill-switch:** Empty `creative-opportunities.toml` → no globals injected on any URL
- [ ] **Auction timeout:** Set `auction_timeout_ms = 1` → `__ts_bids` injects as `{}`, no slot entries
- [ ] **XSS check:** Add `targeting = { zone = "</script><script>alert(1)//" }` to a slot → verify escaped output in HTML source

---

## Known Limitations

| Item                                 | Notes                                                                                                                                                                                                                                                                                                                                       |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AuctionContext.request` placeholder | After `send_async` consumes `req`, a blank `fastly::Request::new()` is passed. Providers that read User-Agent/IP/cookies from this field will degrade silently. Either clone request data before `send_async` or extend `AuctionContext` to drop the `request` field in favor of `client_info`. Must be resolved before production rollout. |
| nurl dynamic backend allowlist       | `fire_winning_nurls` uses `BackendConfig::from_url()`. The SSP domain must be in `fastly.toml`'s dynamic backend origins allowlist, or use Fastly's `allow_dynamic_backends = true`. Confirm with ops before enabling.                                                                                                                      |
| Streaming mode `</head>` hold        | Current `on_end_tag` impl injects whatever bids are available when `</head>` is encountered. In streaming mode (NextJS 16), if the auction finishes before `</head>` this is zero-latency. If the auction is slower than the HTML flush, bids may be empty. A future follow-up can hold the `</head>` boundary for remaining budget.        |
| `burl` absent for APS                | APS bids don't have a `burl`. The `burl` field in `__ts_bids` is `null`/absent for APS slots — the `slotRenderEnded` check correctly short-circuits on `!bidData.burl`.                                                                                                                                                                     |
