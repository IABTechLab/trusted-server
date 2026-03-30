# PR 7 — Geo Lookup + Client Info (Extract-Once) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all `req.get_client_ip_addr()`, `req.get_tls_protocol()`, and `req.get_tls_cipher_openssl_name()` calls from active (non-deprecated) code in `trusted-server-core` by threading the already-populated `RuntimeServices.client_info` to every call site.

**Architecture:** Cascade from the struct that most files depend on (`AuctionContext`) outward to callers. Each task ends with `cargo test --workspace` passing. Function signature changes always update all callers in the same task to keep the codebase compilable. No new abstractions — pure threading of existing types.

**Tech Stack:** Rust 1.91.1, `error-stack`, Fastly SDK, `viceroy` test runner. All tests run via `cargo test --workspace`.

---

## File Map

| File | Change |
|------|--------|
| `crates/trusted-server-core/src/auction/types.rs` | Add `client_info: &'a ClientInfo` to `AuctionContext<'a>` |
| `crates/trusted-server-core/src/auction/endpoints.rs` | Fix `AuctionContext` construction; thread `services` to `generate_synthetic_id`; replace `GeoInfo::from_request` with `services.geo().lookup()`; update `convert_tsjs_to_auction_request` call |
| `crates/trusted-server-core/src/auction/orchestrator.rs` | Fix 2 production `AuctionContext` constructions + 1 test helper |
| `crates/trusted-server-core/src/integrations/prebid.rs` | Update 2 `RequestInfo::from_request` call sites; update 2 test helpers |
| `crates/trusted-server-core/src/http_util.rs` | Change `from_request` to `(req: &Request, client_info: &ClientInfo)`; update `detect_request_scheme`; update 8 test call sites; add 1 new TLS test |
| `crates/trusted-server-core/src/publisher.rs` | Add `services: &RuntimeServices` param; update `from_request`, `get_or_generate_synthetic_id`, and geo call sites |
| `crates/trusted-server-core/src/synthetic.rs` | Add `services: &RuntimeServices` to `generate_synthetic_id` and `get_or_generate_synthetic_id`; update tests |
| `crates/trusted-server-core/src/auction/formats.rs` | Add `services: &RuntimeServices, geo: Option<GeoInfo>` params; thread services to `generate_synthetic_id`; replace `DeviceInfo.ip` and `DeviceInfo.geo` Fastly calls |
| `crates/trusted-server-core/src/integrations/registry.rs` | Thread `services` to `get_or_generate_synthetic_id` |
| `crates/trusted-server-core/src/integrations/didomi.rs` | Rename `_services` → `services`; add `client_ip: Option<IpAddr>` to `copy_headers` |
| `crates/trusted-server-adapter-fastly/src/main.rs` | Pass `&runtime_services` to `handle_publisher_request` |

---

## Task 1: Add `client_info` to `AuctionContext` and fix all construction sites

