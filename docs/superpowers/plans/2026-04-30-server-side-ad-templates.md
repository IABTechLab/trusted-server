# Server-Side Ad Templates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable Trusted Server to match publisher page URLs to preconfigured ad slot templates, start the server-side auction at the edge, stream page HTML without auction delay, and serve bid targeting through `/ts-bids` so GPT can run without Prebid.js.

**Architecture:** Phase 1 is implemented for Fastly Compute only. `creative-opportunities.toml` is compiled into the Fastly binary and contains URL-matched slot templates. A matched page request mints a UUID request ID, injects only `window.__ts_ad_slots` and `window.__ts_request_id` at `<head>` open, and stores auction state/results in a Fastly Core Cache-backed `BidCache` rendezvous keyed by request ID. The auction must never delay the origin response or `</head>` flushing; `/ts-bids?rid=<request_id>` is the only bid delivery path.

**Tech Stack:** Rust 2024, Fastly Compute, Fastly Core Cache, Fastly `PendingRequest::poll`, `lol_html` 2.7.2, `glob`, `serde`/`toml`, `uuid` v4, `std::time::Instant`, existing `AuctionOrchestrator` provider logic, TypeScript GPT shim.

---

## Source of Truth and Invariants

This plan implements `docs/superpowers/specs/2026-04-15-server-side-ad-templates-design.md`. Where older implementation notes conflict with that design, the design wins.

Non-negotiable invariants:

- The auction must not block origin response dispatch, HTML streaming, `</head>` flushing, or FCP.
- No bid data is injected into HTML. Never add `window.__ts_bids`, `ad_bids_script`, or a `</head>` hold.
- The only injected page globals are `window.__ts_ad_slots` and `window.__ts_request_id`.
- `/ts-bids` long-polls against the original `A_deadline = T0 + auction_timeout_ms`.
- Missing or empty `rid` returns `400`; unknown or expired request IDs return `404`; completed no-bid/timeout results return `{}`.
- If slots match but consent is absent or denied, do not fire the auction and do not inject ad globals; still set the browser-facing response to `Cache-Control: private, no-store`.
- Preserve `Surrogate-Control` and `Fastly-Surrogate-Control` unless the feasibility work proves Fastly requires a different cache strategy.
- Use the repo's actual Prebid config type: `PrebidIntegrationConfig`, not `PrebidConfig`.

The April 15 spec has a few stale `__ts_bids` mentions in prose. Treat those as historical wording. Implementation uses `/ts-bids` and never sets `window.__ts_bids`.

---

## Phase 1 Support Boundary

Phase 1 targets Fastly only. Wire the publisher path, `/ts-bids`, auction completion, and `nurl` fire-and-forget in `crates/trusted-server-adapter-fastly` using the Fastly-supported primitive proven in Task 1.

Keep core helpers platform-conscious where that falls out naturally, but do not add a cross-platform `AuctionIntent` abstraction or implement Cloudflare/Axum support in this plan. Until the EdgeZero migration reaches equivalent non-blocking HTTP polling, request rendezvous, and outbound HTTP semantics for other adapters, server-side ad templates may be disabled or return an explicit unsupported response on non-Fastly platforms.

If Task 1 cannot prove the required Fastly behavior, stop implementation. Do not replace the April 15 behavior with a `/ts-bids`-initiated auction or any design where the browser bid request starts the auction.

---

## File Map

### New files

| File                                                                          | Responsibility                                                                         |
| ----------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `creative-opportunities.toml`                                                 | Slot templates, page patterns, formats, floors, targeting, APS params                  |
| `crates/trusted-server-core/src/creative_opportunities.rs`                    | Config types, TOML parsing, URL glob matching, slot-to-`AdSlot` conversion, validation |
| `crates/trusted-server-core/src/price_bucket.rs`                              | Prebid price granularity bucketing for `hb_pb`                                         |
| `crates/trusted-server-core/src/bid_cache.rs`                                 | Platform-neutral bid cache types, state machine, and in-memory test implementation     |
| `crates/trusted-server-adapter-fastly/src/bid_cache.rs`                       | Fastly Core Cache-backed `BidCache` implementation for Phase 1                         |
| `docs/superpowers/reports/2026-04-30-server-side-ad-templates-concurrency.md` | Feasibility proof for the non-blocking auction path                                    |

### Modified files

| File                                                     | Change summary                                                                                   |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `Cargo.toml`                                             | Add `glob = "0.3"` to workspace dependencies                                                     |
| `crates/trusted-server-core/Cargo.toml`                  | Add `glob = { workspace = true }`                                                                |
| `crates/trusted-server-core/src/lib.rs`                  | Export new modules                                                                               |
| `crates/trusted-server-core/src/auction/types.rs`        | Add `MediaType::banner()` and `Bid::ad_id`                                                       |
| `crates/trusted-server-core/src/integrations/prebid.rs`  | Populate `Bid::ad_id`; add `fire_nurl_at_edge` to `PrebidIntegrationConfig`                      |
| `crates/trusted-server-core/src/settings.rs`             | Add `[creative_opportunities]` settings                                                          |
| `crates/trusted-server-core/build.rs`                    | Validate `creative-opportunities.toml` at build time                                             |
| `crates/trusted-server-adapter-fastly/build.rs`          | Rebuild adapter when `creative-opportunities.toml` changes                                       |
| `crates/trusted-server-core/src/html_processor.rs`       | Inject head globals at `<head>` open only                                                        |
| `crates/trusted-server-core/src/platform/http.rs`        | Add non-blocking pending request polling abstraction                                             |
| `crates/trusted-server-core/src/auction/orchestrator.rs` | Add a pollable server-side auction path that can advance without blocking HTML streaming         |
| `crates/trusted-server-core/src/publisher.rs`            | Match slots, prepare head globals, set browser cache headers, dispatch non-blocking auction path |
| `crates/trusted-server-adapter-fastly/src/platform.rs`   | Implement Fastly `PendingRequest::poll` in the platform HTTP client                              |
| `crates/trusted-server-adapter-fastly/src/main.rs`       | Load slot file, initialize Fastly `BidCache`, add `/ts-bids`, pass publisher dependencies        |
| `crates/trusted-server-core/src/integrations/gpt.rs`     | Emit GPT bootstrap that installs `__tsAdInit` with `/ts-bids` fetch                              |
| `crates/js/lib/src/integrations/gpt/index.ts`            | Add TypeScript `installTsAdInit` and burl firing                                                 |
| `trusted-server.toml`                                    | Add `[creative_opportunities]` config                                                            |

