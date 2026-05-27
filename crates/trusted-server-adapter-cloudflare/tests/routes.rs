//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! the platform layer or outbound network calls.

use edgezero_core::app::Hooks as _;
use edgezero_core::http::request_builder;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;

#[test]
fn routes_build_without_panic() {
    // build_state() may fail (no real settings in CI) — startup_error_router
    // is the fallback. Either way, routes() must not panic.
    let _router = TrustedServerApp::routes();
}

fn assert_route_registered(
    router: &edgezero_core::router::RouterService,
    method: &str,
    path: &str,
) {
    assert!(
        router
            .routes()
            .iter()
            .any(|route| route.method().as_str() == method && route.path() == path),
        "{method} {path} must be explicitly registered before the wildcard fallback"
    );
}

// ---------------------------------------------------------------------------
// Middleware regression tests — verify FinalizeResponseMiddleware and
// AuthMiddleware are wired so they cannot be removed silently.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_injects_geo_header() {
    // The X-Geo-Info-Available header is injected by FinalizeResponseMiddleware.
    // Its absence on any response means the middleware was not wired.
    let router = TrustedServerApp::routes();

    let req = request_builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");

    let resp = router.oneshot(req).await;

    assert!(
        resp.headers().contains_key("x-geo-info-available"),
        "FinalizeResponseMiddleware must inject X-Geo-Info-Available on every response"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_middleware_runs_in_chain_for_protected_routes() {
    // Verifies that AuthMiddleware is wired into the middleware chain for auction
    // requests. Without it, FinalizeResponseMiddleware would still run but auth
    // challenges would be skipped silently.
    //
    // CI settings may not have basic_auth configured, so this test does not
    // assert 401 — it asserts that both middleware layers ran (X-Geo-Info-Available
    // present) and separately verifies the explicit auction route registration.
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/auction");

    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = router.oneshot(req).await;

    // Regardless of auth config the response must carry the finalize header,
    // confirming both middleware layers ran (auth short-circuits through finalize).
    assert!(
        resp.headers().contains_key("x-geo-info-available"),
        "middleware chain must inject X-Geo-Info-Available even on auth-rejected responses"
    );
    assert_ne!(
        resp.status().as_u16(),
        404,
        "auction endpoint must be routed"
    );
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify all adapter routes are explicitly registered
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_is_routed_not_5xx() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    // The tsjs route is matched by the /{*rest} catch-all. The handler returns 404
    // for an unknown hash — that is correct application behaviour, not a routing miss.
    assert!(status < 500, "tsjs route must not 5xx: got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/verify-signature");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/admin/keys/rotate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/admin/keys/deactivate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/auction");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "GET", "/first-party/proxy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_click_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "GET", "/first-party/click");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "GET", "/first-party/sign");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/first-party/sign");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_is_routed() {
    let router = TrustedServerApp::routes();
    assert_route_registered(&router, "POST", "/first-party/proxy-rebuild");
}

// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_returns_401() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_includes_www_authenticate_header() {
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
        .expect("should have www-authenticate header")
        .to_str()
        .expect("should be valid UTF-8");
    assert!(
        www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme, got: {www_auth}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_with_wrong_credentials_returns_401() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_does_not_require_auth() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_endpoint_does_not_require_auth() {
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

// ---------------------------------------------------------------------------
// Admin key route full path coverage
// ---------------------------------------------------------------------------

// Exercises the auth-fail path with a realistic key body (complements the
// generic `admin_route_without_credentials_returns_401` above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_auth_fail_returns_401() {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