**Files:**
- Modify: `crates/trusted-server-core/src/auction/types.rs:102-109`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:74-80`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs:145-150`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs:321-326`
- Modify: `crates/trusted-server-core/src/auction/orchestrator.rs:673-683` (test helper)
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:1283-1293` (test helper)
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:2664-2678` (test helper)

- [ ] **Step 1: Add `client_info` to `AuctionContext` in `types.rs`**

  In `crates/trusted-server-core/src/auction/types.rs`, add the `ClientInfo` import and new field:

  ```rust
  // Add to existing imports at the top of the file:
  use crate::platform::ClientInfo;
  ```

  Change the `AuctionContext` struct (around line 102):
  ```rust
  // Before:
  pub struct AuctionContext<'a> {
      pub settings: &'a Settings,
      pub request: &'a Request,
      pub timeout_ms: u32,
      pub provider_responses: Option<&'a [AuctionResponse]>,
  }

  // After:
  pub struct AuctionContext<'a> {
      pub settings: &'a Settings,
      pub request: &'a Request,
      pub timeout_ms: u32,
      pub provider_responses: Option<&'a [AuctionResponse]>,
      pub client_info: &'a ClientInfo,
  }
  ```

  At this point `cargo build` fails because 6 construction sites are missing the new field.

- [ ] **Step 2: Fix `endpoints.rs` construction site (line ~75)**

  In `crates/trusted-server-core/src/auction/endpoints.rs`, update the `AuctionContext` struct literal:
  ```rust
  // Before:
  let context = AuctionContext {
      settings,
      request: &req,
      timeout_ms: settings.auction.timeout_ms,
      provider_responses: None,
  };

  // After:
  let context = AuctionContext {
      settings,
      request: &req,
      timeout_ms: settings.auction.timeout_ms,
      provider_responses: None,
      client_info: &services.client_info,
  };
  ```

- [ ] **Step 3: Fix `orchestrator.rs` production construction sites**

  In `crates/trusted-server-core/src/auction/orchestrator.rs`:

  Line ~145 (mediator context):
  ```rust
  // Before:
  let mediator_context = AuctionContext {
      settings: context.settings,
      request: context.request,
      timeout_ms: remaining_ms,
      provider_responses: Some(&provider_responses),
  };

  // After:
  let mediator_context = AuctionContext {
      settings: context.settings,
      request: context.request,
      timeout_ms: remaining_ms,
      provider_responses: Some(&provider_responses),
      client_info: context.client_info,
  };
  ```

  Line ~321 (provider context):
  ```rust
  // Before:
  let provider_context = AuctionContext {
      settings: context.settings,
      request: context.request,
      timeout_ms: effective_timeout,
      provider_responses: context.provider_responses,
  };

  // After:
  let provider_context = AuctionContext {
      settings: context.settings,
      request: context.request,
      timeout_ms: effective_timeout,
      provider_responses: context.provider_responses,
      client_info: context.client_info,
  };
  ```

- [ ] **Step 4: Fix `orchestrator.rs` test helper `create_test_context` (line ~673)**

  In the `#[cfg(test)]` module of `crates/trusted-server-core/src/auction/orchestrator.rs`:

  ```rust
  // Before:
  fn create_test_context<'a>(
      settings: &'a crate::settings::Settings,
      req: &'a Request,
  ) -> AuctionContext<'a> {
      AuctionContext {
          settings,
          request: req,
          timeout_ms: 2000,
          provider_responses: None,
      }
  }

  // After:
  fn create_test_context<'a>(
      settings: &'a crate::settings::Settings,
      req: &'a Request,
      client_info: &'a crate::platform::ClientInfo,
  ) -> AuctionContext<'a> {
      AuctionContext {
          settings,
          request: req,
          timeout_ms: 2000,
          provider_responses: None,
          client_info,
      }
  }
  ```

  Then update every call site of `create_test_context` in the same file to pass:
  ```rust
  &crate::platform::ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None }
  ```

- [ ] **Step 5: Fix `prebid.rs` test helper `create_test_auction_context` (line ~1283)**

  In the `#[cfg(test)]` module of `crates/trusted-server-core/src/integrations/prebid.rs`:

  ```rust
  // Before:
  fn create_test_auction_context<'a>(
      settings: &'a Settings,
      request: &'a Request,
  ) -> AuctionContext<'a> {
      AuctionContext {
          settings,
          request,
          timeout_ms: 1000,
          provider_responses: None,
      }
  }

  // After:
  fn create_test_auction_context<'a>(
      settings: &'a Settings,
      request: &'a Request,
      client_info: &'a crate::platform::ClientInfo,
  ) -> AuctionContext<'a> {
      AuctionContext {
          settings,
          request,
          timeout_ms: 1000,
          provider_responses: None,
          client_info,
      }
  }
  ```

  Update every `create_test_auction_context(settings, req)` call in `prebid.rs` to pass:
  ```rust
  create_test_auction_context(settings, req, &crate::platform::ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None })
  ```