---

## Task 1: Feasibility Gate for Non-Blocking Auction Completion

**Files:**

- Create: `docs/superpowers/reports/2026-04-30-server-side-ad-templates-concurrency.md`
- Inspect: `crates/trusted-server-core/src/platform/http.rs`
- Inspect: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Inspect: Fastly crate docs/source for `PendingRequest::poll`, `send_async`, streaming response, and any supported background execution primitive

This task is a stop/go gate. Do not implement the publisher-path auction trigger until this task selects and verifies a Fastly-supported mechanism that lets auction results continue to completion while the page response streams immediately.

This proof is Fastly-only. It does not need to solve Cloudflare, Axum, or any future EdgeZero adapter. Non-Fastly support can remain unsupported/deferred in Phase 1.

Acceptable proof outcomes:

1. A verified background execution primitive exists and can complete `AuctionOrchestrator::run_auction` after response streaming starts.
2. A verified Fastly Core Cache rendezvous plus non-blocking `PendingRequest::poll` design exists that can advance auction pending requests between streaming chunks without delaying HTML chunks.
3. No primitive exists. In that case, stop implementation and update the report with the blocker. Do not replace this with a design that waits for auction completion before streaming.

- [ ] **Step 1: Inspect the runtime primitives**

  Read Fastly and local platform code for `send_async`, `PendingRequest::poll`, `PendingRequest::wait`, `select`, `stream_to_client`, Fastly Core Cache transactions/replacements, and any background execution API.

  Run:

  ```bash
  rg -n "PendingRequest::poll|send_async|select|stream_to_client|background|spawn" \
    crates/trusted-server-core/src crates/trusted-server-adapter-fastly/src \
    ~/.cargo/registry/src
  ```

  Expected: enough evidence to identify whether background completion or non-blocking polling is feasible.

- [ ] **Step 2: Write the feasibility report**

  Create `docs/superpowers/reports/2026-04-30-server-side-ad-templates-concurrency.md` with:

  ```markdown
  # Server-Side Ad Templates Concurrency Feasibility

  ## Selected Primitive

  [Name the Fastly-supported primitive or state "blocked".]

  ## Evidence

  - [File/path and line references inspected]
  - [Small spike, test, or manual Viceroy evidence]

  ## Required Publisher-Path Shape

  - Origin response must be dispatched immediately.
  - HTML streaming must begin as soon as origin response headers are available.
  - Auction completion must write to BidCache without waiting before streaming.
  - /ts-bids must observe pending, complete, empty, and unknown states.
  - BidCache must use Fastly Core Cache or another verified Fastly cross-request primitive; process-global memory is not sufficient because normal Fastly requests start separate Compute instances.
  - Pending auction state must include the original auction deadline so /ts-bids can long-poll against A_deadline without minting a new timeout.
  - Non-Fastly adapters are out of scope for Phase 1 and may remain unsupported.

  ## Stop/Go Decision

  [Go/Blocked]
  ```

- [ ] **Step 3: Verify the response-streaming and rendezvous invariant**

  Build a small spike or route-level test that proves a matched page can emit its first HTML bytes before a deliberately delayed auction result completes, and that `/ts-bids` can observe the same request ID moving from pending to complete through the selected Fastly rendezvous.

  Expected: evidence shows first page bytes are emitted before the delayed auction finishes, and `/ts-bids?rid=<request_id>` sees the completed bid map without relying on process-global memory.

- [ ] **Step 4: Commit the report**

  ```bash
  git add docs/superpowers/reports/2026-04-30-server-side-ad-templates-concurrency.md
  git commit -m "Document concurrency feasibility for server-side ad templates"
  ```

---

## Task 2: Add `glob` Workspace Dependency

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/trusted-server-core/Cargo.toml`

- [ ] **Step 1: Write a temporary failing compile check**

  Temporarily add this to `crates/trusted-server-core/src/lib.rs`:

  ```rust
  use glob::Pattern as _;
  ```

  Run:

  ```bash
  cargo check --package trusted-server-core
  ```

  Expected: compile error because `glob` is not declared yet.

- [ ] **Step 2: Add the workspace dependency**

  In root `Cargo.toml` under `[workspace.dependencies]`, add:

  ```toml
  glob = "0.3"
  ```

- [ ] **Step 3: Add the core crate dependency**

  In `crates/trusted-server-core/Cargo.toml` under `[dependencies]`, add:

  ```toml
  glob = { workspace = true }
  ```

- [ ] **Step 4: Remove the temporary import and verify**

  Remove the temporary `use glob::Pattern as _;`.

  Run:

  ```bash
  cargo check --package trusted-server-core
  ```

  Expected: clean compile.

- [ ] **Step 5: Commit**

  ```bash
  git add Cargo.toml crates/trusted-server-core/Cargo.toml
  git commit -m "Add glob dependency for creative opportunity matching"
  ```

---

## Task 3: Price Bucket Module

**Files:**

- Create: `crates/trusted-server-core/src/price_bucket.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

Phase 1 implements Prebid built-in granularities: `low`, `medium`, `high`, `dense`, and `auto` (`auto` routes to `dense`). The April 15 design mentions `custom`, but no custom bucket schema is specified; do not implement `custom` in this task.

