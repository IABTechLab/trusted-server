//! Cross-adapter parity tests: Axum vs Cloudflare vs Spin in-process.
//!
//! Sends identical requests to all host-runnable adapters and asserts that:
//! - Response status codes match
//! - No adapter writes an `x-geo-*` header on its own routes
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
/// Each adapter reads its settings from its config store when it starts, so
/// all routers are built through their `routes_with_settings` testing seams
/// from this known-good config, which carries the repository's sample policy
/// as a loaded domain carries its own.
fn test_settings() -> Settings {
    Settings::from_toml(
        r#"
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "parity-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true
        "#,
    )
    .expect("should parse parity test settings")
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
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

/// The configuration view is built by core from the settings alone, so
/// every adapter shows the same one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_endpoint_parity() {
    use http_body_util::BodyExt as _;
    use serde_json::Value;

    const PATH: &str = "/_ts/config.json";

    let (axum_status, axum_body) = {
        let mut svc = EdgeZeroAxumService::new(axum_router());
        let req = AxumRequest::builder()
            .method("GET")
            .uri(PATH)
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
    let (cf_status, cf_body) = {
        let req = request_builder()
            .method("GET")
            .uri(PATH)
            .body(edgezero_core::body::Body::empty())
            .expect("should build GET request");
        let resp = cf_router().oneshot(req).await.expect("should respond");
        let status = resp.status().as_u16();
        (status, resp.into_body().into_bytes().unwrap_or_default())
    };
    let (spin_status, _, spin_body) = spin_get_body(PATH).await;

    assert_eq!(axum_status, 200, "Axum should answer {PATH}");
    assert_eq!(cf_status, 200, "Cloudflare should answer {PATH}");
    assert_eq!(spin_status, 200, "Spin should answer {PATH}");

    let axum: Value = serde_json::from_slice(&axum_body).expect("Axum should answer JSON");
    let cf: Value = serde_json::from_slice(&cf_body).expect("Cloudflare should answer JSON");
    let spin: Value = serde_json::from_slice(&spin_body).expect("Spin should answer JSON");
    assert_eq!(axum, cf, "{PATH} must match across adapters (axum, cf)");
    assert_eq!(cf, spin, "{PATH} must match across adapters (cf, spin)");
    assert_eq!(
        axum["settings"]["publisher"]["proxy_secret"], "XXXX",
        "a secret should be masked"
    );
    assert!(
        !String::from_utf8_lossy(&axum_body).contains("parity-test-proxy-secret"),
        "the view should not carry the secret anywhere"
    );
}

/// Whether a request is a browser opening the page is core's decision, and
/// none of these three adapters keeps an identity graph, so each refuses and
/// answers alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_endpoint_parity() {
    use http_body_util::BodyExt as _;

    const PATH: &str = "/_ts/data";
    const OPENED_AS_A_PAGE: [(&str, &str); 2] = [
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-dest", "document"),
    ];

    async fn edge(
        router: RouterService,
        headers: &[(&str, &str)],
    ) -> (u16, Option<http::HeaderValue>, bytes::Bytes) {
        let mut builder = request_builder().method("GET").uri(PATH);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build GET request");
        let resp = router.oneshot(req).await.expect("should respond");
        (
            resp.status().as_u16(),
            resp.headers().get("content-security-policy").cloned(),
            resp.into_body().into_bytes().unwrap_or_default(),
        )
    }

    for (headers, expected) in [(&[][..], 403), (&OPENED_AS_A_PAGE[..], 200)] {
        let axum = {
            let mut svc = EdgeZeroAxumService::new(axum_router());
            let mut builder = AxumRequest::builder().method("GET").uri(PATH);
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            let req = builder
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
            let policy = resp.headers().get("content-security-policy").cloned();
            let body = resp
                .into_body()
                .collect()
                .await
                .expect("should collect body")
                .to_bytes();
            (status, policy, body)
        };
        let cf = edge(cf_router(), headers).await;
        let spin = edge(spin_router(), headers).await;

        assert_eq!(
            axum.0, expected,
            "Axum should answer {PATH} with {expected}"
        );
        assert_eq!(axum, cf, "{PATH} must match across adapters (axum, cf)");
        assert_eq!(cf, spin, "{PATH} must match across adapters (cf, spin)");
        assert!(
            axum.1
                .as_ref()
                .and_then(|policy| policy.to_str().ok())
                .is_some_and(|policy| policy.starts_with("sandbox;")),
            "every answer should be in an origin of its own"
        );
    }
}

