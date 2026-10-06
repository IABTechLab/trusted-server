//! Cross-adapter parity tests: Axum vs Cloudflare vs Spin in-process.
//!
//! Sends identical requests to all host-runnable adapters and asserts that:
//! - Response status codes match
//! - Critical headers (X-Geo-Info-Available, WWW-Authenticate on 401) match
//!
//! Fastly parity is verified via cargo test-fastly + Viceroy in CI.

// Both adapters define `TrustedServerApp` — alias both to avoid name collision.
// axum::http re-exports from the `http` crate, so HeaderMap types are identical.
use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::http::Request as AxumRequest;
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use edgezero_core::http::request_builder;
use edgezero_core::router::RouterService;
use http::HeaderMap;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp as AxumApp;
use trusted_server_adapter_cloudflare::app::TrustedServerApp as CloudflareApp;
use trusted_server_adapter_spin::app::TrustedServerApp as SpinApp;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::nextjs_auction;

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
    let body_bytes = resp.into_body().into_bytes().unwrap_or_default();
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
    let body_bytes = resp.into_body().into_bytes().unwrap_or_default();
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
    let body_bytes = resp.into_body().into_bytes().unwrap_or_default();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn spin_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = spin_post(uri, body).await;
    (s, h)
}

/// Send a POST request to the Spin adapter with additional request headers.
async fn spin_post_with_headers(
    uri: &str,
    body: &str,
    extra_headers: &[(&str, &str)],
) -> (u16, HeaderMap) {
    let router = spin_router();
    let mut builder = request_builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let req = builder
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build POST request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an authorized JSON request to the Axum adapter and return (status, headers).
async fn axum_authorized_json(method: &str, uri: &str, body: &str) -> (u16, HeaderMap) {
    let mut svc = EdgeZeroAxumService::new(axum_router());
    let req = AxumRequest::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
        .header("content-type", "application/json")
        .body(AxumBody::from(body.to_owned()))
        .expect("should build authorized JSON request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an authorized JSON request to the Cloudflare adapter and return (status, headers).
async fn cf_authorized_json(method: &str, uri: &str, body: &str) -> (u16, HeaderMap) {
    let router = cf_router();
    let req = request_builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build authorized JSON request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an authorized JSON request to the Spin adapter and return (status, headers).
async fn spin_authorized_json(method: &str, uri: &str, body: &str) -> (u16, HeaderMap) {
    let router = spin_router();
    let req = request_builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build authorized JSON request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an OPTIONS request to the Axum adapter and return (status, headers).
async fn axum_options(uri: &str) -> (u16, HeaderMap) {
    let mut svc = EdgeZeroAxumService::new(axum_router());
    let req = AxumRequest::builder()
        .method("OPTIONS")
        .uri(uri)
        .body(AxumBody::empty())
        .expect("should build OPTIONS request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an OPTIONS request to the Cloudflare adapter and return (status, headers).
async fn cf_options(uri: &str) -> (u16, HeaderMap) {
    let router = cf_router();
    let req = request_builder()
        .method("OPTIONS")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build OPTIONS request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
}

/// Send an OPTIONS request to the Spin adapter and return (status, headers).
async fn spin_options(uri: &str) -> (u16, HeaderMap) {
    let router = spin_router();
    let req = request_builder()
        .method("OPTIONS")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build OPTIONS request");
    let resp = router.oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
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
        let body = resp.into_body().into_bytes().unwrap_or_default();
        (status, body)
    };

    let (spin_status, _, spin_body_bytes) = spin_get_body("/.well-known/trusted-server.json").await;

    // This endpoint serves a static config document, so the bodies must be
    // identical across adapters — not merely both parsable (or both unparsable)
    // as JSON. Parse first so JSON key ordering / whitespace differences do not
    // cause false failures, but never treat `None == None` as body parity.
    let axum_json: Option<Value> = serde_json::from_slice(&axum_body_bytes).ok();
    let cf_json: Option<Value> = serde_json::from_slice(&cf_body_bytes).ok();
    let spin_json: Option<Value> = serde_json::from_slice(&spin_body_bytes).ok();

    // All adapters must agree on whether the body is JSON. A regression where
    // one returns the discovery JSON and another returns a non-JSON error body
    // is a definitive parity break.
    assert_eq!(
        axum_json.is_some(),
        cf_json.is_some(),
        "/.well-known/trusted-server.json body type (JSON vs non-JSON) must match \
         across adapters (axum_status={axum_status} cf_status={cf_status})"
    );
    assert_eq!(
        cf_json.is_some(),
        spin_json.is_some(),
        "/.well-known/trusted-server.json body type (JSON vs non-JSON) must match \
         across adapters (cf_status={cf_status} spin_status={spin_status})"
    );

    match (axum_json, cf_json, spin_json) {
        // All adapters serve JSON: compare parsed values so serialization
        // differences (key ordering, whitespace) do not cause false failures.
        (Some(axum_value), Some(cf_value), Some(spin_value)) => {
            assert_eq!(
                axum_value, cf_value,
                "/.well-known/trusted-server.json JSON body must match across adapters \
                 (axum_status={axum_status} cf_status={cf_status})"
            );
            assert_eq!(
                cf_value, spin_value,
                "/.well-known/trusted-server.json JSON body must match across adapters \
                 (cf_status={cf_status} spin_status={spin_status})"
            );
        }
        // Without seeded signing/JWKS data the adapters take the same error path
        // and return a non-JSON error body. Compare raw bytes so diverging error
        // payloads are caught instead of all parsing to `None`.
        _ => {
            assert_eq!(
                axum_body_bytes, cf_body_bytes,
                "/.well-known/trusted-server.json non-JSON body must match across adapters \
                 (axum_status={axum_status} cf_status={cf_status})"
            );
            assert_eq!(
                cf_body_bytes, spin_body_bytes,
                "/.well-known/trusted-server.json non-JSON body must match across adapters \
                 (cf_status={cf_status} spin_status={spin_status})"
            );
        }
    }
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
async fn spin_legacy_admin_aliases_are_denied_locally_not_proxied() {
    // The production handler regex `^/_ts/admin` only matches the canonical
    // `/_ts/admin/keys/*` paths. Legacy aliases must therefore fail closed with a
    // local 404 instead of reaching either the admin handlers or the publisher
    // fallback.
    for alias in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        let (canonical_status, _) = spin_post_headers(&format!("/_ts{alias}"), "{}").await;
        assert_eq!(
            canonical_status, 401,
            "canonical /_ts{alias} must challenge unauthenticated callers"
        );

        for method in ["POST", "GET"] {
            let (alias_status, alias_headers) =
                spin_authorized_json(method, alias, r#"{"key_id":"leak-me"}"#).await;
            assert_eq!(
                alias_status, 404,
                "Spin legacy {method} {alias} must be denied locally with 404"
            );
            assert!(
                !alias_headers.contains_key("www-authenticate"),
                "Spin legacy {method} {alias} must not issue an admin auth challenge"
            );
        }
    }
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
    assert_ne!(
        cf_status, 404,
        "Cloudflare /auction must be routed (not 404)"
    );
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
async fn page_bids_options_preflight_denied_parity() {
    // OPTIONS /_ts/page-bids is a CORS preflight to a side-effecting endpoint.
    // Every adapter must refuse it with 403 rather than proxy it to the origin:
    // a permissive origin preflight would let a cross-site page defeat the GET
    // handler's `X-TSJS-Page-Bids` gate and trigger real auctions in a visitor's
    // browser. The denial is unconditional (independent of creative-opportunity
    // configuration), so all adapters must agree on 403.
    //
    // The deprecated `/__ts/page-bids` alias routes to the same handler, so it
    // must deny the preflight identically — an alias that fell through to the
    // origin would reopen the hole the canonical path closes.
    for path in ["/_ts/page-bids", "/__ts/page-bids"] {
        let (axum_status, _) = axum_options(path).await;
        let (cf_status, _) = cf_options(path).await;
        let (spin_status, _) = spin_options(path).await;

        assert_eq!(
            axum_status, 403,
            "Axum OPTIONS {path} must be denied with 403, got {axum_status}"
        );
        assert_eq!(
            cf_status, 403,
            "Cloudflare OPTIONS {path} must be denied with 403, got {cf_status}"
        );
        assert_eq!(
            spin_status, 403,
            "Spin OPTIONS {path} must be denied with 403, got {spin_status}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spin_auction_ignores_spoofed_forwarded_headers() {
    // POST /auction feeds prebid request signing via `RequestInfo::from_request`,
    // which trusts `Forwarded` / `X-Forwarded-*` when the adapter has not stripped
    // them. On Spin, normalization must strip those spoofable headers before the
    // handler runs, so a spoofed request cannot influence routing or the trusted
    // authority used in signed metadata: it must behave exactly like a clean one.
    let body = r#"{"adUnits":[]}"#;
    let (clean_status, _) = spin_post_headers("/auction", body).await;
    let (spoofed_status, _) = spin_post_with_headers(
        "/auction",
        body,
        &[
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "http"),
            ("forwarded", "host=evil.example;proto=http"),
        ],
    )
    .await;

    assert_ne!(spoofed_status, 401, "Spin /auction must not 401");
    assert_ne!(
        spoofed_status, 404,
        "Spin /auction must be routed (not 404)"
    );
    assert_eq!(
        clean_status, spoofed_status,
        "spoofed forwarded headers must not change Spin /auction behaviour: \
         clean={clean_status} spoofed={spoofed_status}"
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

    // Axum and Cloudflare must agree on the exact fallback outcome. Spin uses
    // a different HTTP client, so until a live origin is configured the parity
    // guard only requires it to fail in the same broad class.
    assert_eq!(
        axum_status, cf_status,
        "publisher fallback status must match exactly: axum={axum_status} cf={cf_status}"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_admin_aliases_are_denied_locally_not_proxied() {
    // The production handler regex `^/_ts/admin` only matches the canonical
    // `/_ts/admin/keys/*` paths, so the legacy `/admin/keys/*` aliases are not
    // auth-gated. They must be denied locally with 404 — never routed to the key
    // handlers (which would execute admin operations), and never left unrouted to
    // fall through to the publisher fallback, which forwards the request
    // (including any `Authorization` header and key-management body) to the origin
    // and leaks admin credentials.
    //
    // Primary guard: assert the route tables directly. The legacy aliases must
    // be registered (to the local deny) so they can never reach the publisher
    // fallback as an unrouted path.
    let cf_registered: Vec<(String, String)> = cf_router()
        .routes()
        .iter()
        .map(|route| (route.method().to_string(), route.path().to_string()))
        .collect();
    let spin_registered: Vec<(String, String)> = spin_router()
        .routes()
        .iter()
        .map(|route| (route.method().to_string(), route.path().to_string()))
        .collect();
    let cf_is_registered =
        |method: &str, path: &str| cf_registered.iter().any(|(m, p)| m == method && p == path);
    let spin_is_registered = |method: &str, path: &str| {
        spin_registered
            .iter()
            .any(|(m, p)| m == method && p == path)
    };

    for path in ["/_ts/admin/keys/rotate", "/_ts/admin/keys/deactivate"] {
        assert!(
            cf_is_registered("POST", path),
            "Cloudflare POST {path} must be a registered route"
        );
        assert!(
            spin_is_registered("POST", path),
            "Spin POST {path} must be a registered route"
        );
    }
    for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        for method in ["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"] {
            assert!(
                cf_is_registered(method, path),
                "Cloudflare {method} {path} must be a registered route"
            );
            assert!(
                spin_is_registered(method, path),
                "Spin {method} {path} must be a registered route"
            );
        }
    }

    // Secondary guard: confirm runtime behavior. Canonical paths challenge
    // unauthenticated callers (401). Legacy aliases are denied locally with 404
    // and carry no auth challenge — a reintroduced key handler at these ungated
    // paths would not return 404, and the publisher fallback would not either, so
    // 404 proves the local deny ran.
    for alias in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        let (canonical_status, _) = cf_post_headers(&format!("/_ts{alias}"), "{}").await;
        assert_eq!(
            canonical_status, 401,
            "Cloudflare canonical /_ts{alias} must challenge unauthenticated callers"
        );
        let (spin_canonical_status, _) = spin_post_headers(&format!("/_ts{alias}"), "{}").await;
        assert_eq!(
            spin_canonical_status, 401,
            "Spin canonical /_ts{alias} must challenge unauthenticated callers"
        );

        for method in ["POST", "GET"] {
            let (axum_status, axum_headers) =
                axum_authorized_json(method, alias, r#"{"key_id":"leak-me"}"#).await;
            let (cf_status, cf_headers) =
                cf_authorized_json(method, alias, r#"{"key_id":"leak-me"}"#).await;
            let (spin_status, spin_headers) =
                spin_authorized_json(method, alias, r#"{"key_id":"leak-me"}"#).await;
            assert_eq!(
                axum_status, 404,
                "Axum legacy {method} {alias} must be denied locally with 404"
            );
            assert_eq!(
                cf_status, 404,
                "Cloudflare legacy {method} {alias} must be denied locally with 404"
            );
            assert_eq!(
                axum_status, cf_status,
                "legacy {method} {alias} status must match across adapters"
            );
            assert_eq!(
                cf_status, spin_status,
                "legacy {method} {alias} status must match across adapters"
            );
            assert!(
                !axum_headers.contains_key("www-authenticate"),
                "Axum legacy {method} {alias} must not issue an admin auth challenge"
            );
            assert!(
                !cf_headers.contains_key("www-authenticate"),
                "Cloudflare legacy {method} {alias} must not issue an admin auth challenge"
            );
            assert!(
                !spin_headers.contains_key("www-authenticate"),
                "Spin legacy {method} {alias} must not issue an admin auth challenge"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_cache_purge_not_implemented_parity() {
    // The template cache is Fastly-backed, so the other three adapters answer 501 rather
    // than letting the path fall through to the publisher origin and 404 — a CMS purge
    // webhook needs to tell "not supported here" from "no such endpoint".
    let body = r#"{"scope":"all"}"#;

    let (axum_status, _) = axum_authorized_json("POST", "/_ts/admin/cache/purge", body).await;
    let (cf_status, _) = cf_authorized_json("POST", "/_ts/admin/cache/purge", body).await;
    let (spin_status, _) = spin_authorized_json("POST", "/_ts/admin/cache/purge", body).await;

    assert_eq!(axum_status, 501, "Axum must answer cache purge with 501");
    assert_eq!(
        cf_status, 501,
        "Cloudflare must answer cache purge with 501"
    );
    assert_eq!(spin_status, 501, "Spin must answer cache purge with 501");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_cache_purge_unauthenticated_parity() {
    // Guards the test above from passing for the wrong reason. Without credentials the
    // path must 401, which proves the 501s were reached through auth rather than being
    // the 401s of a probe that never arrived at a handler.
    let body = r#"{"scope":"all"}"#;

    let (axum_status, _) = axum_post_headers("/_ts/admin/cache/purge", body).await;
    let (cf_status, _) = cf_post_headers("/_ts/admin/cache/purge", body).await;
    let (spin_status, _) = spin_post_headers("/_ts/admin/cache/purge", body).await;

    for (adapter, status) in [
        ("Axum", axum_status),
        ("Cloudflare", cf_status),
        ("Spin", spin_status),
    ] {
        assert_eq!(
            status, 401,
            "{adapter} must require auth on the cache purge path"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_cache_purge_rejects_credential_forwarding_methods() {
    // The guard the Fastly route exists for, asserted cross-adapter: a method the route
    // does not claim falls through to the publisher with the Authorization header still
    // attached. Every method must be answered locally, never forwarded.
    for method in ["GET", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"] {
        let (axum_status, _) = axum_authorized_json(method, "/_ts/admin/cache/purge", "").await;
        let (cf_status, _) = cf_authorized_json(method, "/_ts/admin/cache/purge", "").await;
        let (spin_status, _) = spin_authorized_json(method, "/_ts/admin/cache/purge", "").await;

        for (adapter, status) in [
            ("Axum", axum_status),
            ("Cloudflare", cf_status),
            ("Spin", spin_status),
        ] {
            assert_eq!(
                status, 501,
                "{adapter} must answer {method} locally; a fallthrough would ship the \
                 admin credential to the origin"
            );
        }
    }
}

/// A known non-regulated location permits the fixture's server-side auction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adapter_buffers_nextjs_auction_output() {
    let client = Arc::new(nextjs_auction::NextJsAuctionOrigin::default());
    let settings = nextjs_auction::settings();
    let services = nextjs_auction::services(Arc::clone(&client));
    let routers = [
        (
            "Axum",
            AxumApp::routes_with_settings_and_services(settings.clone(), services.clone()),
        ),
        (
            "Cloudflare",
            CloudflareApp::routes_with_settings_and_services(settings.clone(), services.clone()),
        ),
        (
            "Spin",
            SpinApp::routes_with_settings_and_services(settings, services),
        ),
    ];
    let mut expected_html = None;
    for (adapter, router) in routers {
        // Reset per adapter so each count is independently meaningful rather
        // than a running total that a positional assertion cannot distinguish.
        client.reset_auction_requests();
        let request = request_builder()
            .method("GET")
            .uri("https://test-publisher.example.com/article")
            .header("host", "test-publisher.example.com")
            .header("accept", "text/html")
            .body(edgezero_core::body::Body::empty())
            .expect("should build publisher navigation");
        let response = router
            .expect("should build router with fixed services")
            .oneshot(request)
            .await
            .expect("should serve publisher navigation");
        assert_eq!(
            response.status(),
            200,
            "{adapter} should serve fixture HTML"
        );
        let body = response
            .into_body()
            .into_bytes()
            .expect("should buffer adapter output");
        let html = String::from_utf8(body.to_vec()).expect("should emit UTF-8 HTML");
        assert_eq!(
            client.auction_requests(),
            1,
            "{adapter} should dispatch exactly one auction"
        );
        let document = scraper::Html::parse_document(&html);
        let scripts = scraper::Selector::parse("script").expect("should parse script selector");
        let payload: String = document
            .select(&scripts)
            .filter_map(|script| {
                let text: String = script.text().collect();
                let array = text
                    .strip_prefix("self.__next_f.push(")?
                    .strip_suffix(')')?;
                let push: serde_json::Value =
                    serde_json::from_str(array).expect("should retain valid Flight push JSON");
                Some(
                    push[1]
                        .as_str()
                        .expect("should retain Flight string payload")
                        .to_owned(),
                )
            })
            .collect();
        assert_eq!(
            payload,
            nextjs_auction::expected_rewritten_flight_payload(),
            "{adapter} should rewrite the URL and T length while preserving complete payload bytes"
        );
        assert!(
            !html.contains("origin.test-publisher.example.com/app"),
            "{adapter} should remove the origin URL"
        );
        let first = html
            .find("self.__next_f.push")
            .expect("should retain first RSC script");
        let between = html
            .find("window.between=true")
            .expect("should retain intervening script");
        let last = html
            .rfind("self.__next_f.push")
            .expect("should retain last RSC script");
        assert!(
            first < between && between < last,
            "{adapter} should preserve script order"
        );
        let bids = html
            .find("var b=JSON.parse(")
            .expect("should inject auction bids");
        let suffix = html.find("<p>suffix</p>").expect("should retain suffix");
        let close = html
            .rfind("</body>")
            .expect("should retain structural close");
        assert!(
            suffix < bids && bids < close,
            "{adapter} should inject bids at the structural body close"
        );
        assert!(
            html.contains("fixture-creative"),
            "{adapter} should include deterministic auction creative"
        );
        assert!(
            html[bids..].ends_with("</script></body></html>"),
            "{adapter} should place bid markup immediately before the body close"
        );
        assert!(
            !html.contains("__ts_rsc_") && !html.contains("<!--ts-inline-body-close-"),
            "{adapter} should not leak placeholders: {html}"
        );
        // Cross-adapter agreement only. The bytes that matter — the reconstructed
        // Flight payload with its recomputed T length, script order, and the
        // body-close tail — are pinned absolutely above, so a shared-core
        // regression is caught there rather than here.
        if let Some(expected) = &expected_html {
            assert_eq!(
                &html, expected,
                "{adapter} should match the complete buffered Axum HTML"
            );
        } else {
            expected_html = Some(html);
        }
    }
}
#[allow(clippy::panic)]
#[cfg(test)]
mod trace_parity {
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use bytes::Bytes;
    use edgezero_core::key_value_store::{KvError, KvPage};
    use edgezero_core::request::{
        CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, RequestIngress,
        TargetUnavailable,
    };
    use error_stack::Report;
    use http::{Method, StatusCode, header};
    use trusted_server_core::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::platform::{
        BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec,
        PlatformConfigStore, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
        PlatformKvStore, PlatformPendingRequest, PlatformResponse, PlatformSecretStore,
        PlatformSelectResult, RuntimeServices, StoreId, StoreName,
    };
    use trusted_server_core::trace::TraceTerminalResponse;

    use super::*;

    struct ForbiddenLifecycle;

    impl PlatformConfigStore for ForbiddenLifecycle {
        fn get(&self, _store: &StoreName, _key: &str) -> Result<String, Report<PlatformError>> {
            panic!("should not read trace config per request");
        }
        fn put(
            &self,
            _store: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            panic!("should not write trace config");
        }
        fn delete(&self, _store: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            panic!("should not delete trace config");
        }
    }
    impl PlatformSecretStore for ForbiddenLifecycle {
        fn get_bytes(
            &self,
            _store: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            panic!("should not read trace secrets");
        }
        fn create(
            &self,
            _store: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            panic!("should not write trace secrets");
        }
        fn delete(&self, _store: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            panic!("should not delete trace secrets");
        }
    }
    impl PlatformBackend for ForbiddenLifecycle {
        fn naming_policy(&self) -> BackendNamingPolicy {
            panic!("should not resolve trace backends");
        }
        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            panic!("should not predict trace backends");
        }
        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            panic!("should not register trace backends");
        }
    }
    #[async_trait(?Send)]
    impl PlatformKvStore for ForbiddenLifecycle {
        async fn get_bytes(&self, _key: &str) -> Result<Option<Bytes>, KvError> {
            panic!("should not read trace identity");
        }
        async fn put_bytes(&self, _key: &str, _value: Bytes) -> Result<(), KvError> {
            panic!("should not write trace identity");
        }
        async fn put_bytes_with_ttl(
            &self,
            _key: &str,
            _value: Bytes,
            _ttl: Duration,
        ) -> Result<(), KvError> {
            panic!("should not refresh trace identity");
        }
        async fn delete(&self, _key: &str) -> Result<(), KvError> {
            panic!("should not delete trace identity");
        }
        async fn list_keys_page(
            &self,
            _prefix: &str,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<KvPage, KvError> {
            panic!("should not enumerate trace identity");
        }
    }
    #[async_trait(?Send)]
    impl PlatformHttpClient for ForbiddenLifecycle {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            panic!("should not contact trace origin");
        }
        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            panic!("should not launch trace auctions");
        }
        async fn select(
            &self,
            _requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            panic!("should not collect trace auctions");
        }
    }
    #[async_trait(?Send)]
    impl AuctionTelemetrySink for ForbiddenLifecycle {
        fn is_enabled(&self) -> bool {
            panic!("should not inspect trace auction telemetry");
        }
        async fn emit_auction_events(
            &self,
            _services: &RuntimeServices,
            _batch: AuctionEventBatch,
        ) -> Result<(), Report<TrustedServerError>> {
            panic!("should not emit trace auction telemetry");
        }
    }
    struct CountingGeo(Arc<AtomicUsize>);
    impl PlatformGeo for CountingGeo {
        fn lookup(&self, ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            assert_eq!(
                ip,
                Some(
                    "192.0.2.99"
                        .parse()
                        .expect("should parse injected example IP")
                ),
                "should use injected trusted client metadata"
            );
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(GeoInfo {
                city: String::new(),
                country: "GB".to_owned(),
                continent: String::new(),
                latitude: 0.0,
                longitude: 0.0,
                metro_code: 0,
                region: Some("EX".to_owned()),
                asn: Some(64512),
            }))
        }
    }

    fn routers(
        enabled: bool,
        auth_pattern: Option<&str>,
        calls: Arc<AtomicUsize>,
    ) -> Vec<(&'static str, RouterService)> {
        routers_with_client(
            enabled,
            auth_pattern,
            calls,
            Some(
                "192.0.2.99"
                    .parse()
                    .expect("should parse injected client IP"),
            ),
        )
    }

    fn routers_with_client(
        enabled: bool,
        auth_pattern: Option<&str>,
        calls: Arc<AtomicUsize>,
        client_ip: Option<IpAddr>,
    ) -> Vec<(&'static str, RouterService)> {
        let mut settings = test_settings();
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &serde_json::json!({"enabled":true,"trace_page_enabled":enabled}),
            )
            .expect("should insert trace settings");
        if let Some(pattern) = auth_pattern {
            settings.handlers.insert(0,serde_json::from_value(serde_json::json!({"path":pattern,"username":"example-user","password":"example-password"})).expect("should insert trace auth rule"));
        }
        let forbidden = Arc::new(ForbiddenLifecycle);
        let services = RuntimeServices::builder()
            .config_store(forbidden.clone())
            .secret_store(forbidden.clone())
            .kv_store(forbidden.clone())
            .backend(forbidden.clone())
            .http_client(forbidden.clone())
            .auction_telemetry_sink(forbidden)
            .geo(Arc::new(CountingGeo(calls)))
            .client_info(ClientInfo {
                client_ip,
                ..ClientInfo::default()
            })
            .build();
        vec![
            (
                "Axum",
                AxumApp::routes_with_settings_and_services(settings.clone(), services.clone())
                    .expect("should build Axum trace routes"),
            ),
            (
                "Cloudflare",
                CloudflareApp::routes_with_settings_and_services(
                    settings.clone(),
                    services.clone(),
                )
                .expect("should build Cloudflare trace routes"),
            ),
            (
                "Spin",
                SpinApp::routes_with_settings_and_services(settings, services)
                    .expect("should build Spin trace routes"),
            ),
        ]
    }

    #[tokio::test]
    async fn trace_dispatch_parity_missing_client_ip_skips_geo() {
        let calls = Arc::new(AtomicUsize::new(0));
        for (adapter, router) in routers_with_client(true, None, Arc::clone(&calls), None) {
            let response = RouterService::oneshot(
                &router,
                request_builder()
                    .uri("/_ts/trace")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build missing-IP setup"),
            )
            .await
            .expect("should render setup without optional facts");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{adapter} should render missing optional facts"
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "should not synthesize a geo lookup without trusted client IP"
        );
    }

    #[tokio::test]
    async fn trace_dispatch_parity_auth_precedes_flags_aliases_and_methods_without_services() {
        let calls = Arc::new(AtomicUsize::new(0));
        for enabled in [false, true] {
            for (pattern, path, expected) in [
                (Some("^/"), "/%5Fts/trace", StatusCode::UNAUTHORIZED),
                (
                    Some("^/_ts"),
                    "/%5Fts/trace",
                    if enabled {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::NOT_FOUND
                    },
                ),
                (Some("^/_ts"), "/_ts/trace/extra", StatusCode::UNAUTHORIZED),
                (
                    None,
                    "/_ts/trace",
                    if enabled {
                        StatusCode::METHOD_NOT_ALLOWED
                    } else {
                        StatusCode::NOT_FOUND
                    },
                ),
            ] {
                for (adapter, router) in routers(enabled, pattern, Arc::clone(&calls)) {
                    let request = request_builder()
                        .method("EXAMPLE-METHOD")
                        .uri(path)
                        .body(edgezero_core::body::Body::empty())
                        .expect("should build extension method");
                    let response = router
                        .oneshot(request)
                        .await
                        .expect("should reject locally");
                    assert_eq!(
                        response.status(),
                        expected,
                        "{adapter} should apply auth then flag/path/method"
                    );
                    assert!(
                        response
                            .extensions()
                            .get::<TraceTerminalResponse>()
                            .is_some(),
                        "{adapter} should retain terminal marker"
                    );
                    assert_eq!(
                        response.headers()[header::CACHE_CONTROL],
                        "no-store, private",
                        "{adapter} should keep trace errors private"
                    );
                    assert!(
                        !response.headers().contains_key(header::SET_COOKIE),
                        "{adapter} should not write rejected cookies"
                    );
                }
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "should never lookup denied metadata"
        );
    }

    #[tokio::test]
    async fn trace_dispatch_parity_read_only_setup_and_verified_assets() {
        let calls = Arc::new(AtomicUsize::new(0));
        for (adapter, router) in routers(true, None, Arc::clone(&calls)) {
            for (method, path, expected_calls) in [
                (Method::HEAD, "/_ts/trace", 0),
                (Method::GET, "/_ts/trace/state", 0),
                (Method::GET, "/_ts/trace/assets/v1.js", 0),
                (Method::GET, "/_ts/trace/assets/v1.css", 0),
                (Method::GET, "/_ts/trace", 1),
            ] {
                calls.store(0, Ordering::SeqCst);
                let head = method == Method::HEAD;
                let request = request_builder()
                    .method(method)
                    .uri(path)
                    .header("cookie", "__Host-ts-console=1")
                    .header("cf-connecting-ip", "203.0.113.22")
                    .header("spin-client-addr", "203.0.113.33:1234")
                    .body(edgezero_core::body::Body::empty())
                    .expect("should build local read");
                let response = RouterService::oneshot(&router, request)
                    .await
                    .expect("should serve local read");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{adapter} should serve local route"
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    expected_calls,
                    "{adapter} should fetch metadata only for GET setup"
                );
                assert!(
                    !response.headers().contains_key(header::SET_COOKIE),
                    "{adapter} should not refresh cookies"
                );
                let headers = response.headers().clone();
                let bytes = response
                    .into_body()
                    .into_bytes()
                    .expect("should buffer trace response");
                if head {
                    assert!(bytes.is_empty(), "{adapter} should remove HEAD body");
                } else if path == "/_ts/trace" {
                    let html = String::from_utf8(bytes.to_vec()).expect("should render UTF8 setup");
                    assert!(
                        html.contains("192.0.2.0/24"),
                        "{adapter} should prefer injected client metadata"
                    );
                    assert!(
                        !html.contains("192.0.2.99"),
                        "{adapter} should redact full client IP"
                    );
                    assert!(
                        html.contains("64512"),
                        "{adapter} should project populated ASN"
                    );
                } else if path.contains("/assets/") {
                    assert_eq!(
                        headers[header::CACHE_CONTROL],
                        "public, max-age=31536000, immutable",
                        "{adapter} should cache unprotected fixed bytes"
                    );
                    assert_eq!(
                        bytes.as_ref(),
                        trusted_server_js_asset(path),
                        "{adapter} should serve actual verified build bytes"
                    );
                } else {
                    assert_eq!(
                        bytes.as_ref(),
                        br#"{"observed_active":true}"#,
                        "{adapter} should return boolean state only"
                    );
                }
            }
        }
    }

    fn trusted_server_js_asset(path: &str) -> &'static [u8] {
        trusted_server_js::trace_assets::trace_asset(path)
            .expect("should locate verified fixed asset")
            .bytes
    }

    #[tokio::test]
    async fn trace_dispatch_parity_stream_actions_preserve_cookie_policy_and_separate_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        for (adapter, router) in routers(true, None, Arc::clone(&calls)) {
            for action in ["enable", "end"] {
                let mut request = request_builder()
                    .method("POST")
                    .uri(format!("https://publisher.example.com/_ts/trace/{action}"))
                    .header("host", "publisher.example.com")
                    .header("origin", "https://publisher.example.com")
                    .header("sec-fetch-site", "same-origin")
                    .header("x-ts-trace-action", action)
                    .header("cookie", "__Host-ts-console=1, unrelated=value")
                    .body(edgezero_core::body::Body::from_stream(
                        futures_trace_empty_stream(),
                    ))
                    .expect("should build no-content-type streamed action");
                request.extensions_mut().insert(
                    RequestIngress::new(
                        CapturedTarget::Unavailable(TargetUnavailable::NotExposed),
                        Some(
                            InboundOrigin::parse(
                                "https",
                                "publisher.example.com",
                                OriginSource::RuntimeUri,
                            )
                            .expect("should freeze trusted origin"),
                        ),
                        HeaderFidelity::default(),
                        Vec::new(),
                    )
                    .expect("should create ingress snapshot"),
                );
                let response = RouterService::oneshot(&router, request)
                    .await
                    .expect("should request local mutation");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{adapter} should accept explicit empty streamed action independently of cookie ambiguity"
                );
                let cookies: Vec<_> = response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .collect();
                assert_eq!(
                    cookies.len(),
                    1,
                    "{adapter} should emit exactly one action cookie"
                );
                let expected = if action == "enable" {
                    "__Host-ts-console=1; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=1800"
                } else {
                    "__Host-ts-console=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"
                };
                assert_eq!(
                    cookies[0], expected,
                    "{adapter} should reuse the shared cookie policy"
                );
                assert_eq!(
                    response
                        .into_body()
                        .into_bytes()
                        .expect("should buffer mutation response")
                        .as_ref(),
                    br#"{"mutation_requested":true}"#,
                    "{adapter} should report requested mutation only"
                );
                let state = RouterService::oneshot(
                    &router,
                    request_builder()
                        .uri("/_ts/trace/state")
                        .header("cookie", "__Host-ts-console=1, unrelated=value")
                        .body(edgezero_core::body::Body::empty())
                        .expect("should build separate state observation"),
                )
                .await
                .expect("should observe separate state");
                assert_eq!(
                    state
                        .into_body()
                        .into_bytes()
                        .expect("should buffer state")
                        .as_ref(),
                    br#"{"observed_active":false}"#,
                    "{adapter} should not infer accepted browser cookie mutation"
                );
            }
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "should not lookup metadata for actions or state"
        );
    }

    fn futures_trace_empty_stream() -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
        futures::stream::iter([Ok(Bytes::new()), Ok(Bytes::new())])
    }
}