- [ ] **Step 1: Write failing tests**

  Create `crates/trusted-server-core/src/price_bucket.rs` with:

  ```rust
  //! Prebid price granularity bucketing.

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn dense_below_3_increments_by_0_01() {
          assert_eq!(price_bucket(0.0, PriceGranularity::Dense), "0.00");
          assert_eq!(price_bucket(0.015, PriceGranularity::Dense), "0.01");
          assert_eq!(price_bucket(2.99, PriceGranularity::Dense), "2.99");
      }

      #[test]
      fn dense_3_to_8_increments_by_0_05() {
          assert_eq!(price_bucket(3.03, PriceGranularity::Dense), "3.00");
          assert_eq!(price_bucket(3.05, PriceGranularity::Dense), "3.05");
          assert_eq!(price_bucket(7.99, PriceGranularity::Dense), "7.95");
      }

      #[test]
      fn dense_8_to_20_increments_by_0_50() {
          assert_eq!(price_bucket(8.49, PriceGranularity::Dense), "8.00");
          assert_eq!(price_bucket(8.50, PriceGranularity::Dense), "8.50");
          assert_eq!(price_bucket(19.99, PriceGranularity::Dense), "19.50");
      }

      #[test]
      fn built_in_granularities_cap_correctly() {
          assert_eq!(price_bucket(5.01, PriceGranularity::Low), "5.00");
          assert_eq!(price_bucket(20.5, PriceGranularity::Medium), "20.00");
          assert_eq!(price_bucket(20.5, PriceGranularity::High), "20.00");
          assert_eq!(price_bucket(50.0, PriceGranularity::Dense), "20.00");
      }

      #[test]
      fn auto_routes_to_dense() {
          assert_eq!(
              price_bucket(2.53, PriceGranularity::Auto),
              price_bucket(2.53, PriceGranularity::Dense)
          );
      }
  }
  ```

  Run:

  ```bash
  cargo test -p trusted-server-core price_bucket
  ```

  Expected: compile failure because the module implementation and export are missing.

- [ ] **Step 2: Implement `price_bucket.rs`**

  ```rust
  //! Prebid price granularity bucketing.

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
      /// Returns `Dense`; used as a serde default function pointer.
      pub fn dense() -> Self {
          Self::Dense
      }
  }

  /// Convert raw CPM to the `hb_pb` price bucket string.
  #[must_use]
  pub fn price_bucket(cpm: f64, granularity: PriceGranularity) -> String {
      if cpm <= 0.0 {
          return "0.00".to_string();
      }

      match granularity {
          PriceGranularity::Low => bucket(cpm, 5.0, 0.50),
          PriceGranularity::Medium => bucket(cpm, 20.0, 0.10),
          PriceGranularity::High => bucket(cpm, 20.0, 0.01),
          PriceGranularity::Dense | PriceGranularity::Auto => dense_bucket(cpm),
      }
  }

  fn dense_bucket(cpm: f64) -> String {
      if cpm >= 20.0 {
          return "20.00".to_string();
      }
      if cpm >= 8.0 {
          return bucket(cpm, 20.0, 0.50);
      }
      if cpm >= 3.0 {
          return bucket(cpm, 8.0, 0.05);
      }
      bucket(cpm, 3.0, 0.01)
  }

  fn bucket(cpm: f64, cap: f64, increment: f64) -> String {
      let capped = cpm.min(cap);
      format!("{:.2}", (capped / increment).floor() * increment)
  }
  ```

- [ ] **Step 3: Export the module**

  In `crates/trusted-server-core/src/lib.rs`, add:

  ```rust
  pub mod price_bucket;
  ```

- [ ] **Step 4: Run tests**

  ```bash
  cargo test -p trusted-server-core price_bucket
  ```

  Expected: all price bucket tests pass.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/price_bucket.rs crates/trusted-server-core/src/lib.rs
  git commit -m "Add Prebid price bucket granularity"
  ```

---

## Task 4: Auction Types for Slot Defaults and Bid Targeting

**Files:**

- Modify: `crates/trusted-server-core/src/auction/types.rs`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 1: Write failing tests**

  In the `#[cfg(test)]` module in `crates/trusted-server-core/src/auction/types.rs`, add:

  ```rust
  #[test]
  fn media_type_banner_fn_returns_banner() {
      assert_eq!(MediaType::banner(), MediaType::Banner);
  }

  #[test]
  fn bid_has_ad_id_field_for_gam_targeting() {
      let bid = Bid {
          slot_id: "atf".to_string(),
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

  Run:

  ```bash
  cargo test -p trusted-server-core auction::types
  ```

  Expected: compile failure for missing `MediaType::banner` and `Bid::ad_id`.

- [ ] **Step 2: Add `MediaType::banner()`**

  Add:

  ```rust
  impl MediaType {
      /// Returns `Banner`; used as a serde default function pointer.
      pub fn banner() -> Self {
          Self::Banner
      }
  }
  ```

- [ ] **Step 3: Add `Bid::ad_id`**

  Add this field immediately before `metadata`:

  ```rust
  /// Provider ad ID used for `hb_adid` targeting.
  pub ad_id: Option<String>,
  ```

  Update every `Bid` literal in tests and production code with `ad_id: None` unless parsing a real provider ad ID.

- [ ] **Step 4: Populate Prebid ad IDs**

  In `PrebidAuctionProvider::parse_bid`, add:

  ```rust
  let ad_id = bid_obj
      .get("adid")
      .or_else(|| bid_obj.get("id"))
      .and_then(|v| v.as_str())
      .map(String::from);
  ```

  Include `ad_id` in the returned `AuctionBid`.

- [ ] **Step 5: Run tests**

  ```bash
  cargo test -p trusted-server-core auction::types integrations::prebid
  ```

  Expected: tests pass.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/auction/types.rs crates/trusted-server-core/src/integrations/prebid.rs
  git commit -m "Add bid ad IDs for GAM targeting"
  ```

---

## Task 5: Creative Opportunities Config and URL Matching

**Files:**

