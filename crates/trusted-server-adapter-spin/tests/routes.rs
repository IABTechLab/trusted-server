//! Smoke tests for the Spin adapter route wiring.
//!
//! Runs on the host target (no Spin runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! Spin runtime bindings or outbound network calls. Tests with business logic
//! that depends on external stores assert routing only; deterministic auth and
//! method gates assert exact status codes.

use edgezero_core::app::Hooks as _;
use edgezero_core::http::request_builder;
use edgezero_core::router::RouterService;
use trusted_server_adapter_spin::app::TrustedServerApp;
use trusted_server_core::settings::Settings;

/// Build the full application router from explicit test settings.
///
/// The settings baked into the binary contain placeholder secrets that
/// `get_settings()` rejects by design, which would turn every route into a
/// startup error page (and its route table into the fallback-only set).
/// The handler regex is the production-shaped `^/_ts/admin`, matching
/// `Settings::ADMIN_ENDPOINTS` and the default config, so the canonical
/// `/_ts/admin/keys/*` routes are auth-gated exactly as in production.
fn test_router() -> RouterService {
    let settings = Settings::from_toml(
        r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "route-test-proxy-secret"

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

#[test]
fn edgezero_manifest_loads_and_resolves_spin_stores() {
    let loader =
        edgezero_core::manifest::ManifestLoader::load_from_str(include_str!("../edgezero.toml"));
    let manifest = loader.manifest();

    assert!(
        manifest.stores.config.is_some(),
        "Spin EdgeZero manifest must enable config store injection"
    );
    assert_eq!(
        manifest.kv_store_name(edgezero_core::app::SPIN_ADAPTER),
        "default",
        "Spin KV label must match spin.toml key_value_stores"
    );
    assert!(
        manifest.secret_store_enabled(edgezero_core::app::SPIN_ADAPTER),
        "Spin EdgeZero manifest must enable secret handle injection"
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
    let router = test_router();

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
    // present) and that the route is actually reached (status != 404).
    let router = test_router();

    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = router.oneshot(req).await;

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
// Route smoke tests — verify all adapter routes are registered. Some handlers
// depend on platform stores or outbound proxy settings, so those tests assert
// "not the 404 route miss" rather than a full business result.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_is_routed_not_5xx() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    let status = resp.status().as_u16();
    // The tsjs route is matched by the /{*rest} catch-all. The handler returns
    // 404 for an unknown hash; that is application behaviour, not a route miss.
    assert!(status < 500, "tsjs route must not 5xx: got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/verify-signature must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_put_returns_405() {
    let router = test_router();
    let req = request_builder()
        .method("PUT")
        .uri("/verify-signature")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;

    assert_eq!(
        resp.status().as_u16(),
        405,
        "PUT /verify-signature must return method-not-allowed, not route miss"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/admin/keys/rotate must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_key_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/admin/keys/deactivate must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"adUnits":[]}"#))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(resp.status().as_u16(), 404, "/auction must be routed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/proxy")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_click_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/click")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/click must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/sign")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "GET /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/sign")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "POST /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/proxy-rebuild")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy-rebuild must be routed"
    );
}

// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_returns_401() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
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
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
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
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
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
    let router = test_router();
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
    let router = test_router();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_auth_fail_returns_401() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
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
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/deactivate")
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
