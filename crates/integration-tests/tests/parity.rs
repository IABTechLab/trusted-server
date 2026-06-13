//! Cross-adapter parity tests: Axum vs Cloudflare vs Spin in-process.
//!
//! Sends identical requests to all host-runnable adapters and asserts that:
//! - Response status codes match
//! - Critical headers (X-Geo-Info-Available, WWW-Authenticate on 401) match
//!
//! Fastly parity is verified via cargo test-fastly + Viceroy in CI.

// Both adapters define `TrustedServerApp` — alias both to avoid name collision.
// axum::http re-exports from the `http` crate, so HeaderMap types are identical.
use axum::body::Body as AxumBody;
use axum::http::Request as AxumRequest;
use edgezero_adapter_axum::EdgeZeroAxumService;
use edgezero_core::http::request_builder;
use edgezero_core::router::RouterService;
use http::HeaderMap;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp as AxumApp;
use trusted_server_adapter_cloudflare::app::TrustedServerApp as CloudflareApp;
use trusted_server_adapter_spin::app::TrustedServerApp as SpinApp;
use trusted_server_core::settings::Settings;

/// Shared test settings for all adapters.
///
/// The settings baked into the binaries contain placeholder secrets that
/// `get_settings()` rejects by design, so all routers are built through
/// their `routes_with_settings` testing seams from this known-good config.
/// The handler regex is the production-shaped `^/_ts/admin`, matching
/// `Settings::ADMIN_ENDPOINTS` and the default config, so the canonical
/// `/_ts/admin/keys/*` routes are auth-gated exactly as in production.
fn test_settings() -> Settings {
    Settings::from_toml(
        r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "parity-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
        "#,
    )
    .expect("should parse parity test settings")
}

/// Build the Axum adapter router from the shared test settings.
fn axum_router() -> RouterService {
    AxumApp::routes_with_settings(test_settings())
        .expect("should build Axum router from parity test settings")
}

/// Build the Cloudflare adapter router from the shared test settings.
fn cf_router() -> RouterService {
    CloudflareApp::routes_with_settings(test_settings())
        .expect("should build Cloudflare router from parity test settings")
}

/// Build the Spin adapter router from the shared test settings.
fn spin_router() -> RouterService {
    SpinApp::routes_with_settings(test_settings())
        .expect("should build Spin router from parity test settings")
}

/// Send a GET request to the Axum adapter and return (status, headers).
async fn axum_get(uri: &str) -> (u16, HeaderMap) {
    let mut svc = EdgeZeroAxumService::new(axum_router());
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
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send a POST request to the Axum adapter and return (status, headers, body bytes).
async fn axum_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    use http_body_util::BodyExt as _;
    let mut svc = EdgeZeroAxumService::new(axum_router());
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
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .expect("should collect body")
        .to_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn axum_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = axum_post(uri, body).await;
    (s, h)
}

/// Send a GET request to the Cloudflare adapter and return (status, headers).
async fn cf_get(uri: &str) -> (u16, HeaderMap) {
    let router = cf_router();
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build GET request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send a POST request to the Cloudflare adapter and return (status, headers, body bytes).
async fn cf_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    let router = cf_router();
    let req = request_builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build POST request");
    let resp = router.oneshot(req).await.expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().into_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn cf_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = cf_post(uri, body).await;
    (s, h)
}

/// Send a GET request to the Spin adapter and return (status, headers, body bytes).
async fn spin_get_body(uri: &str) -> (u16, HeaderMap, bytes::Bytes) {
    let router = spin_router();
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build GET request");
    let resp = router.oneshot(req).await.expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().into_bytes();
    (status, headers, body_bytes)
}

/// Send a GET request to the Spin adapter and return (status, headers).
async fn spin_get(uri: &str) -> (u16, HeaderMap) {
    let (s, h, _) = spin_get_body(uri).await;
    (s, h)
}

/// Send a POST request to the Spin adapter and return (status, headers, body bytes).
async fn spin_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    let router = spin_router();
    let req = request_builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build POST request");
    let resp = router.oneshot(req).await.expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().into_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn spin_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = spin_post(uri, body).await;
    (s, h)
}