- Create: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`

- [ ] **Step 1: Write failing tests**

  Create `crates/trusted-server-core/src/creative_opportunities.rs` with tests for:
  - `/20**` matches multi-segment article paths.
  - `/` matches only the homepage.
  - slot IDs allow only `[A-Za-z0-9_-]+`.
  - `resolved_gam_unit_path()` defaults to `/{gam_network_id}/{id}`.
  - `resolved_div_id()` defaults to slot ID.
  - `to_ad_slot()` transfers formats, floors, targeting strings, and APS `slotID`.
  - empty slot file parses and produces zero matches.

  Use this helper shape in tests:

  ```rust
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
  ```

  Run:

  ```bash
  cargo test -p trusted-server-core creative_opportunities
  ```

  Expected: compile failure because implementation/export is missing.

- [ ] **Step 2: Implement config types and matching**

  Implement:

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

  #[derive(Debug, Clone, Deserialize)]
  pub struct CreativeOpportunityFormat {
      pub width: u32,
      pub height: u32,
      #[serde(default = "MediaType::banner")]
      pub media_type: MediaType,
  }

  #[derive(Debug, Clone, Default, Deserialize)]
  pub struct SlotProviders {
      pub aps: Option<ApsSlotParams>,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct ApsSlotParams {
      pub slot_id: String,
  }

  #[derive(Debug, Clone, Default, Deserialize)]
  pub struct CreativeOpportunitiesFile {
      #[serde(rename = "slot", default)]
      pub slots: Vec<CreativeOpportunitySlot>,
  }
  ```

  Add methods for `matches_path`, `resolved_gam_unit_path`, `resolved_div_id`, `to_ad_slot`, `validate_slot_id`, and `match_slots`.

- [ ] **Step 3: Export the module**

  In `crates/trusted-server-core/src/lib.rs`, add:

  ```rust
  pub mod creative_opportunities;
  ```

- [ ] **Step 4: Run tests**

  ```bash
  cargo test -p trusted-server-core creative_opportunities
  ```

  Expected: tests pass.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/lib.rs
  git commit -m "Add creative opportunity config matching"
  ```

---

## Task 6: Settings, Config File, and Build Validation

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs`
- Modify: `crates/trusted-server-core/build.rs`
- Create: `crates/trusted-server-adapter-fastly/build.rs`
- Create: `creative-opportunities.toml`
- Modify: `trusted-server.toml`

- [ ] **Step 1: Write failing settings test**

  In `settings.rs` tests, add:

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
  price_granularity = "dense"
  "#;

      let settings = Settings::from_toml(toml).expect("should parse");
      let creative_opportunities = settings
          .creative_opportunities
          .expect("should have creative opportunities");
      assert_eq!(creative_opportunities.gam_network_id, "21765378893");
      assert_eq!(creative_opportunities.auction_timeout_ms, Some(500));
  }
  ```

  Run:

  ```bash
  cargo test -p trusted-server-core settings_parses_creative_opportunities_section
  ```

  Expected: compile failure for missing `Settings::creative_opportunities`.

- [ ] **Step 2: Add settings field**

  Import `CreativeOpportunitiesConfig` and add:

  ```rust
  #[serde(default)]
  pub creative_opportunities: Option<CreativeOpportunitiesConfig>,
  ```

- [ ] **Step 3: Add root config files**

  Create `creative-opportunities.toml`:

  ```toml
  # Slot templates for server-side ad auctions.
  # Empty file = feature disabled.

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

  Add to `trusted-server.toml`:

  ```toml
  [creative_opportunities]
  gam_network_id = "21765378893"
  auction_timeout_ms = 500
  price_granularity = "dense"
  ```

- [ ] **Step 4: Add build-time validation**

  In `crates/trusted-server-core/build.rs`, add `cargo:rerun-if-changed` for `../../creative-opportunities.toml`, parse it as `toml::Value`, and validate each `[[slot]].id` with `^[A-Za-z0-9_\-]+$`.

  Rules:
  - Missing file: startup/build error.
  - Malformed TOML: startup/build error.
  - Empty file with zero slots: valid kill-switch.
  - Invalid slot ID: startup/build error.

- [ ] **Step 5: Add adapter rebuild trigger**

  Create `crates/trusted-server-adapter-fastly/build.rs`:

  ```rust
  fn main() {
      println!("cargo:rerun-if-changed=../../../creative-opportunities.toml");
  }
  ```

- [ ] **Step 6: Run verification**

  ```bash
  cargo test -p trusted-server-core settings_parses_creative_opportunities_section
  cargo build --package trusted-server-core
  cargo build --package trusted-server-adapter-fastly
  ```

  Expected: all pass/build.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-core/src/settings.rs crates/trusted-server-core/build.rs \
    crates/trusted-server-adapter-fastly/build.rs creative-opportunities.toml trusted-server.toml
  git commit -m "Wire creative opportunities into settings and build validation"
  ```

---

## Task 7: Bid Cache and `/ts-bids` Semantics

**Files:**

- Create: `crates/trusted-server-core/src/bid_cache.rs`
- Create: `crates/trusted-server-adapter-fastly/src/bid_cache.rs`
- Modify: `crates/trusted-server-core/src/lib.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

Keep the April 15 design's `BidCache` naming in Phase 1. This is the request-ID rendezvous used by the Fastly adapter; do not introduce a broader `AuctionIntent` abstraction in this plan. Production Fastly must not rely on process-global memory. Use Fastly Core Cache for pending/completed bid state, with an in-memory implementation only for unit tests and non-Fastly unsupported paths.

- [ ] **Step 1: Write failing tests**

  Create tests covering:
  - unknown request ID -> `CacheResult::NotFound`
  - pending request ID -> `CacheResult::Pending`
  - pending entry carries the original auction deadline
  - completed request ID -> bids returned
  - expired entry -> not found
  - `wait_for` returns bids immediately when complete
  - `wait_for` returns `Empty` after original deadline passes
  - `get_auction_deadline` returns the pending entry's original deadline

  Run:

  ```bash
  cargo test -p trusted-server-core bid_cache
  ```

  Expected: compile failure because module is missing.

