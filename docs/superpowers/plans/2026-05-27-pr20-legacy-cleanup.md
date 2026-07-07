# PR20 — Legacy Entry Point Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete `legacy_main()`, the canary routing machinery (`edgezero_enabled`/`edgezero_rollout_pct` flag plumbing), and all code that only existed to support the legacy path — completing issue #501.

**Architecture:** All traffic now flows through `edgezero_main()`. The entry point `main()` becomes a thin trampoline: fast-path health check → logging init → call `edgezero_main()`. The canary config-store flag read is gone; the config store is opened once inside `edgezero_main()` (required for `dispatch_with_config_handle`). Dead test coverage for `route_request()` and `HandlerOutcome` is removed; equivalent EdgeZero-path coverage already lives in `app.rs`.

**Tech Stack:** Rust / Fastly Compute (`wasm32-wasip1`), `edgezero-adapter-fastly`, standard `http` crate types.

---

## File Map

| File                                                      | Action            | What changes                                                                                                                                                                                                                                                                                                     |
| --------------------------------------------------------- | ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-adapter-fastly/src/main.rs`        | Modify (major)    | Delete `legacy_main`, `route_request`, `HandlerOutcome`, all canary flag functions/constants/tests, `build_ja4_debug_response`, `finalize_response`, `http_error_response`, `resolve_publisher_response`, FALLBACK\_\* constants; simplify `main()` and `edgezero_main()`; remove `mod error;`; clean up imports |
| `crates/trusted-server-adapter-fastly/src/route_tests.rs` | **Delete**        | Entire file — all tests reference deleted `route_request`/`HandlerOutcome`; EdgeZero dispatch coverage lives in `app.rs`                                                                                                                                                                                         |
| `crates/trusted-server-adapter-fastly/src/error.rs`       | **Delete**        | Entire file — `to_error_response()` is the only export; it is only called from `legacy_main()`                                                                                                                                                                                                                   |
| `crates/trusted-server-adapter-fastly/src/compat.rs`      | Modify            | Delete `from_fastly_request()`, its private helper `build_http_request()`, and `to_fastly_response_skeleton()` — all legacy-only. Keep `from_fastly_response()`, `to_fastly_response()`, `sanitize_fastly_forwarded_headers()` and their tests. Update module doc comment.                                       |
| `crates/trusted-server-adapter-fastly/src/platform.rs`    | Modify            | Delete `build_runtime_services()`, its `noop_kv_store()` test helper, and its two unit tests — all legacy-only. Update module doc comment.                                                                                                                                                                       |
| `crates/trusted-server-adapter-fastly/src/middleware.rs`  | Modify (doc only) | Remove stale intra-doc links to deleted symbols `finalize_response` and `route_request`                                                                                                                                                                                                                          |
| `crates/trusted-server-adapter-fastly/src/app.rs`         | Modify (doc only) | Remove stale reference to `crate::http_error_response` in `http_error()` doc comment                                                                                                                                                                                                                             |
| `fastly.toml`                                             | Modify            | Remove `edgezero_enabled` and `edgezero_rollout_pct` keys from `[local_server.config_stores.trusted_server_config.contents]`                                                                                                                                                                                     |

**Not touched:** `backend.rs`, `logging.rs`, `management_api.rs`. All other adapter crates (`axum`, `cloudflare`, `spin`) are untouched.

---

## What to delete vs keep in `main.rs`

### Delete — complete functions/types

| Symbol                                     | Lines (approx) | Reason                                                        |
| ------------------------------------------ | -------------- | ------------------------------------------------------------- |
| `const EDGEZERO_ENABLED_KEY`               | 55             | Flag removed                                                  |
| `const EDGEZERO_ROLLOUT_PCT_KEY`           | 56             | Flag removed                                                  |
| `enum HandlerOutcome` + `impl`             | 62-78          | Legacy-path-only type                                         |
| `fn parse_edgezero_flag()`                 | 84-87          | Flag removed                                                  |
| `fn parse_rollout_pct()`                   | 94-100         | Flag removed                                                  |
| `fn fnv1a_bucket()`                        | 108-117        | Flag removed                                                  |
| `fn canary_routes_to_edgezero()`           | 124-131        | Flag removed                                                  |
| `fn is_edgezero_enabled()`                 | 154-159        | Flag removed                                                  |
| `fn read_rollout_pct()`                    | 169-195        | Flag removed                                                  |
| `const FALLBACK_UNAVAILABLE/NOT_SENT/NONE` | 433-435        | JA4 debug only                                                |
| `fn build_ja4_debug_response()`            | 438-479        | `// TODO: remove after JA4 evaluation` — was legacy-path only |
| `fn legacy_main()`                         | 346-431        | THE main deliverable of this PR                               |
| `async fn route_request()`                 | 481-591        | Legacy-path-only dispatcher                                   |
| `fn resolve_publisher_response()`          | 593-612        | Called only by `route_request()`                              |
| `fn finalize_response()`                   | 650-652        | Thin wrapper used only by `legacy_main()`                     |
| `fn http_error_response()`                 | 654-666        | Legacy-path-only; EdgeZero path uses `app::http_error()`      |