// ---------------------------------------------------------------------------
// Route parity: same route → same status on all adapters
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_route_status_parity() {
    let (axum_status, _) = axum_get("/.well-known/trusted-server.json").await;
    let (cf_status, _) = cf_get("/.well-known/trusted-server.json").await;
    let (spin_status, _) = spin_get("/.well-known/trusted-server.json").await;
    assert_eq!(
        axum_status, cf_status,
        "/.well-known/trusted-server.json must return same status: axum={axum_status} cf={cf_status}"
    );
    assert_eq!(
        cf_status, spin_status,
        "/.well-known/trusted-server.json must return same status: cf={cf_status} spin={spin_status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_route_body_is_json_parity() {
    // known divergence: without real signing-key configuration both adapters may
    // return an error body. Assert that whichever body type each returns (JSON or
    // not) is consistent: if the Cloudflare adapter returns valid JSON then the
    // Axum adapter must also return valid JSON for the same route.
    use http_body_util::BodyExt as _;
    use serde_json::Value;

    let (axum_status, axum_body_bytes) = {
        let mut svc = EdgeZeroAxumService::new(axum_router());
        let req = AxumRequest::builder()
            .method("GET")
            .uri("/.well-known/trusted-server.json")
            .body(AxumBody::empty())
            .expect("should build GET request");
        let resp = svc
            .ready()
            .await
            .expect("should be ready")
            .call(req)
            .await
            .expect("should respond");
        let status = resp.status().as_u16();
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("should collect body")
            .to_bytes();
        (status, body)
    };

    let (cf_status, cf_body_bytes) = {
        let router = cf_router();
        let req = request_builder()
            .method("GET")
            .uri("/.well-known/trusted-server.json")
            .body(edgezero_core::body::Body::empty())
            .expect("should build GET request");
        let resp = router.oneshot(req).await.expect("should respond");
        let status = resp.status().as_u16();
        let body = resp.into_body().into_bytes();
        (status, body)
    };

    let (spin_status, _, spin_body_bytes) =
        spin_get_body("/.well-known/trusted-server.json").await;

    // This endpoint serves a static config document, so the parsed JSON bodies
    // must be identical across adapters (not merely both parsable as JSON).
    let axum_json: Option<Value> = serde_json::from_slice(&axum_body_bytes).ok();
    let cf_json: Option<Value> = serde_json::from_slice(&cf_body_bytes).ok();
    let spin_json: Option<Value> = serde_json::from_slice(&spin_body_bytes).ok();
    assert_eq!(
        axum_json, cf_json,
        "/.well-known/trusted-server.json body must match across adapters \
         (axum_status={axum_status} cf_status={cf_status})"
    );
    assert_eq!(
        cf_json, spin_json,
        "/.well-known/trusted-server.json body must match across adapters \
         (cf_status={cf_status} spin_status={spin_status})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_route_parity() {
    // known divergence: without real signing-key configuration the handler may
    // return 5xx. The parity assertion is that both adapters agree on the status
    // (routing and middleware are wired identically).
    let (axum_status, _) = axum_post_headers("/verify-signature", "{}").await;
    let (cf_status, _) = cf_post_headers("/verify-signature", "{}").await;
    let (spin_status, _) = spin_post_headers("/verify-signature", "{}").await;

    assert_ne!(axum_status, 404, "Axum /verify-signature must be routed");
    assert_ne!(
        cf_status, 404,
        "Cloudflare /verify-signature must be routed"
    );
    assert_ne!(spin_status, 404, "Spin /verify-signature must be routed");
    assert_eq!(
        axum_status, cf_status,
        "/verify-signature must return same status: axum={axum_status} cf={cf_status}"
    );
    assert_eq!(
        cf_status, spin_status,
        "/verify-signature must return same status: cf={cf_status} spin={spin_status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_unauthenticated_parity() {
    // Both adapters must return 401 for unauthenticated admin requests on the
    // canonical `/_ts/admin/keys/*` path that production config auth-gates.
    // The authenticated-path divergence (Axum→501 no-KV, CF→4xx no-KV)
    // is separate and not covered here.
    let (axum_status, axum_headers) = axum_post_headers("/_ts/admin/keys/rotate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/_ts/admin/keys/rotate", "{}").await;
    let (spin_status, spin_headers) = spin_post_headers("/_ts/admin/keys/rotate", "{}").await;

    assert_eq!(
        axum_status, 401,
        "Axum must return 401 for unauthenticated admin route"
    );
    assert_eq!(
        cf_status, 401,
        "Cloudflare must return 401 for unauthenticated admin route"
    );
    assert_eq!(
        spin_status, 401,
        "Spin must return 401 for unauthenticated admin route"
    );
    assert_eq!(
        axum_status, cf_status,
        "Axum and Cloudflare must return the same status for unauthenticated admin route"
    );
    assert_eq!(
        cf_status, spin_status,
        "Cloudflare and Spin must return the same status for unauthenticated admin route"
    );

    let axum_www_auth = axum_headers
        .get("www-authenticate")
        .expect("Axum 401 must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    let cf_www_auth = cf_headers
        .get("www-authenticate")
        .expect("Cloudflare 401 must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    assert_eq!(
        axum_www_auth, cf_www_auth,
        "WWW-Authenticate header value must match across adapters for /admin/keys/rotate"
    );
    assert!(
        axum_www_auth.starts_with("Basic"),
        "WWW-Authenticate must use Basic scheme: {axum_www_auth:?}"
    );
    let spin_www_auth = spin_headers
        .get("www-authenticate")
        .expect("Spin 401 must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    assert_eq!(
        cf_www_auth, spin_www_auth,
        "WWW-Authenticate value must match: cf={cf_www_auth:?} spin={spin_www_auth:?}"
    );
    assert!(
        spin_www_auth.starts_with("Basic"),
        "Spin WWW-Authenticate must use Basic scheme: {spin_www_auth:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_unauthenticated_parity() {
    // Mirror of admin_rotate_unauthenticated_parity for the deactivate endpoint.
    let (axum_status, axum_headers) = axum_post_headers("/_ts/admin/keys/deactivate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/_ts/admin/keys/deactivate", "{}").await;
    let (spin_status, spin_headers) = spin_post_headers("/_ts/admin/keys/deactivate", "{}").await;

    assert_eq!(
        axum_status, 401,
        "Axum must return 401 for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        cf_status, 401,
        "Cloudflare must return 401 for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        spin_status, 401,
        "Spin must return 401 for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        axum_status, cf_status,
        "Axum and Cloudflare must return the same status for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        cf_status, spin_status,
        "Cloudflare and Spin must return the same status for unauthenticated admin/keys/deactivate"
    );

    let axum_www_auth = axum_headers
        .get("www-authenticate")
        .expect("Axum 401 on admin/keys/deactivate must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    let cf_www_auth = cf_headers
        .get("www-authenticate")
        .expect("Cloudflare 401 on admin/keys/deactivate must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    assert_eq!(
        axum_www_auth, cf_www_auth,
        "WWW-Authenticate header value must match across adapters for /admin/keys/deactivate"
    );
    assert!(
        axum_www_auth.starts_with("Basic"),
        "WWW-Authenticate must use Basic scheme: {axum_www_auth:?}"
    );
    let spin_www_auth = spin_headers
        .get("www-authenticate")
        .expect("Spin 401 on admin/keys/deactivate must include WWW-Authenticate")
        .to_str()
        .expect("should be valid UTF-8");
    assert_eq!(
        cf_www_auth, spin_www_auth,
        "WWW-Authenticate value must match: cf={cf_www_auth:?} spin={spin_www_auth:?}"
    );
    assert!(
        spin_www_auth.starts_with("Basic"),
        "Spin WWW-Authenticate must use Basic scheme: {spin_www_auth:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn geo_header_parity_on_all_responses() {
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
        let (spin_status, spin_headers) = if *method == "GET" {
            spin_get(path).await
        } else {
            spin_post_headers(path, body).await
        };

        assert!(
            axum_headers.contains_key("x-geo-info-available"),
            "Axum: {method} {path} (status={axum_status}) must have X-Geo-Info-Available"
        );
        assert!(
            cf_headers.contains_key("x-geo-info-available"),
            "Cloudflare: {method} {path} (status={cf_status}) must have X-Geo-Info-Available"
        );
        let axum_geo = axum_headers
            .get("x-geo-info-available")
            .expect("should have x-geo-info-available after assert")
            .to_str()
            .expect("should be valid UTF-8");
        let cf_geo = cf_headers
            .get("x-geo-info-available")
            .expect("should have x-geo-info-available after assert")
            .to_str()
            .expect("should be valid UTF-8");
        assert_eq!(
            axum_geo, cf_geo,
            "{method} {path}: X-Geo-Info-Available value must match across adapters \
             (axum={axum_geo:?} cf={cf_geo:?})"
        );
        // Spin hardcodes X-Geo-Info-Available: false (no geo headers available in the
        // Spin runtime). Value comparison against axum/cf would lock in a known asymmetry,
        // so presence is the gate here.
        assert!(
            spin_headers.contains_key("x-geo-info-available"),
            "Spin: {method} {path} (status={spin_status}) must have X-Geo-Info-Available"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_not_challenged_by_auth_parity() {
    let (axum_status, _) = axum_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (cf_status, _) = cf_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (spin_status, _) = spin_post_headers("/auction", r#"{"adUnits":[]}"#).await;

    assert_ne!(axum_status, 401, "Axum /auction must not 401");
    assert_ne!(cf_status, 401, "Cloudflare /auction must not 401");
    assert_ne!(spin_status, 401, "Spin /auction must not 401");
    assert_ne!(axum_status, 404, "Axum /auction must be routed (not 404)");
    assert_ne!(cf_status, 404, "Cloudflare /auction must be routed (not 404)");
    assert_ne!(spin_status, 404, "Spin /auction must be routed (not 404)");
    assert_eq!(
        axum_status, cf_status,
        "/auction must return the same status across adapters: \
         axum={axum_status} cf={cf_status}"
    );
    assert_eq!(
        cf_status, spin_status,
        "/auction must return the same status across adapters: \
         cf={cf_status} spin={spin_status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_proxy_fallback_parity() {
    // Cookie (Set-Cookie) parity for the publisher proxy requires a live origin.
    // Without an origin, both adapters return an error (4xx or 5xx). The parity
    // assertion is that Set-Cookie presence matches across adapters regardless of
    // whether the proxy succeeds.
    let (axum_status, axum_headers) = axum_get("/").await;
    let (cf_status, cf_headers) = cf_get("/").await;
    let (spin_status, spin_headers) = spin_get("/").await;

    // All adapters must agree: either all proxy to the origin or all fail.
    assert_eq!(
        axum_status >= 500,
        cf_status >= 500,
        "publisher fallback 5xx behaviour must match: axum={axum_status} cf={cf_status}"
    );
    assert_eq!(
        cf_status >= 500,
        spin_status >= 500,
        "publisher fallback 5xx behaviour must match: cf={cf_status} spin={spin_status}"
    );

    let axum_has_cookie = axum_headers.contains_key("set-cookie");
    let cf_has_cookie = cf_headers.contains_key("set-cookie");
    let spin_has_cookie = spin_headers.contains_key("set-cookie");
    assert_eq!(
        axum_has_cookie, cf_has_cookie,
        "Set-Cookie presence must match: axum={axum_has_cookie} cf={cf_has_cookie}"
    );
    assert_eq!(
        cf_has_cookie, spin_has_cookie,
        "Set-Cookie presence must match: cf={cf_has_cookie} spin={spin_has_cookie}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_route_returns_same_status_parity() {
    let (axum_status, _) = axum_get("/this-route-does-not-exist-abc123").await;
    let (cf_status, _) = cf_get("/this-route-does-not-exist-abc123").await;
    let (spin_status, _) = spin_get("/this-route-does-not-exist-abc123").await;

    assert_eq!(
        axum_status, cf_status,
        "unknown routes must return same status: axum={axum_status} cf={cf_status}"
    );
    assert_eq!(
        cf_status, spin_status,
        "unknown routes must return same status: cf={cf_status} spin={spin_status}"
    );
}