- [ ] **Step 2: Implement core bid cache types and test implementation**

  Implement:

  ```rust
  pub type BidMap = std::collections::HashMap<String, serde_json::Value>;

  #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
  pub enum BidCacheEntry {
      Pending { auction_deadline_epoch_ms: u64 },
      Complete { bids: BidMap },
  }

  #[derive(Debug, Clone, Copy)]
  pub struct AuctionDeadline {
      pub instant: Instant,
      pub epoch_ms: u64,
  }

  #[derive(Debug)]
  pub enum CacheResult {
      Complete { bids: BidMap },
      Pending { auction_deadline: AuctionDeadline },
      NotFound,
  }

  #[derive(Debug)]
  pub enum WaitResult {
      Bids(BidMap),
      Empty,
      NotFound,
  }

  pub trait BidCache {
      fn mark_pending(&self, request_id: &str, auction_deadline: AuctionDeadline) -> Result<(), BidCacheError>;
      fn put(&self, request_id: &str, bids: BidMap) -> Result<(), BidCacheError>;
      fn try_get(&self, request_id: &str) -> Result<CacheResult, BidCacheError>;
  }

  pub struct InMemoryBidCache {
      inner: std::sync::Mutex<BidCacheInner>,
  }
  ```

  Required methods:
  - `new(ttl: Duration, capacity: usize) -> Self`
  - `mark_pending(request_id: &str, auction_deadline: AuctionDeadline) -> Result<(), BidCacheError>`
  - `put(request_id: &str, bids: BidMap) -> Result<(), BidCacheError>`
  - `put_empty(request_id: &str)` or equivalent `put(request_id, HashMap::new())`
  - `try_get(request_id: &str) -> Result<CacheResult, BidCacheError>`
  - `get_auction_deadline(request_id: &str) -> Option<AuctionDeadline>`
  - `wait_for(request_id: &str, deadline: AuctionDeadline) -> WaitResult`

  `AuctionDeadline` must be computed once when the page request starts from both `Instant::now()` and `SystemTime::now()`. In-process tests can use the `Instant`; Fastly Core Cache must persist `epoch_ms` and reconstruct an equivalent local `Instant` on `/ts-bids` so it can enforce the original deadline in a separate request. `wait_for` must not mint a new timeout.

- [ ] **Step 3: Implement Fastly Core Cache `BidCache`**

  In `crates/trusted-server-adapter-fastly/src/bid_cache.rs`, implement `FastlyBidCache`:
  - Cache key: `ts-bids:<request_id>`.
  - `mark_pending`: insert `BidCacheEntry::Pending` with the original deadline and a short max-age, marking the cache object as sensitive data.
  - `put`: replace/insert `BidCacheEntry::Complete` with the bid map.
  - `try_get`: lookup and deserialize the cache object.
  - Unknown/missing cache object maps to `CacheResult::NotFound`.

  Use `fastly::cache::core` APIs, not `static` process memory, for production Fastly rendezvous.

- [ ] **Step 4: Export the module**

  In `crates/trusted-server-core/src/lib.rs`, add:

  ```rust
  pub mod bid_cache;
  ```

- [ ] **Step 5: Run tests**

  ```bash
  cargo test -p trusted-server-core bid_cache
  cargo test -p trusted-server-adapter-fastly bid_cache
  ```

  Expected: tests pass.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/bid_cache.rs crates/trusted-server-core/src/lib.rs \
    crates/trusted-server-adapter-fastly/src/bid_cache.rs crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Add request-scoped bid cache"
  ```

---

## Task 8: HTML Head Globals Injection

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write failing HTML processor tests**

  Add tests proving:
  - `ad_slots_script` is prepended at `<head>` open.
  - injected content contains `window.__ts_ad_slots` and `window.__ts_request_id`.
  - output never contains `window.__ts_bids`.
  - there is no `</head>` end-tag handler or bid injection field.

  Run:

  ```bash
  cargo test -p trusted-server-core html_processor
  ```

  Expected: compile failure for missing `HtmlProcessorConfig::ad_slots_script`.

- [ ] **Step 2: Add config field**

  Add to `HtmlProcessorConfig`:

  ```rust
  /// Precomputed head globals script. Contains ad slots and request ID only.
  pub ad_slots_script: Option<String>,
  ```

  Initialize it to `None` in `HtmlProcessorConfig::from_settings`.

- [ ] **Step 3: Inject once at `<head>` open**

  In the existing `element!("head", ...)` handler:
  - Build one `snippet` string.
  - Push `ad_slots_script` first when present.
  - Then push integration head inserts.
  - Then push tsjs script tags.
  - Call `el.prepend(&snippet, ContentType::Html)` once.

  Do not add `on_end_tag`.

- [ ] **Step 4: Add publisher script helpers**

  In `publisher.rs`, add `pub(crate)` helpers:
  - `build_head_globals_script(matched_slots, request_id, co_config) -> String`
  - `html_escape_for_script(json: &str) -> String`

  The script must use `JSON.parse("...escaped JSON...")` and must not interpolate raw JSON into executable JavaScript.

- [ ] **Step 5: Run tests**

  ```bash
  cargo test -p trusted-server-core html_processor
  ```

  Expected: tests pass.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/html_processor.rs crates/trusted-server-core/src/publisher.rs
  git commit -m "Inject server-side ad globals at head open"
  ```

---

## Task 9: Publisher Helpers for Bids, Consent, and Cache Headers

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write failing helper tests**

  Add tests covering:
  - `build_head_globals_script` includes slots and request ID.
  - script escaping neutralizes `</script>`, `<`, `>`, `&`, U+2028, and U+2029.
  - `build_bid_map` emits `hb_pb`, `hb_bidder`, `hb_adid`, and `burl`.
  - bids with `price: None` are omitted.
  - `server_side_auction_allowed` returns true only when TCF exists and purpose 1 is consented.
  - response cache policy sets `Cache-Control: private, no-store` when slots matched and consent is denied.
  - response cache policy sets `Cache-Control: private, no-store` when globals are injected.
  - response cache policy does not remove `Surrogate-Control` or `Fastly-Surrogate-Control`.
  - no-match responses preserve origin cache headers.

  Run:

  ```bash
  cargo test -p trusted-server-core publisher::creative_opportunities_tests
  ```

  Expected: failure because helpers are missing.