/// The permissions a request resolves to are core's decision, so the
/// endpoint that shows them answers the same on every adapter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_endpoint_parity() {
    use http_body_util::BodyExt as _;
    use serde_json::Value;

    const PATH: &str = "/_ts/permissions.json";

    let (axum_status, axum_body) = {
        let mut svc = EdgeZeroAxumService::new(axum_router());
        let req = AxumRequest::builder()
            .method("GET")
            .uri(PATH)
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
    let (cf_status, cf_body) = {
        let req = request_builder()
            .method("GET")
            .uri(PATH)
            .body(edgezero_core::body::Body::empty())
            .expect("should build GET request");
        let resp = cf_router().oneshot(req).await.expect("should respond");
        let status = resp.status().as_u16();
        (status, resp.into_body().into_bytes().unwrap_or_default())
    };
    let (spin_status, _, spin_body) = spin_get_body(PATH).await;

    assert_eq!(axum_status, 200, "Axum should answer {PATH}");
    assert_eq!(cf_status, 200, "Cloudflare should answer {PATH}");
    assert_eq!(spin_status, 200, "Spin should answer {PATH}");

    let axum: Value = serde_json::from_slice(&axum_body).expect("Axum should answer JSON");
    let cf: Value = serde_json::from_slice(&cf_body).expect("Cloudflare should answer JSON");
    let spin: Value = serde_json::from_slice(&spin_body).expect("Spin should answer JSON");
    assert_eq!(axum, cf, "{PATH} must match across adapters (axum, cf)");
    assert_eq!(cf, spin, "{PATH} must match across adapters (cf, spin)");
    assert!(
        axum["set"].is_array() && axum["awaiting"].is_array(),
        "the resolution should say what is set and what is awaited"
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
async fn no_adapter_writes_a_geo_header_on_its_own_routes() {
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

        // The x-geo-* headers are written by a serve middleware on the pages
        // an entry names it for, and never on a route of the server's own.
        for (adapter, status, headers) in [
            ("Axum", axum_status, &axum_headers),
            ("Cloudflare", cf_status, &cf_headers),
            ("Spin", spin_status, &spin_headers),
        ] {
            let geo_headers: Vec<&str> = headers
                .keys()
                .map(http::HeaderName::as_str)
                .filter(|name| name.starts_with("x-geo-"))
                .collect();
            assert!(
                geo_headers.is_empty(),
                "{adapter}: {method} {path} (status={status}) must carry no x-geo-* header: \
                 {geo_headers:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_is_routed_parity() {
    let (axum_status, _) = axum_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (cf_status, _) = cf_post_headers("/auction", r#"{"adUnits":[]}"#).await;
    let (spin_status, _) = spin_post_headers("/auction", r#"{"adUnits":[]}"#).await;

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
async fn key_administration_paths_are_answered_here_and_never_proxied() {
    // A request to a key administration path can carry an operator's
    // credentials and a key payload. Every adapter must answer it with 404 and
    // never reach the publisher fallback, which forwards the request,
    // `Authorization` header and body included, to the origin. A key handler
    // at one of these paths would not return 404, and the publisher fallback
    // would not either, so 404 proves the request was answered here.
    for path in [
        "/_ts/admin/keys/rotate",
        "/_ts/admin/keys/deactivate",
        "/admin/keys/rotate",
        "/admin/keys/deactivate",
    ] {
        for method in ["POST", "GET"] {
            let (axum_status, axum_headers) =
                axum_authorized_json(method, path, r#"{"key_id":"leak-me"}"#).await;
            let (cf_status, cf_headers) =
                cf_authorized_json(method, path, r#"{"key_id":"leak-me"}"#).await;
            let (spin_status, spin_headers) =
                spin_authorized_json(method, path, r#"{"key_id":"leak-me"}"#).await;
            for (adapter, status, headers) in [
                ("Axum", axum_status, &axum_headers),
                ("Cloudflare", cf_status, &cf_headers),
                ("Spin", spin_status, &spin_headers),
            ] {
                assert_eq!(
                    status, 404,
                    "{adapter} {method} {path} must be answered here with 404"
                );
                assert!(
                    !headers.contains_key("www-authenticate"),
                    "{adapter} {method} {path} must not ask for credentials"
                );
            }
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