- [ ] **Step 6: Fix `prebid.rs` test helper `call_to_openrtb` (line ~2664)**

  ```rust
  // Before:
  fn call_to_openrtb(
      config: PrebidIntegrationConfig,
      request: &AuctionRequest,
  ) -> OpenRtbRequest {
      let provider = PrebidAuctionProvider::new(config);
      let settings = make_settings();
      let fastly_req = Request::new(Method::POST, "https://example.com/auction");
      let context = AuctionContext {
          settings: &settings,
          request: &fastly_req,
          timeout_ms: 1000,
          provider_responses: None,
      };
      provider.to_openrtb(request, &context, None)
  }

  // After:
  fn call_to_openrtb(
      config: PrebidIntegrationConfig,
      request: &AuctionRequest,
  ) -> OpenRtbRequest {
      let provider = PrebidAuctionProvider::new(config);
      let settings = make_settings();
      let fastly_req = Request::new(Method::POST, "https://example.com/auction");
      let client_info = crate::platform::ClientInfo {
          client_ip: None,
          tls_protocol: None,
          tls_cipher: None,
      };
      let context = AuctionContext {
          settings: &settings,
          request: &fastly_req,
          timeout_ms: 1000,
          provider_responses: None,
          client_info: &client_info,
      };
      provider.to_openrtb(request, &context, None)
  }
  ```

- [ ] **Step 7: Run tests to verify Task 1 compiles and passes**

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass.

- [ ] **Step 8: Commit Task 1**

  ```bash
  git add crates/trusted-server-core/src/auction/types.rs \
          crates/trusted-server-core/src/auction/endpoints.rs \
          crates/trusted-server-core/src/auction/orchestrator.rs \
          crates/trusted-server-core/src/integrations/prebid.rs
  git commit -m "Add client_info field to AuctionContext and fix all construction sites"
  ```

---

## Task 2: Change `RequestInfo::from_request` to take `&ClientInfo`, add `services` to `handle_publisher_request`, update `main.rs`

These four changes must happen together because:
- `publisher.rs` needs `services` to supply `&services.client_info` to `from_request`
- `main.rs` must be updated when `publisher.rs` signature changes
- `prebid.rs` can now use `context.client_info` (available since Task 1)

**Files:**
- Modify: `crates/trusted-server-core/src/http_util.rs:89-94, 166-212, 393-562` (tests)
- Modify: `crates/trusted-server-core/src/publisher.rs:290-294, 301`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:713, 1011`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:195`

- [ ] **Step 1: Update `http_util.rs` — change `from_request` signature and `detect_request_scheme`**

  Add `ClientInfo` to the imports in `crates/trusted-server-core/src/http_util.rs`. Look at the existing `use crate::` lines at the top and add:
  ```rust
  use crate::platform::ClientInfo;
  ```

  Change `RequestInfo::from_request` (line ~89):
  ```rust
  // Before:
  pub fn from_request(req: &Request) -> Self {
      let host = extract_request_host(req);
      let scheme = detect_request_scheme(req);
      Self { host, scheme }
  }

  // After:
  pub fn from_request(req: &Request, client_info: &ClientInfo) -> Self {
      let host = extract_request_host(req);
      let scheme = detect_request_scheme(req, client_info.tls_protocol.as_deref(), client_info.tls_cipher.as_deref());
      Self { host, scheme }
  }
  ```

  Change `detect_request_scheme` (line ~166):
  ```rust
  // Before:
  fn detect_request_scheme(req: &Request) -> String {
      // 1. First try Fastly SDK's built-in TLS detection methods
      if let Some(tls_protocol) = req.get_tls_protocol() {
          log::debug!("TLS protocol detected: {}", tls_protocol);
          return "https".to_string();
      }

      // Also check TLS cipher - if present, connection is HTTPS
      if req.get_tls_cipher_openssl_name().is_some() {
          log::debug!("TLS cipher detected, using HTTPS");
          return "https".to_string();
      }
      // ... rest unchanged

  // After:
  fn detect_request_scheme(req: &Request, tls_protocol: Option<&str>, tls_cipher: Option<&str>) -> String {
      // 1. Check ClientInfo TLS fields (extracted once at entry point)
      if let Some(protocol) = tls_protocol {
          log::debug!("TLS protocol detected: {}", protocol);
          return "https".to_string();
      }
      if tls_cipher.is_some() {
          log::debug!("TLS cipher detected, using HTTPS");
          return "https".to_string();
      }
      // ... rest unchanged (Forwarded, X-Forwarded-Proto, Fastly-SSL, default http)
  ```

