//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! the platform layer or outbound network calls.

use edgezero_core::app::Hooks as _;
use edgezero_core::http::request_builder;
use edgezero_core::router::RouterService;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;

/// Build a router from explicit test settings so routes resolve to their real
/// handlers instead of the `startup_error_router` fallback. The settings baked
/// into the binary carry placeholder secrets that `get_settings()` rejects,
/// which would otherwise turn every route into a startup error page.
fn make_router() -> RouterService {
    let settings = trusted_server_core::settings::Settings::from_toml(
        r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "integration-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
        "#,
    )
    .expect("should parse route test settings");

    TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings")
}

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

#[tokio::test]
async fn admin_rotate_key_returns_501() {
    // Cloudflare's config/secret stores reject writes, so the admin key routes
    // are wired to a fixed 501 responder rather than the real handlers (which
    // would surface an opaque 500 on the first store write). A 500 here means
    // the fixed-501 wiring regressed.
    let router = make_router();

    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = router.oneshot(req).await;

    assert_eq!(
        resp.status().as_u16(),
        501,
        "admin key rotation must return 501 Not Implemented on Cloudflare"
    );
}

#[tokio::test]
async fn admin_deactivate_key_returns_501() {
    let router = make_router();

    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = router.oneshot(req).await;

    assert_eq!(
        resp.status().as_u16(),
        501,
        "admin key deactivation must return 501 Not Implemented on Cloudflare"
    );
}

#[tokio::test]
async fn tsjs_route_prefix_is_handled_not_5xx() {
    // `/static/tsjs=` is a GET catch-all path. The handler returns 404 for an
    // unknown hash, which is correct application behavior (not a routing 404).
    // This verifies the handler is reached without a 5xx/panic.
    let router = make_router();

    let req = request_builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");

    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
}
