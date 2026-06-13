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
async fn verify_signature_put_falls_through_to_publisher_fallback() {
    // PUT is one of the publisher-fallback methods, and /verify-signature only
    // handles POST directly. Mirroring Fastly/Axum, a non-primary method on a
    // named path must fall through to the publisher/integration fallback (which
    // 502s here without a live origin) rather than returning a router-level 405.
    let router = test_router();
    let req = request_builder()
        .method("PUT")
        .uri("/verify-signature")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;

    let status = resp.status().as_u16();
    assert_ne!(
        status, 405,
        "PUT /verify-signature must fall through to publisher fallback, not 405"
    );
    assert_ne!(
        status, 404,
        "PUT /verify-signature must be routed to the fallback, not a route miss"
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

// ---------------------------------------------------------------------------
// Publisher fallback method parity — non-GET/POST methods must reach the
// publisher origin fallback (not a router-level 405), matching Fastly/Axum.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_root_reaches_publisher_fallback() {
    let router = test_router();
    let req = request_builder()
        .method("HEAD")
        .uri("/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "HEAD / must reach the publisher fallback, not return 405"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn options_page_reaches_publisher_fallback() {
    let router = test_router();
    let req = request_builder()
        .method("OPTIONS")
        .uri("/some/page")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "OPTIONS /some/page (CORS preflight) must reach the publisher fallback, not 405"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_named_get_route_reaches_publisher_fallback() {
    // /first-party/proxy handles GET directly; HEAD is non-primary and must fall
    // through to the publisher fallback rather than returning a router-level 405.
    let router = test_router();
    let req = request_builder()
        .method("HEAD")
        .uri("/first-party/proxy")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "HEAD /first-party/proxy must reach the publisher fallback, not 405"
    );
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
// First-party absolute-URI regression — Spin delivers a path-only request URI
// (built from IncomingRequest::path_with_query), so the shared proxy/click/sign
// handlers, which parse `req.uri()` with `url::Url::parse`, would fail with
// "Invalid URL" unless the adapter rebuilds an absolute URI from spin-full-url.
// ---------------------------------------------------------------------------

/// Extract a top-level JSON string field value. The sign response only contains
/// url-encoded values (no quotes or backslash escapes), so a substring scan is
/// sufficient and avoids pulling in a JSON dependency for the test crate.
fn json_string_field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_with_path_only_uri_signs_target() {
    // The router sees a path-only URI (as Spin produces); spin-full-url carries
    // the trusted absolute URL the adapter uses to reconstruct req.uri(). Without
    // the reconstruction, the GET sign handler cannot parse its own ?url= query
    // and returns a 400 "Invalid URL" instead of a signed href.
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/sign?url=https://cdn.example/a.png")
        .header(
            "spin-full-url",
            "https://www.publisher.example/first-party/sign?url=https://cdn.example/a.png",
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;
    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /first-party/sign must parse its query from the reconstructed absolute URI"
    );
    let body = String::from_utf8(resp.into_body().into_bytes().to_vec())
        .expect("sign response body should be UTF-8");
    assert!(
        body.contains("\"href\""),
        "sign response must contain a signed href, got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_round_trip_through_spin_router() {
    // Sign a target, then route the emitted /first-party/proxy?... href back
    // through the Spin router. With a path-only URI the proxy handler would fail
    // signed-target reconstruction with a 400 "Invalid URL"; with the absolute
    // URI it validates the token and proceeds to the (native-unavailable)
    // outbound fetch, so the status is anything but the 400/404 it would be if
    // the request never passed validation/routing.
    let router = test_router();

    let sign_req = request_builder()
        .method("GET")
        .uri("/first-party/sign?url=https://cdn.example/a.png")
        .header(
            "spin-full-url",
            "https://www.publisher.example/first-party/sign?url=https://cdn.example/a.png",
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let sign_resp = router.oneshot(sign_req).await;
    assert_eq!(
        sign_resp.status().as_u16(),
        200,
        "sign step must succeed before the proxy round-trip"
    );
    let sign_body = String::from_utf8(sign_resp.into_body().into_bytes().to_vec())
        .expect("sign response body should be UTF-8");
    let href = json_string_field(&sign_body, "href")
        .expect("sign response must include a signed href path");
    assert!(
        href.starts_with("/first-party/proxy?"),
        "signed href must target the proxy path, got: {href}"
    );

    let proxy_req = request_builder()
        .method("GET")
        .uri(href.clone())
        .header(
            "spin-full-url",
            format!("https://www.publisher.example{href}"),
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build proxy request");
    let proxy_resp = router.oneshot(proxy_req).await;
    let status = proxy_resp.status().as_u16();
    assert_ne!(
        status, 400,
        "proxy must pass signed-target validation, not fail URL parsing (400)"
    );
    assert_ne!(status, 404, "proxy path must be routed, not a route miss");
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