- [ ] **Step 2: Update `http_util.rs` tests — replace 8 `from_request` call sites**

  Add `ClientInfo` import to the `#[cfg(test)]` module. Look for the existing `use super::*;` line and add below it:
  ```rust
  use crate::platform::ClientInfo;
  ```

  Replace every `RequestInfo::from_request(&req)` in the test module with:
  ```rust
  RequestInfo::from_request(&req, &ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None })
  ```

  There are 8 call sites: the test functions at lines ~398, ~416, ~429, ~440, ~459, ~475, ~494, ~552.

- [ ] **Step 3: Add a new test for TLS-detected HTTPS via `ClientInfo`**

  In the `#[cfg(test)]` module of `http_util.rs`, after the existing `RequestInfo` tests:

  ```rust
  #[test]
  fn request_info_https_from_client_info_tls_protocol() {
      let req = Request::new(fastly::http::Method::GET, "https://test.example.com/page");
      let client_info = ClientInfo {
          client_ip: None,
          tls_protocol: Some("TLSv1.3".to_string()),
          tls_cipher: None,
      };

      let info = RequestInfo::from_request(&req, &client_info);

      assert_eq!(
          info.scheme, "https",
          "should detect https from ClientInfo tls_protocol"
      );
  }
  ```

- [ ] **Step 4: Add `services: &RuntimeServices` to `handle_publisher_request` in `publisher.rs`**

  In `crates/trusted-server-core/src/publisher.rs`, add the import (check if already imported):
  ```rust
  use crate::platform::RuntimeServices;
  ```

  Change the function signature (line ~290):
  ```rust
  // Before:
  pub fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      mut req: Request,
  ) -> Result<Response, Report<TrustedServerError>>

  // After:
  pub fn handle_publisher_request(
      settings: &Settings,
      integration_registry: &IntegrationRegistry,
      services: &RuntimeServices,
      mut req: Request,
  ) -> Result<Response, Report<TrustedServerError>>
  ```

  Update the `RequestInfo::from_request` call (line ~301):
  ```rust
  // Before:
  let request_info = RequestInfo::from_request(&req);

  // After:
  let request_info = RequestInfo::from_request(&req, &services.client_info);
  ```

- [ ] **Step 5: Update `main.rs` to pass `&runtime_services` to `handle_publisher_request`**

  In `crates/trusted-server-adapter-fastly/src/main.rs`, around line 195:
  ```rust
  // Before:
  match handle_publisher_request(settings, integration_registry, req) {

  // After:
  match handle_publisher_request(settings, integration_registry, &runtime_services, req) {
  ```

- [ ] **Step 6: Update `prebid.rs` two `RequestInfo::from_request` call sites**

  In `crates/trusted-server-core/src/integrations/prebid.rs`:

  Line ~713:
  ```rust
  // Before:
  let request_info = RequestInfo::from_request(context.request);

  // After:
  let request_info = RequestInfo::from_request(context.request, context.client_info);
  ```

  Line ~1011:
  ```rust
  // Before:
  let request_info = RequestInfo::from_request(context.request);

  // After:
  let request_info = RequestInfo::from_request(context.request, context.client_info);
  ```

- [ ] **Step 7: Run tests to verify Task 2 compiles and passes**

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass including the new TLS test.