### Delete — `mod` declaration and import

| Symbol                                 | Where            | Reason                      |
| -------------------------------------- | ---------------- | --------------------------- |
| `mod error;`                           | main.rs ~line 41 | `error.rs` is being deleted |
| `use crate::error::to_error_response;` | main.rs ~line 50 | `error.rs` is being deleted |

### Delete — tests in `mod tests`

| Test name                                                | Reason                              |
| -------------------------------------------------------- | ----------------------------------- |
| `parses_true_flag_values`                                | `parse_edgezero_flag` deleted       |
| `rejects_non_true_flag_values`                           | `parse_edgezero_flag` deleted       |
| `parses_valid_rollout_percentages`                       | `parse_rollout_pct` deleted         |
| `rejects_invalid_rollout_percentages`                    | `parse_rollout_pct` deleted         |
| `bucket_is_in_range_0_to_99`                             | `fnv1a_bucket` deleted              |
| `bucket_is_deterministic`                                | `fnv1a_bucket` deleted              |
| `bucket_matches_known_fnv1a_vector`                      | `fnv1a_bucket` deleted              |
| `bucket_distributes_across_range`                        | `fnv1a_bucket` deleted              |
| `empty_key_bucket_is_valid`                              | `fnv1a_bucket` deleted              |
| `rollout_zero_routes_all_to_legacy`                      | `canary_routes_to_edgezero` deleted |
| `rollout_hundred_routes_all_to_edgezero`                 | `canary_routes_to_edgezero` deleted |
| `rollout_fifty_routes_exactly_half_of_bucket_space`      | `canary_routes_to_edgezero` deleted |
| `rollout_one_routes_exactly_one_bucket`                  | `canary_routes_to_edgezero` deleted |
| `ja4_debug_response_uses_plain_text_and_fallback_values` | `build_ja4_debug_response` deleted  |

### Keep — functions (unchanged or slightly updated)

| Symbol                                                        | Notes                                                                    |
| ------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `const TRUSTED_SERVER_CONFIG_STORE`                           | Still used by `open_trusted_server_config_store()`                       |
| `fn open_trusted_server_config_store()`                       | Kept; called inside simplified `edgezero_main()`. Update doc comment.    |
| `fn health_response()`                                        | Kept; fast-path health probe in `main()`                                 |
| `fn edgezero_main()`                                          | Kept; signature changes: no `config_store` param; opens store internally |
| `fn response_was_finalized_by_middleware()`                   | Kept; used by `edgezero_main()`                                          |
| `fn apply_entry_point_finalize()`                             | Kept; used by `edgezero_main()`                                          |
| `pub(crate) fn resolve_publisher_response_buffered()`         | Kept; called by `app.rs::dispatch_fallback()`                            |
| Tests: `health_response_*`                                    | Kept                                                                     |
| Tests: `response_was_finalized_by_middleware_strips_sentinel` | Kept                                                                     |
| Tests: `entry_point_finalize_skips_geo_lookup_for_401`        | Kept                                                                     |

### Imports to remove from `main.rs`

After all deletions, these become unused (let the compiler confirm):

