# PR 7 Design — Geo Lookup + Client Info (Extract-Once)

> Phase 1, PR 7 of the EdgeZero migration.
> Implements [#488](https://github.com/IABTechLab/trusted-server/issues/488),
> part of [#480](https://github.com/IABTechLab/trusted-server/issues/480).
> Blocked by PR 2.

---

## Goal

Eliminate redundant per-call-site extraction of client IP and TLS metadata
from the Fastly request object inside `trusted-server-core`. After this PR,
every function that previously called `req.get_client_ip_addr()`,
`req.get_tls_protocol()`, or `req.get_tls_cipher_openssl_name()` reads from
`services.client_info` instead. The Fastly SDK extraction happens exactly once
— in `build_runtime_services()` at the adapter entry point.

---

## Baseline (already done in PR 6)

These are in place and not touched by PR 7:

- `ClientInfo` struct in `platform/types.rs` with `client_ip: Option<IpAddr>`,
  `tls_protocol: Option<String>`, `tls_cipher: Option<String>`
- `RuntimeServices.client_info: ClientInfo` field + builder support
- `FastlyPlatformGeo` implementation in
  `trusted-server-adapter-fastly/src/platform.rs`
- `build_runtime_services()` already extracts all three `ClientInfo` fields
  from the Fastly request
- `main.rs` geo lookup already uses
  `services.geo().lookup(services.client_info.client_ip)` — that call site
  is complete

---

## Architecture

### Core principle

`ClientInfo` is populated exactly once in `build_runtime_services()` at the
adapter entry point. Every downstream function that currently extracts client
metadata from the Fastly request instead reads from `services.client_info`.
The platform-specific extraction never leaves the adapter crate.

### Injection pattern

Follows the Phase 1 doc pattern: internal utility functions that currently
call Fastly SDK methods gain a `services: &RuntimeServices` parameter where
possible. Two exceptions:

- `RequestInfo::from_request` takes `&ClientInfo` (not `&RuntimeServices`) so
  it remains callable from both `publisher.rs` (which has `services`) and
  `prebid.rs` (which only has `AuctionContext.client_info`).
- `didomi.rs` `copy_headers` takes `Option<IpAddr>` directly — a private
  helper only needs the scalar value.

Callers already hold `RuntimeServices` and thread it through — no new
construction or allocation.

**One exception — `AuctionContext`:** This struct sits between handlers and
auction providers (prebid, APS). PR 12.5 ("Thread RuntimeServices into
integrations") is the designated PR for adding full `&RuntimeServices` to that
layer. PR 7 adds only `client_info: &'a ClientInfo` to `AuctionContext` —
the minimum to fix the two prebid `RequestInfo::from_request` call sites
without stepping on PR 12.5's scope.

### No new traits or structs

This PR is purely plumbing. `ClientInfo`, `PlatformGeo`, `RuntimeServices`,
and all traits are already defined. PR 7 only removes Fastly SDK calls from
core by threading existing abstractions to the remaining call sites.

---

## File-by-File Changes

### `crates/trusted-server-core/src/synthetic.rs`

**Current:** `generate_synthetic_id(settings, req: &Request)` calls
`req.get_client_ip_addr()` on line 100 of `synthetic.rs`.
`get_or_generate_synthetic_id` calls `generate_synthetic_id`.

**Change:** Add `services: &RuntimeServices` parameter to both functions.
Replace `req.get_client_ip_addr()` with `services.client_info.client_ip`.
The `req: &Request` parameter stays — still needed for `User-Agent`,
`Accept-Language`, `Accept-Encoding` headers and cookie reading.

```rust
// Before
pub fn generate_synthetic_id(
    settings: &Settings,
    req: &Request,
) -> Result<String, Report<TrustedServerError>>

// After
pub fn generate_synthetic_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request,
) -> Result<String, Report<TrustedServerError>>
// Inside: let client_ip = services.client_info.client_ip.map(normalize_ip);
```

**Callers that update:** `publisher.rs`, `auction/endpoints.rs`,
`auction/formats.rs`, `integrations/registry.rs`.

---

### `crates/trusted-server-core/src/http_util.rs`

**Current:** `RequestInfo::from_request(req)` calls private
`detect_request_scheme(req)`, which calls `req.get_tls_protocol()` (line 168)
and `req.get_tls_cipher_openssl_name()` (line 174) to determine HTTPS.

**Change:** `RequestInfo::from_request(req, client_info)` — `detect_request_scheme`
gains `tls_protocol: Option<&str>` and `tls_cipher: Option<&str>` parameters
instead of calling the SDK. The `req: &Request` parameter stays for host
extraction and the forwarded/`X-Forwarded-Proto`/`Fastly-SSL` header fallbacks.

Taking `&ClientInfo` (not `&RuntimeServices`) keeps the signature usable from
both `publisher.rs` (which has `&services.client_info`) and `prebid.rs`
(which only has `context.client_info: &ClientInfo`).

```rust
// Before
pub fn from_request(req: &Request) -> Self

// After
pub fn from_request(req: &Request, client_info: &ClientInfo) -> Self
// passes client_info.tls_protocol.as_deref()
//         client_info.tls_cipher.as_deref()
// into detect_request_scheme
```

`detect_request_scheme` remains private. Its signature becomes:

```rust
fn detect_request_scheme(
    req: &Request,
    tls_protocol: Option<&str>,
    tls_cipher: Option<&str>,
) -> String
```

**Test updates:** There are 8 `RequestInfo::from_request` call sites in the
`http_util.rs` test module (lines 398, 416, 429, 440, 459, 475, 494, 552).
All must pass a zero-filled `ClientInfo`. Add the following import to the
`#[cfg(test)]` module if `ClientInfo` is not already in scope:

```rust
use crate::platform::ClientInfo;
```

Each call site becomes:

```rust
RequestInfo::from_request(&req, &ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None })
```

Add one new test: TLS-detected HTTPS using a `ClientInfo` with
`tls_protocol: Some("TLSv1.3".to_string())`, confirming that
`detect_request_scheme` returns `"https"` when the protocol is set in
`ClientInfo` rather than from the Fastly SDK call.

---

### `crates/trusted-server-core/src/integrations/didomi.rs`

**Current:** `copy_headers` calls `original_req.get_client_ip_addr()` (line 107)
to set `X-Forwarded-For`. The `handle` method already has `_services:
&RuntimeServices` (currently unused — note the underscore).

**Change:** `copy_headers` gains `client_ip: Option<IpAddr>` as a parameter.
In `handle`, rename the existing `_services` parameter to `services` (remove
the `_` prefix — do not add a new parameter). Pass `services.client_info.client_ip`
to `copy_headers`.

`copy_headers` is a private method and only needs the IP value — passing
`Option<IpAddr>` directly is cleaner than full `services` in an internal
helper.

```rust
// Before
fn copy_headers(
    &self,
    backend: &DidomiBackend,
    original_req: &Request,
    proxy_req: &mut Request,
)
// inside: original_req.get_client_ip_addr()

// After
fn copy_headers(
    &self,
    backend: &DidomiBackend,
    client_ip: Option<IpAddr>,
    original_req: &Request,
    proxy_req: &mut Request,
)
// inside: client_ip  (no SDK call)
// caller in handle():
//   self.copy_headers(&backend, services.client_info.client_ip, &req, &mut proxy_req)
```

---

### `crates/trusted-server-core/src/auction/formats.rs`

**Current:** `convert_tsjs_to_auction_request` calls:

- `generate_synthetic_id(settings, req)` at line 91 — produces `fresh_id`
  for `UserInfo`, uses Fastly IP extraction internally
- `req.get_client_ip_addr()` (line 140) for `DeviceInfo.ip`
- `GeoInfo::from_request(req)` (deprecated, line 142) for `DeviceInfo.geo`

Note: `generate_synthetic_id` is called once in `formats.rs`, at line 91, to
produce `fresh_id` for `UserInfo`. The `DeviceInfo.ip` fix at line 140 is a
separate `req.get_client_ip_addr()` call that is addressed independently by
reading `services.client_info.client_ip`.

**Change:** Add `services: &RuntimeServices` and `geo: Option<GeoInfo>`
parameters. Thread `services` into the `generate_synthetic_id` call at line 91
to fix `fresh_id` generation. Replace the separate `req.get_client_ip_addr()`
call at line 140 with `services.client_info.client_ip` for `DeviceInfo.ip`.
Use the `geo` parameter for `DeviceInfo.geo`. Remove the `#[allow(deprecated)]`
annotation.

```rust
// Before
pub fn convert_tsjs_to_auction_request(
    body: &AdRequest,
    settings: &Settings,
    req: &Request,
    consent: ConsentContext,
    synthetic_id: &str,
) -> Result<AuctionRequest, Report<TrustedServerError>>

// After
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

The `geo` parameter is computed by the caller (`auction/endpoints.rs`) via
`services.geo().lookup(services.client_info.client_ip)` and passed in. This
avoids a second geo lookup inside `formats.rs`.

---

### `crates/trusted-server-core/src/auction/endpoints.rs`

**Current:** Already has `services: &RuntimeServices`. Calls:

- `get_or_generate_synthetic_id(settings, &req)` — Fastly IP extraction
- `GeoInfo::from_request(&req)` (deprecated, line 61)
- `convert_tsjs_to_auction_request(body, settings, &req, consent, &synthetic_id)`

**Change:**

1. Replace `get_or_generate_synthetic_id(settings, &req)` with
   `get_or_generate_synthetic_id(settings, services, &req)`.

2. Replace `GeoInfo::from_request(&req)` with
   `services.geo().lookup(services.client_info.client_ip)`. Handle the
   `Result` with `unwrap_or_else(|e| { log::warn!(...); None })` — same
   pattern as `main.rs`. Remove `#[allow(deprecated)]`.

3. Pass `services` and `geo` into `convert_tsjs_to_auction_request`.

4. Set `AuctionContext.client_info = &services.client_info` (new field,
   see below).

The geo value is computed once and used for both `consent::build_consent_context`
and `convert_tsjs_to_auction_request` — no double lookup.

---

### `crates/trusted-server-core/src/publisher.rs`

**Current:** `handle_publisher_request(settings, integration_registry, req)`
calls:

- `RequestInfo::from_request(&req)` — TLS SDK extraction
- `get_or_generate_synthetic_id(settings, &req)` — IP SDK extraction
- `GeoInfo::from_request(&req)` (deprecated, line 336)

**Change:** Add `services: &RuntimeServices` parameter. Thread to all three
call sites:

1. `RequestInfo::from_request(&req)` → `RequestInfo::from_request(&req, &services.client_info)`
2. `get_or_generate_synthetic_id(settings, &req)` → `get_or_generate_synthetic_id(settings, services, &req)`
3. Replace deprecated geo call with `services.geo().lookup(services.client_info.client_ip)`.
   Handle the `Result` with warn-and-continue — same pattern as `main.rs`:
   ```rust
   let geo = services.geo().lookup(services.client_info.client_ip)
       .unwrap_or_else(|e| { log::warn!("geo lookup failed: {e:?}"); None });
   ```
   Remove `#[allow(deprecated)]`.

```rust
// Before
pub fn handle_publisher_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    req: Request,
) -> Result<Response, Report<TrustedServerError>>

// After
pub fn handle_publisher_request(
    settings: &Settings,
    integration_registry: &IntegrationRegistry,
    services: &RuntimeServices,
    req: Request,
) -> Result<Response, Report<TrustedServerError>>
```

---

### `crates/trusted-server-core/src/auction/types.rs` — `AuctionContext`

**Current:**

```rust
pub struct AuctionContext<'a> {
    pub settings: &'a Settings,
    pub request: &'a Request,
    pub timeout_ms: u32,
    pub provider_responses: Option<&'a [AuctionResponse]>,
}
```

**Change:** Add `client_info: &'a ClientInfo`.

```rust
pub struct AuctionContext<'a> {
    pub settings: &'a Settings,
    pub request: &'a Request,
    pub timeout_ms: u32,
    pub provider_responses: Option<&'a [AuctionResponse]>,
    pub client_info: &'a ClientInfo,   // new in PR 7
}
```

**All `AuctionContext` construction sites** — every site must add
`client_info: &services.client_info` (production) or propagate an existing
`client_info` reference (derived contexts):

| File                      | Line  | Type                                      | Change                                                            |
| ------------------------- | ----- | ----------------------------------------- | ----------------------------------------------------------------- |
| `auction/endpoints.rs`    | ~75   | production                                | `client_info: &services.client_info`                              |
| `auction/orchestrator.rs` | ~145  | production                                | `client_info: context.client_info` (copy from incoming `context`) |
| `auction/orchestrator.rs` | ~321  | production                                | `client_info: context.client_info` (copy from incoming `context`) |
| `auction/orchestrator.rs` | ~677  | test helper `create_test_context`         | add `client_info: &ClientInfo` param, thread through              |
| `integrations/prebid.rs`  | ~1287 | test helper `create_test_auction_context` | add `client_info: &ClientInfo` param, thread through              |
| `integrations/prebid.rs`  | ~2671 | test helper `call_to_openrtb`             | add `client_info: &ClientInfo` param, thread through              |

The three test helpers need a `client_info: &ClientInfo` parameter added, and
all callers of those helpers must pass `&ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None }`.

Example for `auction/endpoints.rs`:

```rust
let context = AuctionContext {
    settings,
    request: &req,
    timeout_ms: settings.auction.timeout_ms,
    provider_responses: None,
    client_info: &services.client_info,  // new
};
```

**Why `&ClientInfo`, not `&RuntimeServices`:** PR 12.5 ("Thread
RuntimeServices into integrations") is the designated PR for adding full
services access to the auction provider layer. Adding only `client_info` here
keeps PR 7 minimal and avoids overlap.

---

### `crates/trusted-server-core/src/integrations/prebid.rs`

**Current:** Two call sites:

- Line 713: `RequestInfo::from_request(context.request)`
- Line 1011: `RequestInfo::from_request(context.request)`

**Change:** Both become `RequestInfo::from_request(context.request, context.client_info)`.
No other prebid changes in this PR.

---

### `crates/trusted-server-core/src/integrations/registry.rs`

**Current:** `handle_proxy` calls `get_or_generate_synthetic_id(settings, &req)`.
Already has `services: &RuntimeServices`.

**Change:** Pass `services` through:
`get_or_generate_synthetic_id(settings, services, &req)`.

---

### `crates/trusted-server-adapter-fastly/src/main.rs`

Two changes:

1. Pass `&runtime_services` to `handle_publisher_request`. The call is inside
   the `route_request` function's fallback `match` arm; `runtime_services` is
   already in scope there.

```rust
// Before (inside route_request fallback arm, line ~195)
match handle_publisher_request(settings, integration_registry, req) {

// After
match handle_publisher_request(settings, integration_registry, &runtime_services, req) {
```

2. No geo lookup changes — already uses `services.geo().lookup(...)`.

---

## What Does NOT Change

- `geo.rs` — `GeoInfo::from_request` stays (deprecated marker stays),
  `geo_from_fastly`, `set_response_headers`, GDPR helpers — all unchanged
- `trusted-server-adapter-fastly/src/platform.rs` — no changes
- `platform/` module — no new traits or structs
- All integrations except `didomi.rs` and `prebid.rs` — unchanged
- `proxy.rs`, `auth.rs`, `consent/`, `cookies.rs` — unchanged

---

## Testing

| File                      | Test change                                                                                                                                                                                                                                 |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `synthetic.rs`            | Pass `noop_services()` to existing tests; add `use crate::platform::test_support::noop_services;` to `#[cfg(test)]` module; tests still pass `req`                                                                                          |
| `http_util.rs`            | Pass `&ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None }` to all `RequestInfo::from_request` calls (8 sites); add one new test for TLS-detected HTTPS via `ClientInfo { tls_protocol: Some("TLSv1.3".to_string()), .. }` |
| `auction/formats.rs`      | **No test module exists** — no test updates needed in this file                                                                                                                                                                             |
| `didomi.rs`               | Pass `client_ip: None` to `copy_headers` in any existing tests                                                                                                                                                                              |
| `auction/endpoints.rs`    | **No test module exists** — no test updates needed in this file                                                                                                                                                                             |
| `publisher.rs`            | Pass `noop_services()` to existing publisher tests                                                                                                                                                                                          |
| `auction/orchestrator.rs` | Update `create_test_context` helper to accept and thread `client_info: &ClientInfo`; all callers pass `&ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None }`                                                               |
| `integrations/prebid.rs`  | Update `create_test_auction_context` and `call_to_openrtb` test helpers to accept and thread `client_info: &ClientInfo`; all callers pass `&ClientInfo { client_ip: None, tls_protocol: None, tls_cipher: None }`                           |

All existing tests must continue to pass. No behavior changes — only extraction
source changes (from Fastly SDK calls to `ClientInfo` fields that contain the
same values).

---

## Acceptance Criteria

- [ ] Zero `req.get_client_ip_addr()` calls in active (non-deprecated) code in `trusted-server-core` (the deprecated body of `GeoInfo::from_request` in `geo.rs` is excluded — that body stays and is covered by the `#[deprecated]` marker itself)
- [ ] Zero `req.get_tls_protocol()` calls in active (non-deprecated) code in `trusted-server-core`
- [ ] Zero `req.get_tls_cipher_openssl_name()` calls in active (non-deprecated) code in `trusted-server-core`
- [ ] Zero `#[allow(deprecated)]` on `GeoInfo::from_request` calls (the `#[deprecated]` attribute on `GeoInfo::from_request` itself is preserved — only the call-site suppressors are removed; unrelated `#[allow(deprecated)]` annotations in `nextjs/html_post_process.rs` are for a different deprecated function and are out of scope for this PR)
- [ ] `ClientInfo` populated at entry point (PR6 ✅, PR7 verifies no regressions)
- [ ] All production client metadata originates from `RuntimeServices.client_info` (provider-layer reads happen via `AuctionContext.client_info`, which is populated from `&services.client_info` at the endpoint layer)
- [ ] CI gates pass: `cargo build --workspace`, wasm32 build, `cargo test --workspace`, clippy `-D warnings`, `cargo fmt`

---

## Migration Path Alignment

After this PR, `trusted-server-core` no longer extracts client IP or TLS
metadata from `fastly::Request`. The Fastly-specific extraction is fully
contained in `build_runtime_services()` in the adapter crate. When Phase 2
(PR 11-13) replaces `fastly::Request` with `http::Request` throughout core,
these utility functions (`generate_synthetic_id`, `RequestInfo::from_request`,
etc.) already read from `services.client_info` and require zero further
changes for client metadata. The EdgeZero adapter (PR 14-15) will populate
`ClientInfo` from whatever mechanism the EdgeZero framework provides (request
extensions, worker context, etc.) — no core changes needed.