- [ ] **Step 8: Commit Task 2**

  ```bash
  git add crates/trusted-server-core/src/http_util.rs \
          crates/trusted-server-core/src/publisher.rs \
          crates/trusted-server-core/src/integrations/prebid.rs \
          crates/trusted-server-adapter-fastly/src/main.rs
  git commit -m "Change RequestInfo::from_request to take &ClientInfo, thread services into handle_publisher_request"
  ```

---

## Task 3: Add `services` to `generate_synthetic_id` and fix all callers including `formats.rs` geo

This task changes `synthetic.rs` and simultaneously fixes all 4 callers. `formats.rs` also gets the `geo` parameter so DeviceInfo.geo no longer uses the deprecated call.

**Files:**
- Modify: `crates/trusted-server-core/src/synthetic.rs:96-99, 216-218`
- Modify: `crates/trusted-server-core/src/auction/formats.rs:82-88, 91, 136-143`
- Modify: `crates/trusted-server-core/src/auction/endpoints.rs:10-13, 52, 61-68, 71-72`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs:662`
- Modify: `crates/trusted-server-core/src/publisher.rs:328`

- [ ] **Step 1: Update `synthetic.rs` — add `services: &RuntimeServices` to both functions**

  In `crates/trusted-server-core/src/synthetic.rs`, add `RuntimeServices` to imports:
  ```rust
  use crate::platform::RuntimeServices;
  ```

  Change `generate_synthetic_id` (line ~96):
  ```rust
  // Before:
  pub fn generate_synthetic_id(
      settings: &Settings,
      req: &Request,
  ) -> Result<String, Report<TrustedServerError>>

  // After:
  pub fn generate_synthetic_id(
      settings: &Settings,
      services: &RuntimeServices,
      req: &Request,
  ) -> Result<String, Report<TrustedServerError>>
  ```

  Inside the function, replace line ~100:
  ```rust
  // Before:
  let client_ip = req.get_client_ip_addr().map(normalize_ip);

  // After:
  let client_ip = services.client_info.client_ip.map(normalize_ip);
  ```

  Change `get_or_generate_synthetic_id` (line ~216):
  ```rust
  // Before:
  pub fn get_or_generate_synthetic_id(
      settings: &Settings,
      req: &Request,
  ) -> Result<String, Report<TrustedServerError>>

  // After:
  pub fn get_or_generate_synthetic_id(
      settings: &Settings,
      services: &RuntimeServices,
      req: &Request,
  ) -> Result<String, Report<TrustedServerError>>
  ```

  Inside `get_or_generate_synthetic_id`, update the `generate_synthetic_id` call:
  ```rust
  // Before:
  let synthetic_id = generate_synthetic_id(settings, req)?;

  // After:
  let synthetic_id = generate_synthetic_id(settings, services, req)?;
  ```

- [ ] **Step 2: Update `synthetic.rs` tests — add `noop_services` import and thread to test calls**

  In the `#[cfg(test)]` module of `synthetic.rs`, add:
  ```rust
  use crate::platform::test_support::noop_services;
  ```

  Update every `generate_synthetic_id(&settings, &req)` call in the test module:
  ```rust
  // Before:
  generate_synthetic_id(&settings, &req)

  // After:
  generate_synthetic_id(&settings, &noop_services(), &req)
  ```

  Update every `get_or_generate_synthetic_id(&settings, &req)` call:
  ```rust
  // Before:
  get_or_generate_synthetic_id(&settings, &req)

  // After:
  get_or_generate_synthetic_id(&settings, &noop_services(), &req)
  ```

