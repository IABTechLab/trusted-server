# PR 6: Backend + HTTP Client Traits — Design

**Issue:** #487
**Part of:** #480 (EdgeZero migration)
**Blocked by:** PR 2 (#545)
**Date:** 2026-03-25

---

## Context

PR 6 is Phase 1, step 6 of the EdgeZero migration. The goal of Phase 1 is to
make `trusted-server-core` platform-neutral by extracting all platform
behaviors behind traits, with Fastly SDK implementations living in
`trusted-server-adapter-fastly`.

PR 2 (#545) introduced `PlatformBackend`, `PlatformHttpClient`, and the full
set of platform HTTP types (`PlatformHttpRequest`, `PlatformResponse`,
`PlatformPendingRequest`, `PlatformSelectResult`). It also added stub
implementations (`FastlyPlatformBackend` with real backend logic,
`FastlyPlatformHttpClient` returning `NotImplemented`). Both are wired into
`RuntimeServices`.

PR 6 completes what PR 2 stubbed: implements the real Fastly HTTP client,
threads `RuntimeServices` into the handlers that need it, and migrates the
direct Fastly SDK calls in `backend.rs`, `proxy.rs`, and
`auction/orchestrator.rs` to go through the trait.

---

## Scope

### What Is Already Done

- `PlatformBackend` trait and `FastlyPlatformBackend` — fully implemented with
  `predict_name` and `ensure` backed by Fastly SDK, including tests
- `PlatformHttpClient` trait — defined; `FastlyPlatformHttpClient` stub exists
  returning `PlatformError::NotImplemented` for all three methods
- `PlatformHttpRequest`, `PlatformResponse` (with `backend_name:
  Option<String>`), `PlatformPendingRequest`, `PlatformSelectResult` — defined
- `PlatformBackendSpec` — defined with `scheme`, `host`, `port`,
  `certificate_check`, `first_byte_timeout`
- Both wired into `RuntimeServices`

### What This PR Adds

#### 1. Implement `FastlyPlatformHttpClient`

Replace the three `NotImplemented` stubs in
`crates/trusted-server-adapter-fastly/src/platform.rs` with real Fastly SDK
calls:

- `send(&self, req)` — converts `PlatformHttpRequest` to `fastly::Request`,
  calls `.send(&req.backend_name)`, converts the response to `PlatformResponse`
  with `backend_name` attached
- `send_async(&self, req)` — calls `.send_async(&req.backend_name)`, wraps the
  resulting `fastly::PendingRequest` in `PlatformPendingRequest`
- `select(&self, pending)` — downcasts each `PlatformPendingRequest` back to
  `fastly::PendingRequest`, calls `fastly::http::request::select()`, wraps the
  result in `PlatformSelectResult`

#### 2. Thread `RuntimeServices` Into Core Handlers

`handle_auction` and all `handle_first_party_*` proxy handlers in
`crates/trusted-server-adapter-fastly/src/main.rs` currently do not receive
`RuntimeServices`. Add `services: &RuntimeServices` to their signatures so
items 3–5 can happen.

#### 3. Migrate `backend.rs` (core)

- Call sites that construct `BackendConfig::new(...).ensure()` are replaced
  with `services.backend.ensure(&PlatformBackendSpec { ... })`
- `BackendConfig` moves from `trusted-server-core/src/backend.rs` to
  `trusted-server-adapter-fastly/src/platform.rs` as an adapter-internal type
  (it is already what `FastlyPlatformBackend::ensure` uses internally to build
  the Fastly backend builder)
- `trusted-server-core/src/backend.rs` no longer imports
  `fastly::backend::Backend`

#### 4. Migrate `proxy.rs` (core)

- Replace `req.send(&backend_name)` with
  `services.http_client.send(PlatformHttpRequest::new(req, backend_name))`
- `proxy.rs` no longer imports `fastly::Request` for the send path

#### 5. Migrate `auction/orchestrator.rs` (core)

- Replace `fastly::http::request::select()` + `PendingRequest` with
  `services.http_client.send_async()` and `services.http_client.select()`
- `orchestrator.rs` no longer imports `fastly::http::request::{select,
  PendingRequest}`

#### 6. Resolve `BackendConfig` / `PlatformBackendSpec` Overlap

`BackendConfig<'a>` (borrowed string slices) and `PlatformBackendSpec` (owned
`String` fields) carry identical fields. After the migration:

- Core callers construct `PlatformBackendSpec` directly and pass it to
  `services.backend.ensure()`
- `BackendConfig` moves to the adapter as an internal bridge type; a
  `From<&PlatformBackendSpec> for BackendConfig` conversion is added
- `BackendConfig` is no longer part of the public API of
  `trusted-server-core`

#### 7. File EdgeZero Issue

Before this PR merges, file an EdgeZero issue to generalize `ProxyClient` into
an `HttpClient` trait supporting both synchronous proxy-style sends and the
async fan-out pattern (`send_async` + `select`). The trusted-server
`PlatformHttpClient` Fastly implementation works independently until the
generalized EdgeZero trait lands, at which point the Fastly impl swaps to
implementing the EdgeZero trait.

---

## Files Changed

| File | Change |
|---|---|
| `crates/trusted-server-adapter-fastly/src/platform.rs` | Implement `FastlyPlatformHttpClient`; move `BackendConfig` here; add adapter tests |
| `crates/trusted-server-adapter-fastly/src/main.rs` | Thread `RuntimeServices` into `handle_auction` and proxy handlers |
| `crates/trusted-server-core/src/backend.rs` | Remove `fastly::backend::Backend` import; callers use `services.backend` |
| `crates/trusted-server-core/src/proxy.rs` | Replace direct `.send()` with `services.http_client.send()` |
| `crates/trusted-server-core/src/auction/orchestrator.rs` | Replace `select`/`PendingRequest` with `services.http_client` equivalents |
| `crates/trusted-server-core/src/platform/test_support.rs` | Add `StubHttpClient` with canned response support |

---

## Testing Strategy

### `FastlyPlatformHttpClient` (adapter unit tests)

`send`, `send_async`, and `select` require a live Fastly backend to return
success responses and cannot be exercised in `cargo test` unit tests without
Viceroy. Tests confirm each method fails gracefully (returning
`PlatformError::HttpClient`) when called with a non-existent backend in the
test environment — same pattern as the existing
`fastly_platform_http_client_reports_not_implemented` test.

### Core Migration Tests (using `StubHttpClient`)

A `StubHttpClient` is added to `platform::test_support` implementing
`PlatformHttpClient`. It records calls and returns canned `PlatformResponse`
values. Tests verify:

- `proxy.rs` proxy path calls `services.http_client.send()` with the correct
  backend name and request
- `orchestrator.rs` parallel path calls `services.http_client.send_async()`
  once per provider and `services.http_client.select()` to collect results

### `FastlyPlatformBackend`

Existing tests for `predict_name` and `ensure` are sufficient; no new backend
tests are needed unless the `BackendConfig` consolidation changes observable
behavior.

---

## Done When

- `FastlyPlatformHttpClient::send`, `send_async`, and `select` are backed by
  Fastly SDK
- `trusted-server-core` has no direct `fastly::backend::Backend` construction
  or `fastly::Request::send` / `fastly::http::request::select` calls in
  `backend.rs`, `proxy.rs`, or `auction/orchestrator.rs`
- `BackendConfig` is adapter-internal; `PlatformBackendSpec` is the single
  public type for backend configuration
- EdgeZero issue filed for ProxyClient → HttpClient generalization
- The three test categories above exist and pass
- `cargo test --workspace`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `cargo fmt --all -- --check` all pass

---

## Explicitly Out of Scope

- Integration modules (`lockr.rs`, `prebid.rs`, etc.) — their `send()` /
  `send_async()` calls remain on the Fastly SDK until PR 13
- `AuctionContext` signature changes — PR 12.5
- Any changes to other `trusted-server-core` modules not listed above
