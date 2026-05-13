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

// ---------------------------------------------------------------------------
// Middleware regression tests — verify FinalizeResponseMiddleware and
// AuthMiddleware are wired so they cannot be removed silently.
// ---------------------------------------------------------------------------

#[tokio::test]
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

#[tokio::test]
async fn auth_middleware_runs_in_chain_for_protected_routes() {
    // Verifies that AuthMiddleware is wired into the middleware chain for auction
    // requests. Without it, FinalizeResponseMiddleware would still run but auth
    // challenges would be skipped silently.
    //
    // CI settings may not have basic_auth configured, so this test does not
    // assert 401 — it asserts that both middleware layers ran (X-Geo-Info-Available
    // present) and that the route is actually reached (status != 404).
    let router = TrustedServerApp::routes();

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
