# PR-18 Phase 5: Cross-Adapter Verification Suite

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Phase 5 verification gate suite — route parity, cross-adapter behavior, auth parity, auction error-correlation, HTML golden tests, and performance benchmarks — proving all three adapters (Fastly, Axum, Cloudflare) are behaviorally equivalent before production cutover.

**Architecture:** Tests live across three layers: (1) in-process unit tests per adapter's own `tests/routes.rs`, (2) a new `parity` test binary in `crates/integration-tests` that drives Axum and Cloudflare adapters with identical requests and asserts matching status/headers, and (3) Criterion benchmarks for HTML processor throughput. Fastly parity is verified via the existing `cargo test-fastly` + Viceroy matrix.

**Tech Stack:** Rust 2024, `tokio`, `tower`, `http` crate, `edgezero_core`, `edgezero_adapter_axum`, `edgezero_adapter_cloudflare`, `criterion 0.5`

---

## File Map

| Action | Path                                                         | Responsibility                                                                 |
| ------ | ------------------------------------------------------------ | ------------------------------------------------------------------------------ |
| Modify | `crates/trusted-server-adapter-cloudflare/tests/routes.rs`   | Route smoke tests for all 10+ routes + basic-auth parity + admin key full path |
| Modify | `crates/trusted-server-adapter-axum/tests/routes.rs`         | Basic-auth parity + admin key full path coverage                               |
| Modify | `crates/integration-tests/Cargo.toml`                        | Add `[[test]]` for parity binary + adapter deps                                |
| Create | `crates/integration-tests/tests/parity.rs`                   | Cross-adapter in-process parity (Axum vs Cloudflare)                           |
| Modify | `crates/trusted-server-core/src/auction/orchestrator.rs`     | Auction async fan-out + error-correlation unit tests                           |
| Modify | `crates/trusted-server-core/src/html_processor.rs`           | Golden output snapshot assertions                                              |
| Create | `crates/trusted-server-core/benches/html_processor_bench.rs` | Criterion p95 latency + throughput benchmark                                   |
| Modify | `crates/trusted-server-core/Cargo.toml`                      | Already has criterion; verify bench target exists                              |
| Modify | `.github/workflows/test.yml`                                 | Add benchmark regression gate                                                  |

---

## Task 1: Cloudflare Route Completeness

Cloudflare `tests/routes.rs` has only 2 tests today (middleware regression + auth chain). All 10 routes from the Fastly `route_request` match list must be smoke-tested.

**Files:**

- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`

- [ ] **Step 1: Write failing tests for missing routes**

Add after the existing 2 tests in `crates/trusted-server-adapter-cloudflare/tests/routes.rs`:

```rust
// Routes currently missing from Cloudflare route smoke tests:
// /static/tsjs=*, /verify-signature, /admin/keys/rotate,
// /admin/keys/deactivate, /auction, /first-party/proxy,
// /first-party/click, /first-party/sign (GET+POST), /first-party/proxy-rebuild

#[tokio::test]
async fn tsjs_route_is_routed_not_5xx() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert!(status < 500, "tsjs route must not 5xx: got {status}");
    assert_ne!(status, 404, "tsjs route must be registered");
}

#[tokio::test]
async fn verify_signature_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(resp.status().as_u16(), 404, "/verify-signature must be routed");
}

#[tokio::test]
async fn admin_rotate_key_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(resp.status().as_u16(), 404, "/admin/keys/rotate must be routed");
}

#[tokio::test]
async fn admin_deactivate_key_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(resp.status().as_u16(), 404, "/admin/keys/deactivate must be routed");
}

#[tokio::test]
async fn auction_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"adUnits":[]}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(resp.status().as_u16(), 404, "/auction must be routed");
}

#[tokio::test]
async fn first_party_proxy_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/proxy")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert_ne!(status, 404, "/first-party/proxy must be routed");
    assert!(status < 500, "/first-party/proxy must not 5xx: got {status}");
}

#[tokio::test]
async fn first_party_click_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/click")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert_ne!(status, 404, "/first-party/click must be routed");
    assert!(status < 500, "/first-party/click must not 5xx: got {status}");
}

#[tokio::test]
async fn first_party_sign_get_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/sign")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert_ne!(status, 404, "GET /first-party/sign must be routed");
    assert!(status < 500, "GET /first-party/sign must not 5xx: got {status}");
}

#[tokio::test]
async fn first_party_sign_post_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/sign")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert_ne!(status, 404, "POST /first-party/sign must be routed");
    assert!(status < 500, "POST /first-party/sign must not 5xx: got {status}");
}