- [ ] **Step 2: Implement bid map helper**

  Implement `build_bid_map`:

  ```rust
  pub(crate) fn build_bid_map(
      winning_bids: &std::collections::HashMap<String, crate::auction::types::Bid>,
      price_granularity: crate::price_bucket::PriceGranularity,
  ) -> crate::bid_cache::BidMap {
      winning_bids
          .iter()
          .filter_map(|(slot_id, bid)| {
              let cpm = bid.price?;
              Some((
                  slot_id.clone(),
                  serde_json::json!({
                      "hb_pb": crate::price_bucket::price_bucket(cpm, price_granularity),
                      "hb_bidder": bid.bidder,
                      "hb_adid": bid.ad_id.as_deref().unwrap_or(""),
                      "burl": bid.burl,
                  }),
              ))
          })
          .collect()
  }
  ```

- [ ] **Step 3: Implement consent helper**

  Implement a small helper used only by the server-side ad-template path:

  ```rust
  fn server_side_auction_allowed(consent_context: &crate::consent::ConsentContext) -> bool {
      consent_context
          .tcf
          .as_ref()
          .is_some_and(|tcf| tcf.has_purpose_consent(1))
  }
  ```

  This intentionally follows the April 15 design: absent TCF means no server-side auction and no ad globals for this Phase 1 path.

- [ ] **Step 4: Implement browser cache policy helper**

  Implement a helper that receives `slots_matched: bool` and `globals_injected: bool`.

  Required behavior:
  - If no slots matched, do nothing.
  - If slots matched and consent denied, set `Cache-Control: private, no-store`.
  - If globals are injected, set `Cache-Control: private, no-store`.
  - Preserve `Surrogate-Control` and `Fastly-Surrogate-Control`.

- [ ] **Step 5: Run tests**

  ```bash
  cargo test -p trusted-server-core publisher::creative_opportunities_tests
  ```

  Expected: tests pass.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Add publisher helpers for ad template responses"
  ```

---

## Task 10: GPT `__tsAdInit` Bootstrap

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs`
- Modify: `crates/js/lib/src/integrations/gpt/index.ts`
- Modify: `crates/js/lib/src/integrations/gpt/index.test.ts`

- [x] **Step 1: Write failing Rust test**

  In `gpt.rs`, add a test that combines `head_inserts()` output and asserts:
  - contains `__tsAdInit`
  - contains `/ts-bids`
  - contains `__ts_request_id`
  - contains `bidsPromise`
  - contains `slotRenderEnded`
  - contains `sendBeacon`
  - does not contain `__ts_bids`

  Run:

  ```bash
  cargo test -p trusted-server-core integrations::gpt
  ```

  Expected: failure because `__tsAdInit` is missing.

- [x] **Step 2: Extend GPT Rust head injector**

  Keep the existing GPT shim install snippet and add an inline `__tsAdInit` snippet that:
  - reads `window.__ts_ad_slots || []`
  - reads `window.__ts_request_id`
  - starts `fetch('/ts-bids?rid=' + encodeURIComponent(rid), { credentials: 'omit' })`
  - catches failures and resolves to `{}`
  - defines GPT slots immediately
  - applies static targeting immediately
  - waits for `bidsPromise` before applying `hb_*` targeting and calling `refresh()`
  - fires `burl` through `navigator.sendBeacon` only after `slotRenderEnded` confirms matching `hb_adid`

- [x] **Step 3: Write failing TypeScript tests**

  Add tests for:
  - `/ts-bids?rid=<request_id>` fetch with `credentials: 'omit'`
  - static slot targeting applied before refresh
  - `hb_pb`, `hb_bidder`, and `hb_adid` applied before refresh
  - fetch failure still calls `refresh()`
  - `slotRenderEnded` fires `burl` only when rendered slot targeting `hb_adid` matches the bid

  Run:

  ```bash
  cd crates/js/lib && npx vitest run src/integrations/gpt/index.test.ts
  ```

  Expected: failure because `installTsAdInit` is missing.

- [x] **Step 4: Implement `installTsAdInit`**

  In `index.ts`, export `installTsAdInit()` and call it from the integration initialization path. Keep the existing GPT guard behavior.

- [x] **Step 5: Run tests and build**

  ```bash
  cargo test -p trusted-server-core integrations::gpt
  cd crates/js/lib && npx vitest run src/integrations/gpt/index.test.ts
  cd crates/js/lib && node build-all.mjs
  ```

  Expected: all pass.

- [x] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/integrations/gpt.rs crates/js/lib/src/integrations/gpt
  git commit -m "Add GPT bid fetch bootstrap"
  ```

---

## Task 11: `/ts-bids` Endpoint

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

This route is Fastly-only for Phase 1. Future adapters can omit this route, return unsupported, or keep server-side ad templates disabled until the EdgeZero migration provides equivalent request-rendezvous semantics.

- [x] **Step 1: Write failing tests**

  Add route or helper tests for:
  - missing `rid` -> `400`
  - unknown `rid` -> `404`
  - completed bids -> `200` JSON body with slot bid map
  - completed empty bid map -> `200` body `{}`
  - pending request reaches original deadline -> `200` body `{}`
  - all responses set `Cache-Control: private, no-store`

  Run:

  ```bash
  cargo test -p trusted-server-adapter-fastly ts_bids
  ```

  Expected: failure because handler is missing.

- [x] **Step 2: Add route**

  Before the publisher fallback route, add:

  ```rust
  (Method::GET, "/ts-bids") => Ok(handle_ts_bids_request(req, bid_cache)),
  ```

  Adjust `route_request` parameters to receive `bid_cache: &BidCache`.

- [x] **Step 3: Implement handler**

  Handler behavior:
  - Parse `rid` from query.
  - Missing/empty `rid`: `400` plain text.
  - Call `bid_cache.try_get(&rid)`.
  - `CacheResult::Pending { auction_deadline }`: long-poll by rechecking `try_get` until the persisted original deadline or completion.
  - `WaitResult::Bids(bids)`: serialize bids as JSON.
  - Empty map serializes as `{}`.
  - `WaitResult::Empty`: return `200` with `{}`.
  - `WaitResult::NotFound`: return `404`.
  - Always set `Cache-Control: private, no-store`.

- [x] **Step 4: Run tests**

  ```bash
  cargo test -p trusted-server-adapter-fastly ts_bids
  ```

  Expected: tests pass.

- [x] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Add ts-bids endpoint"
  ```