```rust
// Delete entirely:
use trusted_server_core::auction::endpoints::handle_auction;
use trusted_server_core::auction::AuctionOrchestrator;
use trusted_server_core::auth::enforce_basic_auth;
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::request_signing::{
    handle_deactivate_key, handle_rotate_key, handle_trusted_server_discovery,
    handle_verify_signature,
};

// Trim these lines (keep only what's shown):

// Was: use crate::app::{build_state, runtime_services_for_consent_route, TrustedServerApp};
// build_state: only called from legacy_main; runtime_services_for_consent_route: only called from
// route_request (deleted). TrustedServerApp is still needed by edgezero_main.
use crate::app::TrustedServerApp;

// Was: use crate::platform::{build_runtime_services, FastlyPlatformGeo};
use crate::platform::FastlyPlatformGeo;

// Was: use edgezero_core::http::{header, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse};
// Method and Request: only used by route_request (deleted).
use edgezero_core::http::{header, HeaderValue, Response as HttpResponse};

// Was: use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
// IntoHttpResponse: only in route_tests (deleted). TrustedServerError: used by
// resolve_publisher_response_buffered. Keep TrustedServerError only.
use trusted_server_core::error::TrustedServerError;

// Was: use trusted_server_core::proxy::{handle_first_party_click, ...};
// DELETE ENTIRE LINE — no proxy handlers remain in main.rs

// Was: use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic,
//     stream_publisher_body, OwnedProcessResponseParams, PublisherResponse};
// handle_publisher_request, handle_tsjs_dynamic: only in route_request (deleted).
use trusted_server_core::publisher::{stream_publisher_body, OwnedProcessResponseParams, PublisherResponse};
```

Also in `mod tests`: remove `use fastly::mime;` (only for ja4 test, which is deleted).

`std::net::IpAddr`, `std::sync::Arc`, `edgezero_core::body::Body as EdgeBody`, `fastly::http::Method as FastlyMethod`, `fastly::{Request as FastlyRequest, Response as FastlyResponse}`, `edgezero_adapter_fastly::FastlyConfigStore`, `edgezero_core::config_store::ConfigStoreHandle`, `error_stack::Report`, `edgezero_core::app::Hooks as _` all remain.

### Simplified `main()` — target state