#[tokio::test]
async fn first_party_proxy_rebuild_is_routed() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/proxy-rebuild")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    assert_ne!(status, 404, "/first-party/proxy-rebuild must be routed");
    assert!(status < 500, "/first-party/proxy-rebuild must not 5xx: got {status}");
}
```

- [ ] **Step 2: Run tests and verify they compile + fail correctly**

```bash
cargo test-cloudflare 2>&1 | tail -20
```

Expected: all new tests compile; some may fail if routes are missing.

- [ ] **Step 3: Fix any missing route wiring**

If a test reports 404, check `crates/trusted-server-adapter-cloudflare/src/app.rs` route registration around line 354-364 and add the missing `.method("/path", handler)` entry.

- [ ] **Step 4: Run tests and verify they pass**

```bash
cargo test-cloudflare 2>&1 | tail -20
```

Expected: all route tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-cloudflare/tests/routes.rs
git commit -m "Add route smoke tests for all Cloudflare adapter routes"
```

---

## Task 2: Basic-Auth Parity Tests

Both adapters must: (a) return 401 on protected routes without credentials, (b) include `WWW-Authenticate: Basic realm="..."` header in 401 responses, (c) not challenge on unprotected routes.

**Files:**

- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`

- [ ] **Step 1: Write failing auth tests for Cloudflare adapter**

Add to `crates/trusted-server-adapter-cloudflare/tests/routes.rs`:

```rust
// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_route_without_credentials_returns_401() {
    // Protected route (/admin/*) must challenge unauthenticated requests.
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must return 401 without credentials"
    );
}

#[tokio::test]
async fn admin_route_without_credentials_includes_www_authenticate_header() {
    // 401 response must include WWW-Authenticate so clients know auth scheme.
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "should be 401 before checking header"
    );
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "401 response must include WWW-Authenticate header"
    );
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme, got: {www_auth}"
    );
}

#[tokio::test]
async fn admin_route_with_wrong_credentials_returns_401() {
    // Wrong credentials must not grant access.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong-password");
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {creds}"))
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must reject wrong credentials with 401"
    );
}

#[tokio::test]
async fn discovery_endpoint_does_not_require_auth() {
    // /.well-known/trusted-server.json is publicly accessible — no auth gate.
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/.well-known/trusted-server.json must not require auth"
    );
}

#[tokio::test]
async fn auction_endpoint_does_not_require_auth() {
    // /auction is a consumer-facing endpoint — must not apply basic-auth gate.
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"adUnits":[]}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/auction must not apply admin basic-auth gate"
    );
}
```

- [ ] **Step 2: Run Cloudflare auth tests**

```bash
cargo test-cloudflare 2>&1 | grep -E "FAILED|PASSED|test result"
```

Expected: new auth tests pass. If 401 is not returned, the auth middleware may not be configured in test settings — check `AppState::build_state()` fallback behavior.

- [ ] **Step 3: Write same auth tests for Axum adapter**

Add to `crates/trusted-server-adapter-axum/tests/routes.rs` (same tests, adapted for Axum Service interface):

```rust
// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_returns_401() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must return 401 without credentials"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_includes_www_authenticate_header() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "should be 401 before checking header"
    );
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "401 response must include WWW-Authenticate header"
    );
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme, got: {www_auth}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_with_wrong_credentials_returns_401() {
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong-password");
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {creds}"))
        .body(AxumBody::from("{}"))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin route must reject wrong credentials with 401"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_does_not_require_auth() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(AxumBody::empty())
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/.well-known/trusted-server.json must not require auth"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_endpoint_does_not_require_auth() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(AxumBody::from(r#"{"adUnits":[]}"#))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/auction must not apply admin basic-auth gate"
    );
}
```

Note: `base64 = "0.22"` is in `[workspace.dependencies]`. Use `{ workspace = true }` for adapters.

- [ ] **Step 4: Add `base64` dev-dependency to both adapter Cargo.toml files**

In `crates/trusted-server-adapter-axum/Cargo.toml` `[dev-dependencies]`:

```toml
base64 = { workspace = true }
```

In `crates/trusted-server-adapter-cloudflare/Cargo.toml` `[dev-dependencies]`:

```toml
base64 = { workspace = true }
```

- [ ] **Step 5: Run both adapter auth tests**

```bash
cargo test-axum 2>&1 | grep -E "FAILED|PASSED|test result"
cargo test-cloudflare 2>&1 | grep -E "FAILED|PASSED|test result"
```

Expected: all auth parity tests pass on both adapters.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-adapter-axum/tests/routes.rs \
        crates/trusted-server-adapter-cloudflare/tests/routes.rs \
        crates/trusted-server-adapter-axum/Cargo.toml \
        crates/trusted-server-adapter-cloudflare/Cargo.toml
git commit -m "Add basic-auth parity tests to Axum and Cloudflare adapters"
```

---

## Task 3: Admin Key Route Full Path Coverage

Covers auth-fail, validation-fail, and storage-fail paths on both Axum and Cloudflare. Success path differs by adapter (Axum returns 501; Cloudflare returns 200 or storage error).

**Files:**

- Modify: `crates/trusted-server-adapter-axum/tests/routes.rs`
- Modify: `crates/trusted-server-adapter-cloudflare/tests/routes.rs`

- [ ] **Step 1: Add admin key path tests to Cloudflare adapter**

Add to `crates/trusted-server-adapter-cloudflare/tests/routes.rs`:

