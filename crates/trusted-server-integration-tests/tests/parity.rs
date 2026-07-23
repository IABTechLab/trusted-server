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
use edgezero_adapter_axum::service::EdgeZeroAxumService;
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

            [integrations.ad_trace]
            enabled = true
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

async fn axum_document_get(uri: &str) -> (u16, HeaderMap) {
    let mut svc = EdgeZeroAxumService::new(axum_router());
    let req = AxumRequest::builder()
        .method("GET")
        .uri(uri)
        .header("sec-fetch-dest", "document")
        .body(AxumBody::empty())
        .expect("should build document GET request");
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

async fn cf_document_get(uri: &str) -> (u16, HeaderMap) {
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .header("sec-fetch-dest", "document")
        .body(edgezero_core::body::Body::empty())
        .expect("should build document GET request");
    let resp = cf_router().oneshot(req).await.expect("should respond");
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

async fn spin_document_get(uri: &str) -> (u16, HeaderMap) {
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .header("sec-fetch-dest", "document")
        .body(edgezero_core::body::Body::empty())
        .expect("should build document GET request");
    let resp = spin_router().oneshot(req).await.expect("should respond");
    (resp.status().as_u16(), resp.headers().clone())
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
async fn console_activation_finalizes_auth_short_circuits() {
    let uri = "/_ts/admin/keys/rotate?ts_console=1";
    let responses = [
        axum_document_get(uri).await,
        cf_document_get(uri).await,
        spin_document_get(uri).await,
    ];

    for (status, headers) in responses {
        assert_eq!(status, 401);
        assert_eq!(
            headers
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("private, no-store")
        );
        assert!(
            headers
                .get_all("set-cookie")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|value| value.starts_with("__Host-ts-console=1;")),
            "auth short-circuit should preserve the console session action"
        );
    }
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
    // OPTIONS /__ts/page-bids is a CORS preflight to a side-effecting endpoint.
    // Every adapter must refuse it with 403 rather than proxy it to the origin:
    // a permissive origin preflight would let a cross-site page defeat the GET
    // handler's `X-TSJS-Page-Bids` gate and trigger real auctions in a visitor's
    // browser. The denial is unconditional (independent of creative-opportunity
    // configuration), so all adapters must agree on 403.
    let (axum_status, _) = axum_options("/__ts/page-bids").await;
    let (cf_status, _) = cf_options("/__ts/page-bids").await;
    let (spin_status, _) = spin_options("/__ts/page-bids").await;

    assert_eq!(
        axum_status, 403,
        "Axum OPTIONS /__ts/page-bids must be denied with 403, got {axum_status}"
    );
    assert_eq!(
        cf_status, 403,
        "Cloudflare OPTIONS /__ts/page-bids must be denied with 403, got {cf_status}"
    );
    assert_eq!(
        spin_status, 403,
        "Spin OPTIONS /__ts/page-bids must be denied with 403, got {spin_status}"
    );
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