```rust
/// Entry point for the Fastly Compute program.
///
/// Uses an undecorated `main()` with `FastlyRequest::from_client()` instead of
/// `#[fastly::main]` so the EdgeZero streaming publisher path can call
/// [`fastly::Response::stream_to_client`] explicitly.
fn main() {
    let req = FastlyRequest::from_client();

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    logging::init_logger();
    edgezero_main(req);
}
```

### Simplified `edgezero_main()` — target state

```rust
/// Handles a request through the EdgeZero router path.
fn edgezero_main(mut req: FastlyRequest) {
    let config_store = match open_trusted_server_config_store() {
        Ok(cs) => cs,
        Err(e) => {
            log::error!("failed to open config store: {e}");
            FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                .with_body_text_plain("Internal Server Error")
                .send_to_client();
            return;
        }
    };

    let app = TrustedServerApp::build_app();

    // Strip client-spoofable forwarded headers before dispatch.
    compat::sanitize_fastly_forwarded_headers(&mut req);

    // Capture client IP before the request is consumed by dispatch.
    let client_ip = req.get_client_ip_addr();

    // `dispatch_with_config_handle` skips logger initialisation and injects
    // the config store directly (init_logger already called in main()).
    let mut response =
        match edgezero_adapter_fastly::dispatch_with_config_handle(&app, req, config_store) {
            Ok(response) => compat::from_fastly_response(response),
            Err(e) => {
                log::error!("EdgeZero dispatch failed: {e}");
                FastlyResponse::from_status(fastly::http::StatusCode::INTERNAL_SERVER_ERROR)
                    .with_body_text_plain("Internal Server Error")
                    .send_to_client();
                return;
            }
        };

    if !response_was_finalized_by_middleware(&mut response) {
        match get_settings() {
            Ok(settings) => {
                apply_entry_point_finalize(&settings, client_ip, &mut response, |client_ip| {
                    FastlyPlatformGeo.lookup(client_ip).unwrap_or_else(|e| {
                        log::warn!("entry-point geo lookup failed: {e}");
                        None
                    })
                })
            }
            Err(e) => {
                log::warn!("entry-point finalize skipped: failed to reload settings: {e:?}");
            }
        }
    }

    compat::to_fastly_response(response).send_to_client();
}
```

### Updated `open_trusted_server_config_store()` doc comment

```rust
/// Opens the Fastly Config Store used by the EdgeZero dispatcher.
///
/// # Errors
///
/// Returns [`fastly::Error`] if the config store cannot be opened.
fn open_trusted_server_config_store() -> Result<ConfigStoreHandle, fastly::Error> {
```

---

## What to delete vs keep in `compat.rs`

### Delete

| Symbol                                        | Lines | Reason                                              |
| --------------------------------------------- | ----- | --------------------------------------------------- |
| `fn build_http_request()`                     | 11-28 | Private helper only used by `from_fastly_request()` |
| `pub(crate) fn from_fastly_request()`         | 35-38 | Only called from `legacy_main()`                    |
| `pub(crate) fn to_fastly_response_skeleton()` | 78-85 | Only called from `legacy_main()` streaming path     |

### Keep

- `pub(crate) fn from_fastly_response()` — used by `edgezero_main()`
- `pub(crate) fn to_fastly_response()` — used by `edgezero_main()`
- `pub(crate) fn sanitize_fastly_forwarded_headers()` — used by `edgezero_main()`
- Both test functions in `mod tests` — test shared functions above

### Update module doc comment

Replace the current:

```rust
//! Contains only the functions used by the legacy `main()` entry point.
//! Relocated from `trusted-server-core` as part of removing all `fastly` crate
//! imports from the core library.
```

With:

```rust
//! Compatibility bridge between `fastly` SDK types and `http` crate types.
```

---

## What to delete in `platform.rs`

- `pub fn build_runtime_services(...)` — only caller was `legacy_main()`
- `fn noop_kv_store()` test helper — only used by the two `build_runtime_services` tests
- `fn build_runtime_services_client_info_is_none_without_tls()` test
- `fn build_runtime_services_returns_cloneable_services()` test
- Update module doc: remove the sentence "This module also provides `build_runtime_services`, a free function that..."

---

## What to update in `middleware.rs`

Two stale intra-doc links after `finalize_response` and `route_request` are deleted:

**Line ~4 (module doc):**
Remove: `from the legacy [\`crate::finalize_response\`] and [\`crate::route_request\`]:`

**Line ~150 (fn-level doc on `apply_finalize_headers` or equivalent):**
Remove: `Mirrors [\`crate::finalize_response\`] exactly`

Use `grep -n "finalize_response\|route_request" middleware.rs` to find exact lines before editing.

---

## What to remove from `fastly.toml`

The `[local_server.config_stores.trusted_server_config.contents]` block currently has two keys the app no longer reads. Remove them and their comments, leaving the store entry intact (the store itself is still needed for EdgeZero dispatch):

```toml
# Before (remove these lines):
            # "true" / "1" (case-insensitive) enable the EdgeZero path. Missing,
            # unreadable, or any other value falls back to the legacy entry point.
            # Keep "false" until EdgeZero reaches full functional parity with legacy.
            edgezero_enabled = "false"
            # Integer 0-100. Effective only when edgezero_enabled = "true".
            #   0    -> all traffic to legacy (instant rollback — no deploy needed)
            #   1-99 -> canary: clients whose fnv1a_bucket(client_ip) < this value go EdgeZero
            #   100  -> all traffic to EdgeZero (full cutover)
            # Key absent when edgezero_enabled = "true" is treated as 100 (full rollout).
            # IMPORTANT: Set this to "0" in production BEFORE setting edgezero_enabled = "true".
            edgezero_rollout_pct = "0"
```

The resulting `[local_server.config_stores.trusted_server_config.contents]` block should be empty `{}` — the store entry stays because `open_trusted_server_config_store()` still opens it for the EdgeZero dispatcher.

---

## Tasks

> **IMPORTANT about tasks 2-4:** Tasks 2 and 3 leave `main.rs` in a broken-compilation state. Do NOT run `cargo check` between Tasks 2 and 4 except at the explicitly marked checkpoints. Task 4 completes the main.rs rewrite and restores compilability.

---

### Task 1: Delete `route_tests.rs` and its `mod` declaration

**Files:**

- Delete: `crates/trusted-server-adapter-fastly/src/route_tests.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Delete `route_tests.rs`**

```bash
git rm crates/trusted-server-adapter-fastly/src/route_tests.rs
```

- [ ] **Step 2: Remove the `mod route_tests` declaration from `main.rs` (~line 46-47)**

Delete:

```rust
#[cfg(test)]
mod route_tests;
```

- [ ] **Step 3: Verify compiles**

```bash
cargo check -p trusted-server-adapter-fastly
```

Expected: PASS.

- [ ] **Step 4: Run Fastly tests to confirm baseline**

```bash
cargo test-fastly
```

Expected: PASS (tests in `main.rs` and `app.rs` still run).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Remove legacy route_request test file

route_tests.rs tested route_request() and HandlerOutcome, which are
legacy-path only. Equivalent EdgeZero dispatch coverage exists in
app.rs. Part of #501 legacy entry point cleanup."
```

---

### Task 2: Delete `legacy_main()` and all legacy-path-only code from `main.rs`

> Do all deletions in one pass before running `cargo check`. They have cross-references; partial deletion will not compile. The ONLY intentional `cargo check` in this task is at Step 8 — expected to partially fail.

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Delete `HandlerOutcome` enum and its `impl` block (~lines 62-78)**

Delete from the doc comment `/// Result of routing a request...` through the closing `}` of `impl HandlerOutcome`.

- [ ] **Step 2: Delete `legacy_main()` function (~lines 346-431)**

Delete from the doc comment `/// Handles a request using the original Fastly-native entry point...` through the closing `}` of `legacy_main`. This includes the `// TODO: delete after Phase 5...` comment on line 345.

- [ ] **Step 3: Delete the three FALLBACK\_\* constants and `build_ja4_debug_response()` (~lines 433-479)**

Delete:

```rust
const FALLBACK_UNAVAILABLE: &str = "unavailable";
const FALLBACK_NOT_SENT: &str = "not sent";
const FALLBACK_NONE: &str = "none";
```

And the entire `build_ja4_debug_response()` function and doc comment.

- [ ] **Step 4: Delete `route_request()` function (~lines 481-591)**

Delete from `async fn route_request(` through its closing `}`.

- [ ] **Step 5: Delete `resolve_publisher_response()` (~lines 593-612)**

Delete the non-buffered `fn resolve_publisher_response` — NOT `resolve_publisher_response_buffered`.

- [ ] **Step 6: Delete `finalize_response()` thin wrapper (~lines 650-652)**

Delete:

```rust
fn finalize_response(settings: &Settings, geo_info: Option<&GeoInfo>, response: &mut HttpResponse) {
    apply_finalize_headers(settings, geo_info, response);
}
```

- [ ] **Step 7: Delete `http_error_response()` (~lines 654-666)**

Delete from `fn http_error_response(` through its closing `}`.

- [ ] **Step 8: Check current state (expected to partially fail)**

```bash
cargo check -p trusted-server-adapter-fastly 2>&1 | grep "^error" | head -15
```

Expected errors: `main()` still calls `legacy_main()` and canary functions. These are fixed in Tasks 3 and 4.

---

### Task 3: Delete canary flag plumbing from `main.rs`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Delete `EDGEZERO_ENABLED_KEY` and `EDGEZERO_ROLLOUT_PCT_KEY` constants (~lines 55-56)**

Delete:

```rust
const EDGEZERO_ENABLED_KEY: &str = "edgezero_enabled";
const EDGEZERO_ROLLOUT_PCT_KEY: &str = "edgezero_rollout_pct";
```

- [ ] **Step 2: Delete `parse_edgezero_flag()` with its doc comment**

- [ ] **Step 3: Delete `parse_rollout_pct()` with its doc comment**

- [ ] **Step 4: Delete `fnv1a_bucket()` with its doc comment**

- [ ] **Step 5: Delete `canary_routes_to_edgezero()` with its doc comment**

- [ ] **Step 6: Delete `is_edgezero_enabled()` with its doc comment**

- [ ] **Step 7: Delete `read_rollout_pct()` with its doc comment**

---

### Task 4: Simplify `main()` and rewrite `edgezero_main()`

> After this task, `main.rs` compiles cleanly. The remaining tasks clean up other files.

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Replace `main()` body**

Replace the entire current `fn main()` (including its doc comment) with the simplified version from the "Simplified `main()`" section above.

- [ ] **Step 2: Replace `edgezero_main()` signature and update its body**

- Change signature from `fn edgezero_main(mut req: FastlyRequest, config_store: ConfigStoreHandle)` to `fn edgezero_main(mut req: FastlyRequest)`
- Add the `open_trusted_server_config_store()` call at the top (with error handling)
- Remove the old dispatch-succeeded/failed comment about logger initialization avoiding double-init
- Add the new comment from the target state above

Replace `open_trusted_server_config_store()` doc comment with the simplified version above.

- [ ] **Step 3: Verify compiles**

```bash
cargo check -p trusted-server-adapter-fastly
```

Expected: PASS or unused-import warnings only (cleaned up in Task 5).

- [ ] **Step 4: Run Fastly tests**

```bash
cargo test-fastly
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Delete legacy_main, canary routing, and flag plumbing

Remove legacy_main(), route_request(), HandlerOutcome, and all flag-reading
machinery (edgezero_enabled / edgezero_rollout_pct). Entry point is now
a direct trampoline to edgezero_main(), which opens the config store
internally. Part of #501 legacy entry point cleanup."
```

---

### Task 5: Delete legacy-only functions from `compat.rs` and delete `error.rs`

> Both files have functions that were only ever called from `legacy_main()` (now deleted).
> `error.rs` is entirely legacy-only and can be deleted. `compat.rs` has three survivors.

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/compat.rs`
- Delete: `crates/trusted-server-adapter-fastly/src/error.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (remove `mod error;` and its import)

- [ ] **Step 1: Delete `build_http_request()` and `from_fastly_request()` from `compat.rs`**

Delete lines 11-38 — the private `build_http_request()` helper and the `from_fastly_request()` function with its doc comment.

- [ ] **Step 2: Delete `to_fastly_response_skeleton()` from `compat.rs`**

Delete lines 74-85 — the function and its doc comment.

- [ ] **Step 3: Update `compat.rs` module doc comment**

Replace:

```rust
//! Compatibility bridge between `fastly` SDK types and `http` crate types.
//!
//! Contains only the functions used by the legacy `main()` entry point.
//! Relocated from `trusted-server-core` as part of removing all `fastly` crate
//! imports from the core library.
```

With:

```rust
//! Compatibility bridge between `fastly` SDK types and `http` crate types.
```

- [ ] **Step 4: Delete `error.rs`**

```bash
git rm crates/trusted-server-adapter-fastly/src/error.rs
```

- [ ] **Step 5: Remove `mod error;` from `main.rs` (~line 41)**

Delete the line:

```rust
mod error;
```

- [ ] **Step 6: Remove `use crate::error::to_error_response;` from `main.rs` (~line 50)**

Delete the line:

```rust
use crate::error::to_error_response;
```

- [ ] **Step 7: Verify compiles**

```bash
cargo check -p trusted-server-adapter-fastly
```

Expected: PASS or unused-import warnings only.

- [ ] **Step 8: Run Fastly tests**

```bash
cargo test-fastly
```

Expected: PASS. The two `compat.rs` tests still run (they test the surviving functions).

- [ ] **Step 9: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/compat.rs
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Delete legacy-only compat functions and error module

Remove from_fastly_request(), to_fastly_response_skeleton(), and their
build_http_request() helper from compat.rs — all were only called from
legacy_main(). Delete error.rs entirely (to_error_response() was its only
export; only caller was legacy_main()). Part of #501."
```

---

### Task 6: Delete canary tests and strip all unused imports from `main.rs`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

- [ ] **Step 1: Delete canary-related test functions from `mod tests`**

Delete these 14 test functions:

1. `parses_true_flag_values`
2. `rejects_non_true_flag_values`
3. `parses_valid_rollout_percentages`
4. `rejects_invalid_rollout_percentages`
5. `bucket_is_in_range_0_to_99`
6. `bucket_is_deterministic`
7. `bucket_matches_known_fnv1a_vector`
8. `bucket_distributes_across_range`
9. `empty_key_bucket_is_valid`
10. `rollout_zero_routes_all_to_legacy`
11. `rollout_hundred_routes_all_to_edgezero`
12. `rollout_fifty_routes_exactly_half_of_bucket_space`
13. `rollout_one_routes_exactly_one_bucket`
14. `ja4_debug_response_uses_plain_text_and_fallback_values`

Also delete `use fastly::mime;` from the test `use` block — only needed for `ja4` test.

- [ ] **Step 2: Delete unused top-level imports (from the "Imports to remove" section above)**

Delete these entire `use` lines:

- `use trusted_server_core::auction::endpoints::handle_auction;`
- `use trusted_server_core::auction::AuctionOrchestrator;`
- `use trusted_server_core::auth::enforce_basic_auth;`
- `use trusted_server_core::platform::RuntimeServices;`
- `use trusted_server_core::request_signing::{...};`

Trim these lines:

```rust
// Was:
use crate::app::{build_state, runtime_services_for_consent_route, TrustedServerApp};
// Change to:
use crate::app::TrustedServerApp;

// Was:
use crate::platform::{build_runtime_services, FastlyPlatformGeo};
// Change to:
use crate::platform::FastlyPlatformGeo;

// Was:
use edgezero_core::http::{header, HeaderValue, Method, Request as HttpRequest, Response as HttpResponse};
// Change to:
use edgezero_core::http::{header, HeaderValue, Response as HttpResponse};

// Was:
use trusted_server_core::error::{IntoHttpResponse, TrustedServerError};
// Change to:
use trusted_server_core::error::TrustedServerError;

// Delete entire line:
use trusted_server_core::proxy::{handle_first_party_click, handle_first_party_proxy, handle_first_party_proxy_rebuild, handle_first_party_proxy_sign};

// Was:
use trusted_server_core::publisher::{handle_publisher_request, handle_tsjs_dynamic, stream_publisher_body, OwnedProcessResponseParams, PublisherResponse};
// Change to:
use trusted_server_core::publisher::{stream_publisher_body, OwnedProcessResponseParams, PublisherResponse};
```

- [ ] **Step 3: Verify no unused-import warnings**

```bash
cargo check -p trusted-server-adapter-fastly 2>&1 | grep "unused import"
```

Expected: no output. If any warnings remain, remove the flagged import.

- [ ] **Step 4: Run Fastly tests**

```bash
cargo test-fastly
```

Expected: PASS. Surviving tests: `health_response_*`, `response_was_finalized_by_middleware_strips_sentinel`, `entry_point_finalize_skips_geo_lookup_for_401`.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Strip canary tests and unused imports from main.rs

Remove 14 canary-routing test functions and the JA4 debug test.
Strip all imports that were only consumed by deleted code. Part of #501."
```

---

### Task 7: Delete `build_runtime_services` from Fastly `platform.rs`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs`

- [ ] **Step 1: Find exact lines**

```bash
grep -n "build_runtime_services\|fn noop_kv_store" crates/trusted-server-adapter-fastly/src/platform.rs
```

- [ ] **Step 2: Delete `pub fn build_runtime_services(...)` and its doc comment**

- [ ] **Step 3: Delete `fn noop_kv_store()` test helper**

Confirmed: `noop_kv_store()` is only used by the two `build_runtime_services` tests being deleted — safe to remove.

- [ ] **Step 4: Delete the two test functions**

Delete:

- `fn build_runtime_services_client_info_is_none_without_tls()`
- `fn build_runtime_services_returns_cloneable_services()`

- [ ] **Step 5: Update module-level doc comment**

Find and remove the sentence "This module also provides `build_runtime_services`, a free function that..." from the module doc.

- [ ] **Step 6: Verify compiles and tests pass**

```bash
cargo check -p trusted-server-adapter-fastly && cargo test-fastly
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/platform.rs
git commit -m "Remove build_runtime_services from Fastly platform module

build_runtime_services() was the legacy-path per-request service factory.
The EdgeZero path uses build_per_request_services() in app.rs instead.
Remove the dead function, its noop_kv_store() test helper, and its tests.
Part of #501."
```

---

### Task 8: Update stale doc comments in `middleware.rs` and `app.rs`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/middleware.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`

- [ ] **Step 1: Find stale references in `middleware.rs`**

```bash
grep -n "finalize_response\|route_request" crates/trusted-server-adapter-fastly/src/middleware.rs
```

- [ ] **Step 2: Fix module doc in `middleware.rs` (~line 4)**

Remove the phrase `from the legacy [\`crate::finalize_response\`] and [\`crate::route_request\`]:`. Keep the description of what the middleware does; remove only the stale intra-doc links.

- [ ] **Step 3: Fix fn-level doc in `middleware.rs` (~line 150)**

Remove `Mirrors [\`crate::finalize_response\`] exactly` (or rephrase without the broken intra-doc link).

- [ ] **Step 4: Fix `http_error()` doc in `app.rs` (~line 243)**

Replace:

```rust
/// Convert a [`Report<TrustedServerError>`] into an HTTP [`Response`],
/// mirroring [`crate::http_error_response`] exactly.
///
/// The near-identical function in `main.rs` is intentional: the legacy path
/// uses fastly HTTP types while this path uses `edgezero_core` types.
```

With:

```rust
/// Converts a [`Report<TrustedServerError>`] into an HTTP [`Response`].
```

- [ ] **Step 5: Verify doc check**

```bash
cargo doc --no-deps -p trusted-server-adapter-fastly 2>&1 | grep "warning\|error" | head -20
```

Expected: no broken intra-doc link warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/middleware.rs crates/trusted-server-adapter-fastly/src/app.rs
git commit -m "Remove stale legacy references from doc comments

middleware.rs referenced deleted finalize_response() and route_request().
app.rs referenced deleted http_error_response(). Part of #501."
```

---

### Task 9: Remove canary flag keys from `fastly.toml`

**Files:**

- Modify: `fastly.toml`

- [ ] **Step 1: Remove the two flag keys and their comments**

In `[local_server.config_stores.trusted_server_config.contents]` (around lines 46-56), remove:

```toml
            # "true" / "1" (case-insensitive) enable the EdgeZero path. Missing,
            # unreadable, or any other value falls back to the legacy entry point.
            # Keep "false" until EdgeZero reaches full functional parity with legacy.
            edgezero_enabled = "false"
            # Integer 0-100. Effective only when edgezero_enabled = "true".
            #   0    -> all traffic to legacy (instant rollback — no deploy needed)
            #   1-99 -> canary: clients whose fnv1a_bucket(client_ip) < this value go EdgeZero
            #   100  -> all traffic to EdgeZero (full cutover)
            # Key absent when edgezero_enabled = "true" is treated as 100 (full rollout).
            # IMPORTANT: Set this to "0" in production BEFORE setting edgezero_enabled = "true".
            edgezero_rollout_pct = "0"
```

The `[local_server.config_stores.trusted_server_config.contents]` block becomes empty (`{}`). The store declaration and the `[local_server.config_stores.trusted_server_config]` header stay — the store is still opened by `open_trusted_server_config_store()`.

- [ ] **Step 2: Verify local config loads without error**

```bash
cargo check -p trusted-server-adapter-fastly
```

Expected: PASS (fastly.toml is only read at runtime by Viceroy/Fastly, not by `cargo check`).

- [ ] **Step 3: Commit**

```bash
git add fastly.toml
git commit -m "Remove edgezero_enabled and edgezero_rollout_pct from fastly.toml

The app no longer reads these config store keys. Remove them from the
local dev config store. The trusted_server_config store itself stays —
it is still opened by open_trusted_server_config_store(). Part of #501."
```

---

### Task 10: Run full CI gates

**Files:** None (verification only)

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

If it fails: `cargo fmt --all && git add -u && git commit -m "Format after legacy cleanup"`

- [ ] **Step 2: Clippy — all adapter targets**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: zero warnings/errors on all targets.

- [ ] **Step 3: Test — all adapter targets**

```bash
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
```

Expected: PASS.

- [ ] **Step 4: Parity test**

```bash
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity
```

Expected: PASS.

- [ ] **Step 5: JS tests and format**

```bash
cd crates/js/lib && npx vitest run && npm run format
```

Expected: PASS.

- [ ] **Step 6: Docs format**

```bash
cd docs && npm run format -- --check
```

Expected: PASS.

- [ ] **Step 7: Confirm all clean before PR**

```bash
git status
```

Expected: clean working tree. All changes committed.

---

## Execution options

**Plan complete and saved to `docs/superpowers/plans/2026-05-27-pr20-legacy-cleanup.md`.**

**1. Subagent-Driven (recommended)** — Fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using `executing-plans`, batch with checkpoints

Which approach?
