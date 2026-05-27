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
    let router = CloudflareApp::routes();
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
    let router = CloudflareApp::routes();
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

fn header_value<'a>(headers: &'a HeaderMap, name: &str, adapter: &str) -> &'a str {
    headers
        .get(name)
        .unwrap_or_else(|| panic!("{adapter} response must include {name}"))
        .to_str()
        .unwrap_or_else(|_| panic!("{adapter} {name} header must be valid UTF-8"))
}

// ---------------------------------------------------------------------------
// Route parity: same route → same status on both adapters
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_route_status_parity() {
    let (axum_status, _) = axum_get("/.well-known/trusted-server.json").await;
    let (cf_status, _) = cf_get("/.well-known/trusted-server.json").await;
    assert_eq!(
        axum_status, cf_status,
        "/.well-known/trusted-server.json must return same status: axum={axum_status} cf={cf_status}"
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
        let mut svc = EdgeZeroAxumService::new(AxumApp::routes());
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
        let router = CloudflareApp::routes();
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

    // Both adapters must agree on whether the response is JSON.
    let axum_json: Option<Value> = serde_json::from_slice(&axum_body_bytes).ok();
    let cf_json: Option<Value> = serde_json::from_slice(&cf_body_bytes).ok();
    assert_eq!(
        axum_json.is_some(),
        cf_json.is_some(),
        "/.well-known/trusted-server.json body JSON-parsability must match across adapters \
         (axum_status={axum_status} cf_status={cf_status})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_route_parity() {
    // known divergence: without real signing-key configuration the handler may
    // return 5xx. The parity assertion is that both adapters agree on the status
    // (routing and middleware are wired identically).
    let (axum_status, _) = axum_post_headers("/verify-signature", "{}").await;
    let (cf_status, _) = cf_post_headers("/verify-signature", "{}").await;

    assert_ne!(axum_status, 404, "Axum /verify-signature must be routed");
    assert_ne!(
        cf_status, 404,
        "Cloudflare /verify-signature must be routed"
    );
    assert_eq!(
        axum_status, cf_status,
        "/verify-signature must return same status: axum={axum_status} cf={cf_status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_unauthenticated_parity() {
    // Both adapters must return 401 for unauthenticated admin requests.
    // The authenticated-path divergence (Axum→501 no-KV, CF→4xx no-KV)
    // is separate and not covered here.
    let (axum_status, axum_headers) = axum_post_headers("/admin/keys/rotate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/admin/keys/rotate", "{}").await;

    assert_eq!(
        axum_status, 401,
        "Axum must return 401 for unauthenticated admin route"
    );
    assert_eq!(
        cf_status, 401,
        "Cloudflare must return 401 for unauthenticated admin route"
    );
    assert_eq!(
        axum_status, cf_status,
        "both adapters must return the same status for unauthenticated admin route"
    );

    let axum_www_auth = header_value(&axum_headers, "www-authenticate", "Axum");
    let cf_www_auth = header_value(&cf_headers, "www-authenticate", "Cloudflare");
    assert_eq!(
        axum_www_auth, cf_www_auth,
        "WWW-Authenticate values must match across adapters"
    );
    assert!(
        axum_www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme: {axum_www_auth:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_unauthenticated_parity() {
    // Mirror of admin_rotate_unauthenticated_parity for the deactivate endpoint.
    let (axum_status, axum_headers) = axum_post_headers("/admin/keys/deactivate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/admin/keys/deactivate", "{}").await;

    assert_eq!(
        axum_status, 401,
        "Axum must return 401 for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        cf_status, 401,
        "Cloudflare must return 401 for unauthenticated admin/keys/deactivate"
    );
    assert_eq!(
        axum_status, cf_status,
        "both adapters must return the same status for unauthenticated admin/keys/deactivate"
    );

    let axum_www_auth = header_value(&axum_headers, "www-authenticate", "Axum");
    let cf_www_auth = header_value(&cf_headers, "www-authenticate", "Cloudflare");
    assert_eq!(
        axum_www_auth, cf_www_auth,
        "WWW-Authenticate values must match across adapters"
    );
    assert!(
        axum_www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme: {axum_www_auth:?}"
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

        let axum_geo = header_value(&axum_headers, "x-geo-info-available", "Axum");
        let cf_geo = header_value(&cf_headers, "x-geo-info-available", "Cloudflare");
        assert_eq!(
            axum_geo, cf_geo,
            "X-Geo-Info-Available values must match for {method} {path} \
             (axum_status={axum_status} cf_status={cf_status})"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_not_challenged_by_auth_parity() {
    let (axum_status, _) = axum_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (cf_status, _) = cf_post_headers("/auction", r#"{"adUnits":[]}"#).await;

    assert_ne!(axum_status, 401, "Axum /auction must not 401");
    assert_ne!(cf_status, 401, "Cloudflare /auction must not 401");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_proxy_fallback_parity() {
    // Cookie (Set-Cookie) parity for the publisher proxy requires a live origin.
    // Without an origin, both adapters return an error (4xx or 5xx). The parity
    // assertion is that Set-Cookie presence matches across adapters regardless of
    // whether the proxy succeeds.
    let (axum_status, axum_headers) = axum_get("/").await;
    let (cf_status, cf_headers) = cf_get("/").await;

    // Both adapters must agree: either both proxy to the origin or both fail.
    assert_eq!(
        axum_status >= 500,
        cf_status >= 500,
        "publisher fallback 5xx behaviour must match: axum={axum_status} cf={cf_status}"
    );

    let axum_has_cookie = axum_headers.contains_key("set-cookie");
    let cf_has_cookie = cf_headers.contains_key("set-cookie");
    assert_eq!(
        axum_has_cookie, cf_has_cookie,
        "Set-Cookie presence must match: axum={axum_has_cookie} cf={cf_has_cookie}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_route_returns_same_status_parity() {
    let (axum_status, _) = axum_get("/this-route-does-not-exist-abc123").await;
    let (cf_status, _) = cf_get("/this-route-does-not-exist-abc123").await;

    assert_eq!(
        axum_status, cf_status,
        "unknown routes must return same status: axum={axum_status} cf={cf_status}"
    );
}
