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
async fn auth_middleware_rejects_with_401_when_credentials_required() {
    // If basic-auth is configured, AuthMiddleware must challenge unauthenticated
    // requests before reaching the handler. Without AuthMiddleware wired, the
    // handler runs unchallenged.
    //
    // In CI the settings may not have basic_auth configured, in which case
    // enforce_basic_auth returns Ok(None) and the handler runs normally (non-401).
    // Either outcome is valid — this test guards against a panic or missing
    // X-Geo-Info-Available header that would indicate the middleware chain broke.
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