- [ ] **Step 3: Update `formats.rs` — add `services` and `geo` params, fix IP and geo extraction**

  In `crates/trusted-server-core/src/auction/formats.rs`, add imports:
  ```rust
  use crate::platform::{GeoInfo, RuntimeServices};
  ```
  (Remove the existing `use crate::geo::GeoInfo;` if present — `GeoInfo` is re-exported from `platform`.)

  Actually check: `formats.rs` currently imports `use crate::geo::GeoInfo;` at line 19. Change to:
  ```rust
  use crate::platform::{GeoInfo, RuntimeServices};
  ```

  Change `convert_tsjs_to_auction_request` signature:
  ```rust
  // Before:
  pub fn convert_tsjs_to_auction_request(
      body: &AdRequest,
      settings: &Settings,
      req: &Request,
      consent: ConsentContext,
      synthetic_id: &str,
  ) -> Result<AuctionRequest, Report<TrustedServerError>>

  // After:
  pub fn convert_tsjs_to_auction_request(
      body: &AdRequest,
      settings: &Settings,
      req: &Request,
      services: &RuntimeServices,
      geo: Option<GeoInfo>,
      consent: ConsentContext,
      synthetic_id: &str,
  ) -> Result<AuctionRequest, Report<TrustedServerError>>
  ```

  Update the `generate_synthetic_id` call (line ~91):
  ```rust
  // Before:
  let fresh_id =
      generate_synthetic_id(settings, req).change_context(TrustedServerError::Auction {
          message: "Failed to generate fresh ID".to_string(),
      })?;

  // After:
  let fresh_id =
      generate_synthetic_id(settings, services, req).change_context(TrustedServerError::Auction {
          message: "Failed to generate fresh ID".to_string(),
      })?;
  ```

  Replace `DeviceInfo` construction (lines ~136-143):
  ```rust
  // Before:
  let device = Some(DeviceInfo {
      user_agent: req
          .get_header_str("user-agent")
          .map(std::string::ToString::to_string),
      ip: req.get_client_ip_addr().map(|ip| ip.to_string()),
      #[allow(deprecated)]
      geo: GeoInfo::from_request(req),
  });

  // After:
  let device = Some(DeviceInfo {
      user_agent: req
          .get_header_str("user-agent")
          .map(std::string::ToString::to_string),
      ip: services.client_info.client_ip.map(|ip| ip.to_string()),
      geo,
  });
  ```

- [ ] **Step 4: Update `endpoints.rs` — thread `services` to synthetic, compute geo, update `convert_tsjs` call**

  In `crates/trusted-server-core/src/auction/endpoints.rs`:

  Remove deprecated geo import — `GeoInfo` is still needed for the `geo` local, update its import path if needed. Check the existing `use crate::geo::GeoInfo;` at line 10 — change to:
  ```rust
  use crate::platform::GeoInfo;
  ```

  Update `get_or_generate_synthetic_id` call (line ~52):
  ```rust
  // Before:
  let synthetic_id = get_or_generate_synthetic_id(settings, &req).change_context(

  // After:
  let synthetic_id = get_or_generate_synthetic_id(settings, services, &req).change_context(
  ```

  Replace the deprecated `GeoInfo::from_request` call (lines ~60-61) with geo lookup:
  ```rust
  // Before:
  #[allow(deprecated)]
  let geo = GeoInfo::from_request(&req);

  // After:
  let geo = services
      .geo()
      .lookup(services.client_info.client_ip)
      .unwrap_or_else(|e| {
          log::warn!("geo lookup failed: {e:?}");
          None
      });
  ```

  Update `convert_tsjs_to_auction_request` call (line ~71):
  ```rust
  // Before:
  let auction_request =
      convert_tsjs_to_auction_request(&body, settings, &req, consent_context, &synthetic_id)?;

  // After:
  let auction_request =
      convert_tsjs_to_auction_request(&body, settings, &req, services, geo, consent_context, &synthetic_id)?;
  ```

- [ ] **Step 5: Update `registry.rs` — thread `services` to `get_or_generate_synthetic_id`**

  In `crates/trusted-server-core/src/integrations/registry.rs`, line ~662:
  ```rust
  // Before:
  let synthetic_id_result = get_or_generate_synthetic_id(settings, &req);

  // After:
  let synthetic_id_result = get_or_generate_synthetic_id(settings, services, &req);
  ```