---

## Task 12: Publisher Path Integration

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

Do not start this task unless Task 1 has a `Go` decision. Implement exactly the non-blocking shape selected by Task 1.

- [x] **Step 1: Write failing integration tests**

  Add tests proving:
  - platform HTTP client exposes a non-blocking `poll` operation for pending requests
  - Fastly `poll` maps `PendingRequest::poll()` pending/done/error states into platform-neutral results
  - server-side auction can be started and advanced through a non-blocking poll method
  - no matching slots -> no globals, no auction pending entry, cache headers preserved
  - matching slots + denied/missing consent -> no globals, no auction pending entry, `Cache-Control: private, no-store`
  - matching slots + consent allowed -> globals injected, pending entry registered with original deadline
  - origin response is returned/streamed without waiting for auction completion
  - completed auction writes `BidCache`
  - auction failure writes empty bid map or otherwise lets `/ts-bids` resolve `{}` before deadline
  - `Surrogate-Control` and `Fastly-Surrogate-Control` are preserved

  Run:

  ```bash
  cargo test -p trusted-server-core publisher
  ```

  Expected: failures for missing integration.

- [x] **Step 2: Add platform pending-request polling**

  Extend `PlatformHttpClient` with a non-blocking poll method:

  ```rust
  async fn poll(
      &self,
      pending: PlatformPendingRequest,
  ) -> Result<PlatformPollResult, Report<PlatformError>>;
  ```

  `PlatformPollResult` must include:
  - `Pending(PlatformPendingRequest)`
  - `Ready(Result<PlatformResponse, Report<PlatformError>>)`

  In the Fastly adapter, implement it with `fastly::http::request::PollResult` from `PendingRequest::poll()`. Non-Fastly/test implementations may return `PlatformError::Unsupported` until EdgeZero migration adds equivalent primitives.

- [x] **Step 3: Add pollable auction progression**

  Refactor auction orchestration without changing existing `/auction` behavior:
  - Keep `AuctionOrchestrator::run_auction` for existing endpoints.
  - Add a server-side-template path that can `start` provider requests and return a `PendingAuction`.
  - `PendingAuction::poll_once()` must call platform `poll` and return immediately.
  - `PendingAuction::finish_due_to_deadline()` must drop remaining pending requests and compute winners from responses collected so far.
  - Parsing and winning-bid selection must reuse existing provider/orchestrator logic.

  This is the mechanism that lets publisher streaming continue while auction work advances opportunistically between streaming chunks.

- [x] **Step 4: Load creative opportunities and bid cache in adapter**

  In `main.rs`, add:

  ```rust
  const CREATIVE_OPPORTUNITIES_TOML: &str =
      include_str!("../../../creative-opportunities.toml");
  ```

  Parse immutable creative opportunity config through a process-global lazy value if compatible with Fastly Compute:

  ```rust
  static CREATIVE_OPPORTUNITIES: std::sync::LazyLock<
      trusted_server_core::creative_opportunities::CreativeOpportunitiesFile,
  > = std::sync::LazyLock::new(|| {
      toml::from_str(CREATIVE_OPPORTUNITIES_TOML)
          .expect("should parse creative-opportunities.toml")
  });
  ```

  Initialize Fastly `BidCache` through the Task 1 verified Core Cache-backed implementation. Do not use process-global request state for production bid rendezvous.

  The Fastly bid cache itself should be a lightweight value over Core Cache APIs and may be constructed per request because the state lives in Fastly Core Cache, not the Rust object.

- [x] **Step 5: Update publisher handler signature**

  Add the dependencies required by the selected Task 1 shape:
  - `orchestrator: &AuctionOrchestrator`
  - `slots_file: &CreativeOpportunitiesFile`
  - `bid_cache: &BidCache`

  Keep `AuctionContext` construction aligned with current code: include `settings`, `request`, `client_info`, `timeout_ms`, `provider_responses`, and `services`.

- [x] **Step 6: Match slots and decide consent before origin body processing**

  Required behavior:
  - Mint `request_id` only when slots match.
  - Match against `req.get_path()`.
  - If no slots match, do not register `BidCache`, do not inject globals, and preserve cache headers.
  - If slots match but consent is denied/absent, do not run auction and do not inject globals; set browser `Cache-Control: private, no-store`.
  - If slots match and consent allows, register pending cache entry with `A_deadline`, inject globals, and dispatch auction through the Task 1 verified non-blocking path.

- [x] **Step 7: Preserve streaming invariant**

  The implementation must satisfy:
  - Origin request is dispatched immediately.
  - Page response headers/body are not held for `run_auction`.
  - No `wait()` or blocking `select()` for auction work occurs before the page starts streaming.
  - During body streaming, auction work may only use non-blocking `poll` calls between chunk writes.
  - If the auction completes after page streaming starts, it writes bid results to `BidCache`.

- [x] **Step 8: Force chunked browser response for processed HTML**

  For responses that enter the HTML processing path:
  - Remove `Content-Length`.
  - Set `Transfer-Encoding: chunked` if Fastly permits it explicitly.
  - Do not force chunked on binary pass-through responses.

- [x] **Step 9: Run tests**

  ```bash
  cargo test -p trusted-server-core publisher
  cargo test -p trusted-server-adapter-fastly
  ```

  Expected: tests pass.

- [x] **Step 10: Commit**

  ```bash
  git add crates/trusted-server-core/src/platform/http.rs crates/trusted-server-adapter-fastly/src/platform.rs \
    crates/trusted-server-core/src/auction/orchestrator.rs crates/trusted-server-core/src/publisher.rs \
    crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Start ad template auctions without blocking HTML streaming"
  ```

---

## Task 13: Server-Side `nurl` Fire-and-Forget

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

This task implements `nurl` firing only for the Fastly Phase 1 path. Future adapters may return unsupported or disable server-side ad templates until they can provide equivalent non-blocking outbound HTTP behavior.