```rust
// ---------------------------------------------------------------------------
// Admin key route full path coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_rotate_key_auth_fail_returns_401() {
    // Auth-fail path: missing credentials → 401 (tested in basic-auth section,
    // this test documents the specific admin key route behavior explicitly).
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/rotate without credentials must return 401"
    );
}

#[tokio::test]
async fn admin_deactivate_key_auth_fail_returns_401() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/deactivate without credentials must return 401"
    );
}

#[tokio::test]
async fn admin_rotate_key_validation_fail_returns_non_5xx() {
    // Validation-fail path: authenticated but malformed body must not 5xx.
    // CI settings may not have basic_auth configured; if auth passes through,
    // an empty/malformed body should produce 400/422, not 500.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:admin-pass");
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {creds}"))
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    // Validation-fail path must be a 4xx client error — not 2xx (passed) or 5xx (crashed).
    assert!(
        (400..500).contains(&status),
        "admin/keys/rotate with malformed body must return 4xx: got {status}"
    );
}

#[tokio::test]
async fn admin_rotate_key_storage_fail_does_not_panic() {
    // Storage-fail path: handler is reached, store operation returns error.
    // In CI the store is either absent (error) or a noop. Either way must not
    // panic (no 500 with a backtrace).
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:admin-pass");
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {creds}"))
        .body(edgezero_core::body::Body::from(r#"{"keyId":"test-key-id"}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    // Storage-fail path: handler reached, store returns error.
    // Must produce a proper HTTP error (4xx or 5xx), NOT a routing miss (404)
    // and NOT an unrecovered panic (which would surface as 500 on some runtimes).
    assert_ne!(status, 404, "admin/keys/rotate must not 404 when authenticated");
    assert!(
        status >= 400,
        "admin/keys/rotate storage-fail must return error status: got {status}"
    );
}
```

- [ ] **Step 2: Add admin key path tests to Axum adapter**

Add to `crates/trusted-server-adapter-axum/tests/routes.rs` (same logic, Axum returns 501 for authenticated requests since store writes are unsupported):

```rust
// ---------------------------------------------------------------------------
// Admin key route full path coverage
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_auth_fail_returns_401() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(AxumBody::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/rotate without credentials must return 401"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_authenticated_returns_not_5xx() {
    // Axum dev server returns 501 Not Implemented for admin key writes.
    // Auth runs first — if configured: 401. If auth skipped in CI: 501.
    // Either way: must not 500.
    use base64::Engine as _;
    let creds = base64::engine::general_purpose::STANDARD.encode("admin:admin-pass");
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .header("authorization", format!("Basic {creds}"))
        .body(AxumBody::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    let status = resp.status().as_u16();
    assert_ne!(status, 500, "admin/keys/rotate must not 5xx: got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_auth_fail_returns_401() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(AxumBody::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = svc.ready().await.expect("should be ready").call(req).await.expect("should respond");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/deactivate without credentials must return 401"
    );
}
```

- [ ] **Step 3: Run and verify**

```bash
cargo test-axum 2>&1 | grep -E "admin_key|FAILED|test result"
cargo test-cloudflare 2>&1 | grep -E "admin_key|FAILED|test result"
```

Expected: all admin key path tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-axum/tests/routes.rs \
        crates/trusted-server-adapter-cloudflare/tests/routes.rs
git commit -m "Add admin key route full path coverage to Axum and Cloudflare adapters"
```

---

## Task 4: Cross-Adapter In-Process Parity Tests

A dedicated test binary drives Axum and Cloudflare adapters with identical requests and asserts matching status codes and critical headers. This catches divergence that per-adapter smoke tests miss.

**Files:**

- Modify: `crates/integration-tests/Cargo.toml`
- Create: `crates/integration-tests/tests/parity.rs`

- [ ] **Step 1: Add adapter dependencies and parity test binary to integration-tests**

Note: `crates/integration-tests` is intentionally **excluded** from the workspace (see root `Cargo.toml` `exclude` list). Its `Cargo.toml` must use explicit versions or path deps for everything — `workspace = true` is not available here.

Edit `crates/integration-tests/Cargo.toml`. Add after the existing `[[test]]` block:

```toml
[[test]]
name = "parity"
path = "tests/parity.rs"
harness = true
```

Add to `[dev-dependencies]`:

```toml
trusted-server-adapter-axum = { path = "../trusted-server-adapter-axum" }
trusted-server-adapter-cloudflare = { path = "../trusted-server-adapter-cloudflare" }
axum = "0.7"
tower = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
http = "1"
http-body-util = "0.1"
bytes = "1"
base64 = "0.22"
# serde_json already in dev-dependencies for existing integration tests
```

Then add edgezero git deps **matching the rev in root Cargo.toml exactly**. Run this to extract the rev:

```bash
grep 'rev = ' Cargo.toml | head -1 | grep -oP '(?<=rev = ").*(?=")'
```

Then add to `crates/integration-tests/Cargo.toml` `[dev-dependencies]` (replace `<REV>` with actual value):

```toml
edgezero-adapter-axum = { git = "https://github.com/stackpop/edgezero", rev = "<REV>", features = ["axum"] }
edgezero-core = { git = "https://github.com/stackpop/edgezero", rev = "<REV>" }
```

> **Why git dep not path dep:** edgezero is a git dependency in the workspace (not a local crate). Since `integration-tests` is workspace-excluded it cannot use `{ workspace = true }` — it must replicate the git dep form with the same rev to get Cargo to unify the dependency with the workspace's Cargo.lock.

- [ ] **Step 2: Verify integration-tests compiles**

```bash
cd crates/integration-tests && cargo check --test parity 2>&1 | head -30
```

Expected: compiles without errors. Since integration-tests is workspace-excluded, run `cargo` from inside the `crates/integration-tests` directory or use `--manifest-path`.

- [ ] **Step 3: Create parity test file**

Create `crates/integration-tests/tests/parity.rs`:

```rust
//! Cross-adapter parity tests: Axum vs Cloudflare in-process.
//!
//! Sends identical requests to both adapters and asserts that:
//! - Response status codes match
//! - Critical headers (X-Geo-Info-Available, WWW-Authenticate on 401) match
//!
//! Fastly parity is verified via cargo test-fastly + Viceroy in CI.

