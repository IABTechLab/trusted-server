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
use edgezero_core::http::request_builder;
use edgezero_core::router::RouterService;
use http::HeaderMap;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp as AxumApp;
use trusted_server_adapter_cloudflare::app::TrustedServerApp as CloudflareApp;
use trusted_server_core::settings::Settings;

/// Shared test settings for both adapters.
///
/// The settings baked into the binaries contain placeholder secrets that
/// `get_settings()` rejects by design, so both routers are built through
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

    // This endpoint serves a static config document, so the bodies must be
    // identical across adapters — not merely both parsable (or both unparsable)
    // as JSON. Parse first so JSON key ordering / whitespace differences do not
    // cause false failures, but never treat `None == None` as body parity.
    let axum_json: Option<Value> = serde_json::from_slice(&axum_body_bytes).ok();
    let cf_json: Option<Value> = serde_json::from_slice(&cf_body_bytes).ok();

    // Both adapters must agree on whether the body is JSON. A regression where
    // one returns the discovery JSON and the other returns a non-JSON error body
    // is a definitive parity break.
    assert_eq!(
        axum_json.is_some(),
        cf_json.is_some(),
        "/.well-known/trusted-server.json body type (JSON vs non-JSON) must match \
         across adapters (axum_status={axum_status} cf_status={cf_status})"
    );

    match (axum_json, cf_json) {
        // Both adapters serve JSON: compare parsed values so serialization
        // differences (key ordering, whitespace) do not cause false failures.
        (Some(axum_value), Some(cf_value)) => assert_eq!(
            axum_value, cf_value,
            "/.well-known/trusted-server.json JSON body must match across adapters \
             (axum_status={axum_status} cf_status={cf_status})"
        ),
        // Without seeded signing/JWKS data both adapters take the same error path
        // and return a non-JSON error body. Compare raw bytes so diverging error
        // payloads are caught instead of both parsing to `None`.
        _ => assert_eq!(
            axum_body_bytes, cf_body_bytes,
            "/.well-known/trusted-server.json non-JSON body must match across adapters \
             (axum_status={axum_status} cf_status={cf_status})"
        ),
    }
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
    // Both adapters must return 401 for unauthenticated admin requests on the
    // canonical `/_ts/admin/keys/*` path that production config auth-gates.
    // The authenticated-path divergence (Axum→501 no-KV, CF→4xx no-KV)
    // is separate and not covered here.
    let (axum_status, axum_headers) = axum_post_headers("/_ts/admin/keys/rotate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/_ts/admin/keys/rotate", "{}").await;

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
        "WWW-Authenticate values must match across adapters for /_ts/admin/keys/rotate"
    );
    assert!(
        axum_www_auth.starts_with("Basic realm="),
        "WWW-Authenticate must be Basic scheme: {axum_www_auth:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_deactivate_unauthenticated_parity() {
    // Mirror of admin_rotate_unauthenticated_parity for the deactivate endpoint.
    let (axum_status, axum_headers) = axum_post_headers("/_ts/admin/keys/deactivate", "{}").await;
    let (cf_status, cf_headers) = cf_post_headers("/_ts/admin/keys/deactivate", "{}").await;

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
        "WWW-Authenticate values must match across adapters for /_ts/admin/keys/deactivate"
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
    assert_ne!(axum_status, 404, "Axum /auction must be routed (not 404)");
    assert_ne!(cf_status, 404, "Cloudflare /auction must be routed (not 404)");
    assert_eq!(
        axum_status, cf_status,
        "/auction must return the same status across adapters: \
         axum={axum_status} cf={cf_status}"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cf_legacy_admin_aliases_are_not_unauthenticated_admin_routes() {
    // The production handler regex `^/_ts/admin` only matches the canonical
    // `/_ts/admin/keys/*` paths, so the legacy `/admin/keys/*` aliases must not be
    // registered as admin routes on Cloudflare. If they were, AuthMiddleware would
    // not match them and unauthenticated callers would reach the real key
    // handlers. With the aliases removed, each behaves like any other unrouted
    // path: it falls through to the publisher fallback rather than the auth-gated
    // admin route.
    //
    // Primary guard: assert the route table directly, analogous to the Fastly
    // `NAMED_ROUTES` guard. A behavioral check alone can false-pass, because a
    // reintroduced key handler could fail with the same fallback status and carry
    // no `WWW-Authenticate` header in this no-store/no-origin environment.
    let registered: Vec<(String, String)> = cf_router()
        .routes()
        .iter()
        .map(|route| (route.method().to_string(), route.path().to_string()))
        .collect();
    let is_registered =
        |method: &str, path: &str| registered.iter().any(|(m, p)| m == method && p == path);

    assert!(
        is_registered("POST", "/_ts/admin/keys/rotate"),
        "canonical POST /_ts/admin/keys/rotate must be a registered admin route"
    );
    assert!(
        is_registered("POST", "/_ts/admin/keys/deactivate"),
        "canonical POST /_ts/admin/keys/deactivate must be a registered admin route"
    );
    assert!(
        !is_registered("POST", "/admin/keys/rotate"),
        "legacy POST /admin/keys/rotate must not be registered (would bypass `^/_ts/admin` auth)"
    );
    assert!(
        !is_registered("POST", "/admin/keys/deactivate"),
        "legacy POST /admin/keys/deactivate must not be registered (would bypass `^/_ts/admin` auth)"
    );

    // Secondary guard: confirm the runtime behavior matches an unrouted path.
    for alias in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        let (canonical_status, _) = cf_post_headers(&format!("/_ts{alias}"), "{}").await;
        assert_eq!(
            canonical_status, 401,
            "canonical /_ts{alias} must challenge unauthenticated callers"
        );

        let (alias_status, alias_headers) = cf_post_headers(alias, "{}").await;
        assert_ne!(
            alias_status, 401,
            "legacy {alias} must not be an auth-gated admin route"
        );
        assert!(
            !alias_headers.contains_key("www-authenticate"),
            "legacy {alias} must not issue an admin auth challenge"
        );

        let (unknown_status, _) = cf_post_headers("/this-route-does-not-exist-abc123", "{}").await;
        assert_eq!(
            alias_status, unknown_status,
            "legacy {alias} must fall through to the publisher fallback like an unknown path: \
             alias={alias_status} unknown={unknown_status}"
        );
    }
}