- [ ] **Step 1: Write failing config test**

  In Prebid tests, assert:

  ```rust
  #[test]
  fn prebid_fire_nurl_at_edge_defaults_to_true() {
      let config = PrebidIntegrationConfig {
          fire_nurl_at_edge: true,
          ..base_config()
      };
      assert!(config.fire_nurl_at_edge, "should default to edge nurl firing");
  }
  ```

  Adjust to the existing test helper style in `prebid.rs`.

  Run:

  ```bash
  cargo test -p trusted-server-core integrations::prebid
  ```

  Expected: failure until field/default is implemented.

- [ ] **Step 2: Add config field**

  Add to `PrebidIntegrationConfig`:

  ```rust
  #[serde(default = "default_fire_nurl_at_edge")]
  pub fire_nurl_at_edge: bool,
  ```

  Add:

  ```rust
  fn default_fire_nurl_at_edge() -> bool {
      true
  }
  ```

- [ ] **Step 3: Fire winning nurls after auction completion**

  In the selected non-blocking auction completion path, after writing bid results to `BidCache`, call a helper that:
  - Reads `PrebidIntegrationConfig` via `settings.integrations.get_typed::<PrebidIntegrationConfig>("prebid")`.
  - Defaults to `true` if config is absent.
  - Uses the Fastly-supported async HTTP primitive from Task 1, for example `fastly::Request::get(nurl).send_async(&backend_name)`, for each winning bid with `nurl`.
  - Logs warnings but never fails the page or `/ts-bids`.

- [ ] **Step 4: Run tests**

  ```bash
  cargo test -p trusted-server-core integrations::prebid publisher
  ```

  Expected: tests pass.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/publisher.rs
  git commit -m "Fire Prebid nurls from the edge"
  ```

---

## Task 14: End-to-End Verification

**Files:**

- Modify only if needed by test fixes.

- [ ] **Step 1: Run Rust tests**

  ```bash
  cargo test --workspace
  ```

  Expected: all tests pass.

- [ ] **Step 2: Run Rust formatting check**

  ```bash
  cargo fmt --all -- --check
  ```

  Expected: no formatting changes needed.

- [ ] **Step 3: Run Clippy**

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

  Expected: no warnings.

- [ ] **Step 4: Run JS tests and build**

  ```bash
  cd crates/js/lib && npx vitest run
  cd crates/js/lib && node build-all.mjs
  ```

  Expected: tests and build pass.

- [ ] **Step 5: Run manual Fastly verification**

  Run:

  ```bash
  fastly compute serve
  ```

  In another shell, use the local server from `fastly.toml`:

  ```bash
  curl -i http://127.0.0.1:7676/about
  curl -i http://127.0.0.1:7676/2024/01/test-article/
  curl -i "http://127.0.0.1:7676/ts-bids"
  curl -i "http://127.0.0.1:7676/ts-bids?rid=not-real"
  ```

  For a matched page with valid consent, copy the `window.__ts_request_id` value from the HTML and run:

  ```bash
  curl -i "http://127.0.0.1:7676/ts-bids?rid=<request_id>"
  ```

  Verify with curl output:
  - `/about`: no `__ts_ad_slots`, no `__ts_request_id`, no TS-added `Cache-Control: private, no-store`.
  - matched URL with valid consent: contains `__ts_ad_slots` and `__ts_request_id` at `<head>` open, no `__ts_bids`, browser-facing `Cache-Control: private, no-store`.
  - matched URL with denied/missing consent: no ad globals and browser-facing `Cache-Control: private, no-store`.
  - `Surrogate-Control` and `Fastly-Surrogate-Control` from origin are preserved.
  - `/ts-bids?rid=<rid>` returns JSON, `Content-Type: application/json`, `Cache-Control: private, no-store`.
  - `/ts-bids` without `rid` returns `400`.
  - `/ts-bids?rid=not-real` returns `404`.
  - First HTML bytes arrive before a delayed auction completes, using the evidence path from Task 1.

- [ ] **Step 6: Run browser verification**

  With `fastly compute serve` still running, verify manually in Chrome or with Chrome MCP:
  - Open `http://127.0.0.1:7676/2024/01/test-article/`.
  - Evaluate `window.__ts_ad_slots` and `window.__ts_request_id`; both should exist only on matched, consent-allowed pages.
  - Evaluate `window.__ts_bids`; it should be `undefined`.
  - Inspect the Network panel and confirm the page issues a single `/ts-bids?rid=<request_id>` request.
  - Confirm the console has no GPT bootstrap errors.
  - Confirm no ad globals exist on `/about` or matched pages with denied/missing consent.

- [ ] **Step 7: Commit any final test/documentation fixes**

  ```bash
  git status --short
  git add <changed-files>
  git commit -m "Verify server-side ad templates end to end"
  ```

---

## Known Limitations and Follow-Ups

| Item                       | Notes                                                                                                                                                                     |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly-only Phase 1        | Non-Fastly adapters may remain unsupported until EdgeZero migration provides equivalent non-blocking HTTP polling, rendezvous, and HTTP semantics.                        |
| Concurrency primitive      | Must be proven in Task 1. If Fastly cannot complete auction work after streaming starts, implementation stops rather than violating FCP.                                  |
| `BidCache` locality        | Fastly Core Cache is POP/locality scoped. `/ts-bids` can miss if routed away from the page request's cache locality; it should fail closed to `{}` or `404` as specified. |
| `custom` price granularity | Mentioned in the design but no schema is defined. Phase 1 implements built-in Prebid granularities only.                                                                  |
| Dynamic slot config        | Phase 1 uses `include_str!`; slot changes require redeploy. KV-backed config is a follow-up.                                                                              |
| Server-side GAM            | Out of scope. GPT remains client-side in Phase 1.                                                                                                                         |
| PBS stored requests        | Slot IDs must exist in PBS stored request configuration before production rollout.                                                                                        |
| Dynamic backend allowlist  | `nurl` fire-and-forget requires SSP domains to be allowed by Fastly dynamic backend policy.                                                                               |