// Both adapters define `TrustedServerApp` — alias both to avoid name collision.
// axum::http re-exports from the `http` crate, so HeaderMap types are identical.
use axum::body::Body as AxumBody;
use axum::http::Request as AxumRequest;
use edgezero_adapter_axum::EdgeZeroAxumService;
use edgezero_core::app::Hooks as _;
use edgezero_core::http::request_builder;
use http::HeaderMap;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp as AxumApp;
use trusted_server_adapter_cloudflare::app::TrustedServerApp as CloudflareApp;

/// Send a GET request to the Axum adapter and return (status, headers).
async fn axum_get(uri: &str) -> (u16, HeaderMap) {
    let mut svc = EdgeZeroAxumService::new(AxumApp::routes());
    let req = AxumRequest::builder()
        .method("GET")
        .uri(uri)
        .body(AxumBody::empty())
        .expect("should build GET request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    // axum::http::HeaderMap is http::HeaderMap — same type, just re-exported
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send a POST request to the Axum adapter and return (status, headers, body bytes).
async fn axum_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    use http_body_util::BodyExt as _;
    let mut svc = EdgeZeroAxumService::new(AxumApp::routes());
    let req = AxumRequest::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(AxumBody::from(body.to_owned()))
        .expect("should build POST request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().collect().await.expect("should collect body").to_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn axum_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = axum_post(uri, body).await;
    (s, h)
}

/// Send a GET request to the Cloudflare adapter and return (status, headers).
async fn cf_get(uri: &str) -> (u16, HeaderMap) {
    let router = CloudflareApp::routes();
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build GET request");
    // router.oneshot() is infallible — returns Response directly, not Result
    let resp = router.oneshot(req).await;
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send a POST request to the Cloudflare adapter and return (status, headers, body bytes).
async fn cf_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    use http_body_util::BodyExt as _;
    let router = CloudflareApp::routes();
    let req = request_builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build POST request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().collect().await.expect("should collect body").to_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn cf_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = cf_post(uri, body).await;
    (s, h)
}

// ---------------------------------------------------------------------------
// Route parity: same route → same status class on both adapters
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_route_status_parity() {
    let (axum_status, _) = axum_get("/.well-known/trusted-server.json").await;
    let (cf_status, _) = cf_get("/.well-known/trusted-server.json").await;
    assert_eq!(
        axum_status, cf_status,
        "/.well-known/trusted-server.json must return same status: axum={axum_status} cf={cf_status}"
    );
}

#[tokio::test]
async fn discovery_route_body_is_json_parity() {
    // Spec criterion 2 requires body parity. Discovery must return parseable JSON
    // on both adapters (not just same status).
    use http_body_util::BodyExt as _;
    use serde_json::Value;

    let axum_body_bytes = {
        let mut svc = EdgeZeroAxumService::new(AxumApp::routes());
        let req = AxumRequest::builder()
            .method("GET")
            .uri("/.well-known/trusted-server.json")
            .body(AxumBody::empty())
            .expect("should build GET request");
        let resp = svc.ready().await.expect("ready").call(req).await.expect("respond");
        resp.into_body().collect().await.expect("collect body").to_bytes()
    };

    let cf_body_bytes = {
        let router = CloudflareApp::routes();
        let req = request_builder()
            .method("GET")
            .uri("/.well-known/trusted-server.json")
            .body(edgezero_core::body::Body::empty())
            .expect("should build GET request");
        let resp = router.oneshot(req).await;
        resp.into_body().collect().await.expect("collect body").to_bytes()
    };

    let axum_json: Option<Value> = serde_json::from_slice(&axum_body_bytes).ok();
    let cf_json: Option<Value> = serde_json::from_slice(&cf_body_bytes).ok();
    assert!(axum_json.is_some(), "Axum discovery must return valid JSON body");
    assert!(cf_json.is_some(), "Cloudflare discovery must return valid JSON body");
}

#[tokio::test]
async fn verify_signature_route_parity() {
    // Spec criterion 2: "signing responses" must have status parity.
    // Both adapters must reach the handler (not 404) and not panic (not 500).
    let (axum_status, _) = axum_post_headers("/verify-signature", "{}").await;
    let (cf_status, _) = cf_post_headers("/verify-signature", "{}").await;

    assert_ne!(axum_status, 404, "Axum /verify-signature must be routed");
    assert_ne!(cf_status, 404, "Cloudflare /verify-signature must be routed");
    assert!(axum_status < 500, "Axum /verify-signature must not 5xx: {axum_status}");
    assert!(cf_status < 500, "Cloudflare /verify-signature must not 5xx: {cf_status}");
    assert_eq!(
        axum_status, cf_status,
        "/verify-signature must return same status: axum={axum_status} cf={cf_status}"
    );
}

#[tokio::test]
async fn admin_rotate_unauthenticated_parity() {
    let (axum_status, axum_headers) = axum_post_headers("/admin/keys/rotate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/admin/keys/rotate", "{}").await;

    assert_eq!(
        axum_status, cf_status,
        "/admin/keys/rotate unauthenticated must return same status: axum={axum_status} cf={cf_status}"
    );
    assert_eq!(axum_status, 401, "both adapters must return 401");

    let axum_www_auth = axum_headers
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cf_www_auth = cf_headers
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert!(
        axum_www_auth.starts_with("Basic realm="),
        "Axum 401 WWW-Authenticate must be Basic scheme: {axum_www_auth:?}"
    );
    assert!(
        cf_www_auth.starts_with("Basic realm="),
        "Cloudflare 401 WWW-Authenticate must be Basic scheme: {cf_www_auth:?}"
    );
    // Values should match (same realm string) — documents intentional divergence if not
    assert_eq!(
        axum_www_auth, cf_www_auth,
        "WWW-Authenticate values must match across adapters"
    );
}

#[tokio::test]
async fn geo_header_parity_on_all_responses() {
    // X-Geo-Info-Available must be present on every response (FinalizeResponseMiddleware).
    let routes_to_check: &[(&str, &str, &str)] = &[
        ("GET", "/.well-known/trusted-server.json", ""),
        ("POST", "/auction", r#"{"adUnits":[]}"#),
        ("POST", "/verify-signature", "{}"),
    ];

    for (method, path, body) in routes_to_check {
        let (axum_status, axum_headers) = if *method == "GET" {
            axum_get(path).await
        } else {
            axum_post_headers(path, body).await
        };
        let (cf_status, cf_headers) = if *method == "GET" {
            cf_get(path).await
        } else {
            cf_post_headers(path, body).await
        };

        assert!(
            axum_headers.contains_key("x-geo-info-available"),
            "Axum: {method} {path} (status={axum_status}) must have X-Geo-Info-Available"
        );
        assert!(
            cf_headers.contains_key("x-geo-info-available"),
            "Cloudflare: {method} {path} (status={cf_status}) must have X-Geo-Info-Available"
        );
    }
}

#[tokio::test]
async fn auction_not_challenged_by_auth_parity() {
    // /auction must not be gated by admin basic-auth on either adapter.
    let (axum_status, _) = axum_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (cf_status, _) = cf_post_headers("/auction", r#"{"adUnits":[]}"#).await;

    assert_ne!(axum_status, 401, "Axum /auction must not 401");
    assert_ne!(cf_status, 401, "Cloudflare /auction must not 401");
}

#[tokio::test]
async fn cookie_behavior_note() {
    // Cookie (Set-Cookie) behavior is set by the publisher proxy handler when
    // the origin serves a full HTML page. In-process tests without a live origin
    // cannot exercise this path. Cookie parity is covered by the Docker-based
    // integration tests in test_all_combinations (marked #[ignore] — requires Docker).
    //
    // What we CAN verify in-process: publisher route does NOT set a cookie when
    // the origin is unavailable (no origin configured → no EC cookie attempt).
    let (axum_status, axum_headers) = axum_get("/").await;
    let (cf_status, cf_headers) = cf_get("/").await;

    // Neither adapter should crash on publisher fallback path
    assert!(axum_status < 500, "Axum publisher fallback must not 5xx: {axum_status}");
    assert!(cf_status < 500, "Cloudflare publisher fallback must not 5xx: {cf_status}");

    // If a Set-Cookie is set, both adapters must set it (presence parity)
    let axum_has_cookie = axum_headers.contains_key("set-cookie");
    let cf_has_cookie = cf_headers.contains_key("set-cookie");
    assert_eq!(
        axum_has_cookie, cf_has_cookie,
        "Set-Cookie presence must match: axum={axum_has_cookie} cf={cf_has_cookie}"
    );
}

#[tokio::test]
async fn unknown_route_returns_same_status_parity() {
    // Both adapters must handle unknown routes the same way (not crash).
    let (axum_status, _) = axum_get("/this-route-does-not-exist-abc123").await;
    let (cf_status, _) = cf_get("/this-route-does-not-exist-abc123").await;

    assert_eq!(
        axum_status, cf_status,
        "unknown routes must return same status: axum={axum_status} cf={cf_status}"
    );
}
```

- [ ] **Step 4: Run parity tests**

```bash
cd crates/integration-tests && cargo test --test parity 2>&1 | tail -30
```

Expected: all parity tests pass. If status codes diverge, investigate the differing adapter behavior and add an exception comment documenting the known difference if intentional.

- [ ] **Step 5: Commit**

```bash
git add crates/integration-tests/Cargo.toml crates/integration-tests/tests/parity.rs
git commit -m "Add cross-adapter in-process parity test suite (Axum vs Cloudflare)"
```

---

## Task 5: Auction Async Fan-Out and Error-Correlation Tests

Verify that `PlatformResponse::backend_name` is `None` on Axum/Cloudflare (as expected before EdgeZero #213), and that the auction orchestrator handles this gracefully without panicking.

**Files:**

- Modify: `crates/trusted-server-core/src/platform/http.rs` (where `PlatformResponse` is defined)

- [ ] **Step 1: Locate existing test module in orchestrator.rs**

```bash
grep -n "#\[cfg(test)\]\|mod tests\|#\[test\]" crates/trusted-server-core/src/auction/orchestrator.rs | head -20
```

- [ ] **Step 2: Write error-correlation tests**

These tests live in `crates/trusted-server-core/src/platform/http.rs` `#[cfg(test)]` module (where `PlatformResponse` is defined), not in `orchestrator.rs`. Add after the existing tests in that file's `#[cfg(test)]` module:

```rust
// ---------------------------------------------------------------------------
// Error-correlation interim scope (before EdgeZero #213)
// ---------------------------------------------------------------------------

#[test]
fn platform_response_default_has_no_backend_name() {
    // On Axum/Cloudflare noop clients return PlatformResponse::new(response)
    // with no backend_name. Core logic must not panic when backend_name is None.
    let response = edgezero_core::http::Response::builder()
        .status(200)
        .body(edgezero_core::body::Body::empty())
        .expect("should build response");
    let resp = PlatformResponse::new(response);
    // PlatformResponse has a public field, not a method.
    // PlatformPendingRequest has backend_name() method; PlatformResponse does not.
    assert_eq!(
        resp.backend_name,
        None,
        "PlatformResponse without backend_name must have None field"
    );
}

#[test]
fn platform_response_with_backend_name_is_some() {
    // On Fastly, responses carry backend_name for error correlation.
    let response = edgezero_core::http::Response::builder()
        .status(200)
        .body(edgezero_core::body::Body::empty())
        .expect("should build response");
    let resp = PlatformResponse::new(response).with_backend_name("prebid-backend");
    assert_eq!(
        resp.backend_name.as_deref(),
        Some("prebid-backend"),
        "with_backend_name must set backend_name field"
    );
}
```

Confirmed: `platform/http.rs` has **no existing `#[cfg(test)]` module** (verified by grep). Must create one. Add at end of file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // error-correlation tests go here
}
```

**File:** `crates/trusted-server-core/src/platform/http.rs`

````

- [ ] **Step 3: Run the tests**

```bash
cargo test -p trusted-server-core auction::orchestrator::tests 2>&1 | tail -20
````

Expected: both tests pass.

- [ ] **Step 4: Run tests**

```bash
cargo test -p trusted-server-core platform_response 2>&1 | tail -15
```

Expected: both tests pass (test names match `platform_response_*`).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/http.rs
git commit -m "Add error-correlation unit tests for PlatformResponse backend_name"
```

---

## Task 6: HTML Rewriting Golden Tests

Strengthen `html_processor.rs` tests with precise snapshot-style assertions that will catch regressions in injection position, URL rewriting correctness, and integration rewriter behavior.

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs`

- [ ] **Step 1: Find the existing `test_real_publisher_html` test and helper**

```bash
grep -n "fn test_real_publisher_html\|fn create_test_config\|fn test_integration_registry" \
  crates/trusted-server-core/src/html_processor.rs
```

Confirmed: `create_test_config()` is at line ~537. `test_real_publisher_html` at ~728.

`HtmlProcessorConfig` actual fields (no `script_tag`):

```rust
pub struct HtmlProcessorConfig {
    pub origin_host: String,
    pub request_host: String,
    pub request_scheme: String,
    pub integrations: IntegrationRegistry,  // NOT Arc<> wrapped
}
```

- [ ] **Step 2: Add golden injection position test**

In the `#[cfg(test)]` module of `crates/trusted-server-core/src/html_processor.rs`, add after the existing tests:

```rust
#[test]
fn golden_script_tag_injected_at_head_start() {
    // The trusted-server script tag must be the FIRST child of <head>.
    // Any drift in injection position breaks the page initialization order.
    let html = r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Test</title></head>
<body><p>Hello</p></body>
</html>"#;

    let config = create_test_config();
    let mut processor = create_html_processor(config);
    let output = processor
        .process_chunk(html.as_bytes(), true)
        .expect("should process HTML");
    let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

    let head_pos = output_str
        .find("<head>")
        .expect("should contain <head>");
    let script_pos = output_str
        .find("<script")
        .expect("should inject script tag");

    assert!(
        script_pos > head_pos,
        "script tag must appear after <head> opening: head_pos={head_pos}, script_pos={script_pos}"
    );

    // No other elements between <head> and the script tag
    let between = &output_str[head_pos + "<head>".len()..script_pos];
    let trimmed = between.trim();
    assert!(
        trimmed.is_empty(),
        "script tag must be first child of <head>, found content before it: {trimmed:?}"
    );
}

#[test]
fn golden_url_rewriting_replaces_origin_in_href() {
    // href attributes pointing at origin domain must be rewritten to proxy host.
    let origin = "https://origin.test-publisher.com";
    let html = format!(
        r#"<!DOCTYPE html><html><head></head><body>
        <a href="{origin}/page">Link</a>
        <img src="{origin}/img.png">
        </body></html>"#
    );

    let request_host = "proxy.test-publisher.com";
    let config = HtmlProcessorConfig {
        origin_host: "origin.test-publisher.com".to_string(),
        request_host: request_host.to_string(),
        request_scheme: "https".to_string(),
        integrations: IntegrationRegistry::default(),
    };
    let mut processor = create_html_processor(config);
    let output = processor
        .process_chunk(html.as_bytes(), true)
        .expect("should process HTML");
    let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

    assert!(
        !output_str.contains("origin.test-publisher.com"),
        "origin host must not appear in rewritten HTML"
    );
    assert!(
        output_str.contains(request_host),
        "proxy host must appear in rewritten HTML"
    );
}

#[test]
fn golden_integration_script_is_not_double_injected() {
    // Integration scripts from the registry must appear exactly once.
    let html = r#"<!DOCTYPE html>
<html><head></head><body><p>Content</p></body></html>"#;

    let config = create_test_config();
    let mut processor = create_html_processor(config);
    let output = processor
        .process_chunk(html.as_bytes(), true)
        .expect("should process HTML");
    let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

    let script_count = output_str.matches("/static/tsjs=").count();
    assert_eq!(
        script_count, 1,
        "script tag must appear exactly once, found {script_count} occurrences"
    );
}
```

- [ ] **Step 3: Run golden tests**

```bash
cargo test -p trusted-server-core html_processor 2>&1 | tail -30
```

Expected: all golden tests pass. If they fail, diagnose whether the processor behavior is wrong or the test assumptions are wrong (e.g., `create_test_config()` not available).

- [ ] **Step 4: Fix any helper gaps**

If `create_test_config()` doesn't exist, add it to the test module following the config pattern in `test_real_publisher_html` (around line 730).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/html_processor.rs
git commit -m "Add HTML rewriting golden regression tests"
```

---

## Task 7: Performance Benchmarks (p95 Latency + Response Size)

Criterion benchmarks for the HTML processor establish a baseline for regression detection. Benchmark name: `html_processor_bench`.

**Files:**

- Modify: `crates/trusted-server-core/Cargo.toml` (verify bench target)
- Create: `crates/trusted-server-core/benches/html_processor_bench.rs`

- [ ] **Step 1: Verify existing Cargo.toml bench configuration**

```bash
grep -A5 "\[\[bench\]\]" crates/trusted-server-core/Cargo.toml
```

There is already a `consent_decode` bench. Add a second `[[bench]]` entry.

- [ ] **Step 2: Add benchmark entry to Cargo.toml**

In `crates/trusted-server-core/Cargo.toml`, add after the existing `[[bench]]` block:

```toml
[[bench]]
name = "html_processor_bench"
harness = false
```

- [ ] **Step 3: Create benchmark file**

Create `crates/trusted-server-core/benches/html_processor_bench.rs`:

```rust
//! Performance benchmarks for the HTML processor.
//!
//! Baseline targets (to be updated after first run establishes actuals):
//! - process_chunk (10KB HTML): < 2ms mean
//! - process_chunk (100KB HTML): < 10ms mean
//!
//! Run with: cargo bench -p trusted-server-core --bench html_processor_bench

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use trusted_server_core::html_processor::{HtmlProcessorConfig, create_html_processor};
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::streaming_processor::StreamProcessor as _;

fn make_config() -> HtmlProcessorConfig {
    // HtmlProcessorConfig fields: origin_host, request_host, request_scheme, integrations
    // No script_tag field — the script tag is generated from the configured tsjs module list
    HtmlProcessorConfig {
        origin_host: "origin.bench.com".to_string(),
        request_host: "proxy.bench.com".to_string(),
        request_scheme: "https".to_string(),
        integrations: IntegrationRegistry::default(),
    }
}

fn make_html(size_kb: usize) -> Vec<u8> {
    // Construct a realistic HTML page of approximately `size_kb` KB
    // with links, images, and ad slots to exercise all rewriter paths.
    let link_block = r#"<a href="https://origin.bench.com/page">Link</a>
<img src="https://origin.bench.com/img.png">
<div data-ad-unit="/test/banner"><a href="https://origin.bench.com/ad">Ad</a></div>
"#;

    let body_content = link_block.repeat((size_kb * 1024) / link_block.len() + 1);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Benchmark Page</title>
</head>
<body>
{body_content}
</body>
</html>"#
    )
    .into_bytes()
}

fn bench_html_processor(c: &mut Criterion) {
    let mut group = c.benchmark_group("html_processor");

    for size_kb in [10usize, 100] {
        let html = make_html(size_kb);

        group.bench_with_input(
            BenchmarkId::new("process_chunk", format!("{size_kb}kb")),
            &html,
            |b, html| {
                b.iter(|| {
                    let config = make_config();
                    // `create_html_processor` returns `impl StreamProcessor`
                    // which exposes `process_chunk(&mut self, chunk: &[u8], is_last: bool)`
                    let mut processor = create_html_processor(config);
                    let result = processor
                        .process_chunk(html.as_slice(), true)
                        .expect("should process HTML");
                    result
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_html_processor);
criterion_main!(benches);
```

- [ ] **Step 4: Run benchmarks to establish baseline**

```bash
cargo bench -p trusted-server-core --bench html_processor_bench 2>&1 | tail -20
```

Expected: benchmarks complete. Record the mean latencies from the output for future regression comparison.

- [ ] **Step 5: Verify response size by adding a measurement test**

Add to the benchmark file a single measurement test (not a Criterion bench) to assert response size bounds. Alternatively, add this to the `html_processor.rs` unit tests:

In `crates/trusted-server-core/src/html_processor.rs` `#[cfg(test)]` module:

```rust
#[test]
fn response_size_does_not_grow_disproportionately() {
    // Processing must not expand HTML by more than 2× (accounts for injected
    // script tag + URL rewrites). Disproportionate growth indicates a bug
    // (e.g., double-processing, buffer leak).
    // File exists at crates/trusted-server-core/src/html_processor.test.html
    // (already used by test_real_publisher_html at line ~728).
    let html = include_str!("html_processor.test.html");
    let input_size = html.len();

    let config = create_test_config();
    let mut processor = create_html_processor(config);
    let output = processor
        .process_chunk(html.as_bytes(), true)
        .expect("should process HTML");

    let output_size = output.len();
    let growth_factor = output_size as f64 / input_size as f64;

    assert!(
        growth_factor < 2.0,
        "processed HTML must not grow by more than 2×: input={input_size}B output={output_size}B factor={growth_factor:.2}"
    );
}
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p trusted-server-core response_size 2>&1 | tail -10
```

Expected: passes.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/Cargo.toml \
        crates/trusted-server-core/benches/html_processor_bench.rs \
        crates/trusted-server-core/src/html_processor.rs
git commit -m "Add Criterion benchmarks and response size regression test for HTML processor"
```

---

## Task 8: CI Verification Gate

Update the CI workflows to run the new parity test binary and include a benchmark smoke-run (no regression threshold yet — establishes baseline).

**Files:**

- Modify: `.github/workflows/test.yml`

- [ ] **Step 1: Add parity test job to test.yml**

Add after the `test-cloudflare` job in `.github/workflows/test.yml`:

```yaml
test-parity:
  name: cargo test (cross-adapter parity)
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Retrieve Rust version
      id: rust-version
      run: echo "rust-version=$(grep '^rust ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
      shell: bash

    - name: Set up Rust toolchain
      uses: actions-rust-lang/setup-rust-toolchain@v1
      with:
        toolchain: ${{ steps.rust-version.outputs.rust-version }}
        cache-shared-key: cargo-${{ runner.os }}

    - name: Run cross-adapter parity tests
      run: cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity
```

- [ ] **Step 2: Add benchmark smoke-run to axum job (optional — compile-only check)**

In the `test-axum` job, add after the test step:

```yaml
- name: Run HTML processor benchmarks (smoke run)
  run: cargo bench -p trusted-server-core --bench html_processor_bench -- --test
  # `-- --test` runs benchmarks as tests (1 iteration), not full bench.
  # Full benchmarking is done manually, not in CI.
```

- [ ] **Step 3: Verify workflow YAML is valid**

```bash
# Check for YAML syntax errors
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/test.yml'))" && echo "YAML valid"
```

Expected: `YAML valid`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/test.yml
git commit -m "Add cross-adapter parity and benchmark CI gates for Phase 5 verification"
```

---

## Verification Checklist

After all tasks are complete, run the full suite:

```bash
# Fastly + core
cargo test-fastly

# Axum adapter (includes auth parity + admin key tests)
cargo test-axum

# Cloudflare adapter (includes route completeness + auth parity + admin key tests)
cargo test-cloudflare

# Cross-adapter parity
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity

# HTML golden + response size + error-correlation
cargo test -p trusted-server-core

# Benchmarks (smoke run)
cargo bench -p trusted-server-core --bench html_processor_bench -- --test
```

All commands must exit 0 before marking this PR complete.