- [ ] **Step 6: Update `publisher.rs` — thread `services` to `get_or_generate_synthetic_id`**

  In `crates/trusted-server-core/src/publisher.rs`, line ~328:
  ```rust
  // Before:
  let synthetic_id = get_or_generate_synthetic_id(settings, &req)?;

  // After:
  let synthetic_id = get_or_generate_synthetic_id(settings, services, &req)?;
  ```

- [ ] **Step 7: Run tests to verify Task 3 compiles and passes**

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass.

- [ ] **Step 8: Commit Task 3**

  ```bash
  git add crates/trusted-server-core/src/synthetic.rs \
          crates/trusted-server-core/src/auction/formats.rs \
          crates/trusted-server-core/src/auction/endpoints.rs \
          crates/trusted-server-core/src/integrations/registry.rs \
          crates/trusted-server-core/src/publisher.rs
  git commit -m "Add services param to generate_synthetic_id, remove Fastly IP/geo calls in formats and endpoints"
  ```

---

## Task 4: Fix `publisher.rs` geo — replace deprecated `GeoInfo::from_request`

`publisher.rs` now has `services` (from Task 2) so this is a straightforward swap.

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs:335-336`

- [ ] **Step 1: Replace `GeoInfo::from_request` in `publisher.rs`**

  In `crates/trusted-server-core/src/publisher.rs`, around line 335:
  ```rust
  // Before:
  #[allow(deprecated)]
  let geo = crate::geo::GeoInfo::from_request(&req);

  // After:
  let geo = services
      .geo()
      .lookup(services.client_info.client_ip)
      .unwrap_or_else(|e| {
          log::warn!("geo lookup failed: {e:?}");
          None
      });
  ```

  Verify the existing `use crate::platform::GeoInfo` is present or the type is not needed by name in `publisher.rs` (the `geo` variable is `Option<GeoInfo>` but GeoInfo may not be used by name). If `GeoInfo` is referenced by name, add the import:
  ```rust
  use crate::platform::GeoInfo;
  ```

- [ ] **Step 2: Check for any remaining `use crate::geo::GeoInfo` in `publisher.rs`**

  If `publisher.rs` still has `use crate::geo::GeoInfo` and it's no longer needed, remove that import.

- [ ] **Step 3: Run tests**

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass.

- [ ] **Step 4: Commit Task 4**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Replace deprecated GeoInfo::from_request in publisher.rs with services.geo().lookup()"
  ```

---

## Task 5: Fix `integrations/didomi.rs` — rename `_services`, update `copy_headers`

**Files:**
- Modify: `crates/trusted-server-core/src/integrations/didomi.rs:101-128, 199-220`

- [ ] **Step 1: Update `copy_headers` signature**

  In `crates/trusted-server-core/src/integrations/didomi.rs`, change the `copy_headers` method:
  ```rust
  // Before:
  fn copy_headers(
      &self,
      backend: &DidomiBackend,
      original_req: &Request,
      proxy_req: &mut Request,
  ) {
      if let Some(client_ip) = original_req.get_client_ip_addr() {
          proxy_req.set_header("X-Forwarded-For", client_ip.to_string());
      }
      // ...

  // After:
  fn copy_headers(
      &self,
      backend: &DidomiBackend,
      client_ip: Option<std::net::IpAddr>,
      original_req: &Request,
      proxy_req: &mut Request,
  ) {
      if let Some(ip) = client_ip {
          proxy_req.set_header("X-Forwarded-For", ip.to_string());
      }
      // ... rest of the method unchanged
  ```

- [ ] **Step 2: Rename `_services` to `services` and update the `copy_headers` call in `handle`**

  In the `handle` method (around line 199), rename `_services` to `services`:
  ```rust
  // Before:
  async fn handle(
      &self,
      _settings: &Settings,
      _services: &RuntimeServices,
      req: Request,
  ) -> Result<Response, Report<TrustedServerError>>

  // After:
  async fn handle(
      &self,
      _settings: &Settings,
      services: &RuntimeServices,
      req: Request,
  ) -> Result<Response, Report<TrustedServerError>>
  ```

  Update the `copy_headers` call inside `handle` (around line 220):
  ```rust
  // Before:
  self.copy_headers(&backend, &req, &mut proxy_req);

  // After:
  self.copy_headers(&backend, services.client_info.client_ip, &req, &mut proxy_req);
  ```

- [ ] **Step 3: Add a focused unit test for `copy_headers`**

  In the `#[cfg(test)]` module of `didomi.rs`, add:

  ```rust
  #[test]
  fn copy_headers_sets_x_forwarded_for_from_client_ip() {
      use std::net::{IpAddr, Ipv4Addr};
      let integration = DidomiIntegration::new(Arc::new(config(true)));
      let backend = DidomiBackend::Sdk;
      let original_req = Request::new(Method::GET, "https://example.com/test");
      let mut proxy_req = Request::new(Method::GET, "https://sdk.privacy-center.org/test");
      let client_ip = Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)));

      integration.copy_headers(&backend, client_ip, &original_req, &mut proxy_req);

      assert_eq!(
          proxy_req
              .get_header("X-Forwarded-For")
              .and_then(|v| v.to_str().ok()),
          Some("1.2.3.4"),
          "should set X-Forwarded-For from client_ip"
      );
  }

  #[test]
  fn copy_headers_omits_x_forwarded_for_when_no_client_ip() {
      let integration = DidomiIntegration::new(Arc::new(config(true)));
      let backend = DidomiBackend::Sdk;
      let original_req = Request::new(Method::GET, "https://example.com/test");
      let mut proxy_req = Request::new(Method::GET, "https://sdk.privacy-center.org/test");

      integration.copy_headers(&backend, None, &original_req, &mut proxy_req);

      assert!(
          proxy_req.get_header("X-Forwarded-For").is_none(),
          "should omit X-Forwarded-For when client_ip is None"
      );
  }
  ```

- [ ] **Step 4: Run tests**

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass including the two new `copy_headers` tests.

- [ ] **Step 5: Commit Task 5**

  ```bash
  git add crates/trusted-server-core/src/integrations/didomi.rs
  git commit -m "Remove Fastly IP extraction from Didomi copy_headers, use ClientInfo instead"
  ```

---

## Task 6: Verify acceptance criteria

- [ ] **Step 1: Verify zero active-code Fastly SDK calls remain**

  ```bash
  grep -rn "get_client_ip_addr\|get_tls_protocol\|get_tls_cipher_openssl_name" \
    crates/trusted-server-core/src/ \
    --include="*.rs"
  ```

  Expected output: only the deprecated function body in `crates/trusted-server-core/src/geo.rs` — no other matches.

- [ ] **Step 2: Verify zero `#[allow(deprecated)]` on `GeoInfo::from_request` call sites**

  ```bash
  grep -rn "allow(deprecated)" crates/trusted-server-core/src/ --include="*.rs"
  ```

  Expected: no results for lines adjacent to `GeoInfo::from_request` calls. Any remaining `#[allow(deprecated)]` should be for unrelated items (e.g., in `nextjs/html_post_process.rs`).

- [ ] **Step 3: Run full CI checks**

  ```bash
  cargo fmt --all -- --check
  ```
  Expected: no formatting issues.

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```
  Expected: no warnings.

  ```bash
  cargo test --workspace
  ```
  Expected: all tests pass.

  ```bash
  cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
  ```
  Expected: wasm32 build succeeds.

- [ ] **Step 4: Commit final verification result**

  No code changes expected in this step. If clippy or fmt found issues, fix them and include in a final commit:

  ```bash
  git add -p  # stage only the clippy/fmt fixes
  git commit -m "Fix clippy and fmt issues from PR7 threading changes"
  ```
