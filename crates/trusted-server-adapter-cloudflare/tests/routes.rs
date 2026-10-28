//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! the platform layer or outbound network calls.

use edgezero_core::app::Hooks as _;
use edgezero_core::http::{Request, Response, request_builder};
use edgezero_core::router::RouterService;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_core::settings::Settings;

const LEGACY_ADMIN_DENY_METHODS: &[&str] =
    &["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// Build the full application router from explicit test settings.
///
/// A Worker reads its settings from its config store when it builds its
/// application, and `TrustedServerApp::routes()`, which cannot read the store,
/// serves the startup error router on every route (and its route table is the
/// fallback-only set).
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
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true
        "#,
    )
    .expect("should parse route test settings")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());

    TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings")
}

/// Return the set of (METHOD, path) pairs explicitly registered on the router.
fn registered_routes() -> Vec<(String, String)> {
    test_router()
        .routes()
        .into_iter()
        .map(|r| (r.method().to_string(), r.path().to_string()))
        .collect()
}

async fn route(router: RouterService, req: Request) -> Response {
    router.oneshot(req).await.expect("should route request")
}

fn assert_route_registered(method: &str, path: &str) {
    let routes = registered_routes();
    assert!(
        routes.iter().any(|(m, p)| m == method && p == path),
        "{method} {path} must be explicitly registered; registered routes: {routes:?}"
    );
}

/// Build a router from explicit test settings so routes resolve to their real
/// handlers instead of the `startup_error_router` fallback, which
/// `TrustedServerApp::routes()` serves because it cannot read the config
/// store.
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

            [geo]
            assume_single_jurisdiction = true

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"
        "#,
    )
    .expect("should parse route test settings")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample());

    TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings")
}

#[test]
fn routes_build_without_panic() {
    // routes() cannot read the config store, so it serves the startup error
    // router, and building it must not panic.
    let _router = TrustedServerApp::routes();
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

    let resp = route(router, req).await;

    assert!(
        resp.headers().contains_key("x-geo-info-available"),
        "FinalizeResponseMiddleware must inject X-Geo-Info-Available on every response"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_middleware_runs_in_chain_for_protected_routes() {
    // Verifies that AuthMiddleware is wired by asserting the 401 + WWW-Authenticate
    // challenge on a protected route (/_ts/admin/keys/rotate). Only AuthMiddleware
    // short-circuits with this response — FinalizeResponseMiddleware alone would not.
    let router = test_router();

    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.status().as_u16(),
        401,
        "AuthMiddleware must short-circuit with 401 on protected routes without credentials"
    );
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "AuthMiddleware must include WWW-Authenticate on 401 responses"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_admin_aliases_denied_locally_not_proxied_to_publisher() {
    // Regression for the credential-leak finding: the production basic-auth regex
    // `^/_ts/admin` does not match `/admin/keys/*`, so those aliases are not
    // auth-gated. Any publisher-fallback method carrying an `Authorization`
    // header must be denied locally with 404, never proxied to the publisher
    // origin (which would leak the admin credentials and key body). A
    // publisher-fallback proxy without a backend would surface as a 5xx, so 404
    // proves the local deny ran.
    for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        for method in LEGACY_ADMIN_DENY_METHODS {
            let router = test_router();
            let req = request_builder()
                .method(*method)
                .uri(path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .header("content-type", "application/json")
                .body(edgezero_core::body::Body::from("{\"key_id\":\"leak-me\"}"))
                .expect("should build authorized legacy-alias request");

            let resp = route(router, req).await;

            assert_eq!(
                resp.status().as_u16(),
                404,
                "legacy {method} {path} with Authorization must be denied locally (404), not proxied to publisher"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify all adapter routes are registered and do not 5xx
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_is_routed_not_5xx() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    let status = resp.status().as_u16();
    // The tsjs route is matched by the /{*rest} catch-all. The handler returns 404
    // for an unknown hash — that is correct application behaviour, not a routing miss.
    assert!(status < 500, "tsjs route must not 5xx: got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_emits_cloudflare_cache_header_for_matching_hash() {
    let router = test_router();
    let src = trusted_server_core::tsjs::tsjs_script_src(
        &trusted_server_core::tsjs_bundle::compile_time_parts(&["creative"]),
    );
    let req = request_builder()
        .method("GET")
        .uri(src)
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.status().as_u16(),
        200,
        "matching TSJS hash should serve OK"
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=31536000, immutable"),
        "browser cache policy should be immutable for matching TSJS hash"
    );
    assert_eq!(
        resp.headers()
            .get("cloudflare-cdn-cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("max-age=31536000"),
        "Cloudflare adapter should emit the Cloudflare-specific edge header"
    );
    assert!(
        resp.headers().get("surrogate-control").is_none(),
        "Cloudflare adapter must not emit Fastly Surrogate-Control"
    );
}

/// Verify that every expected explicit route is registered in the route table.
///
/// Uses [`RouterService::routes()`] for introspection rather than checking
/// response status codes — wildcards (`/{*rest}`) can return non-404 even when
/// an explicit registration is missing, making status-based checks false positives.
#[test]
fn all_explicit_routes_are_registered() {
    let expected: &[(&str, &str)] = &[
        ("GET", "/.well-known/trusted-server.json"),
        ("POST", "/verify-signature"),
        ("GET", "/_ts/permissions"),
        ("GET", "/_ts/permissions.json"),
        ("GET", "/_ts/config"),
        ("GET", "/_ts/config.json"),
        ("GET", "/_ts/data"),
        ("POST", "/_ts/admin/keys/rotate"),
        ("POST", "/_ts/admin/keys/deactivate"),
        ("GET", "/_ts/admin/ec"),
        ("GET", "/_ts/admin/ec/{id}"),
        ("GET", "/_ts/admin/eids"),
        ("POST", "/auction"),
        // SPA re-auction endpoint, plus its deprecated `/__ts/` alias. Both
        // paths are spelled out as literals rather than referencing
        // `PAGE_BIDS_PATH` / `PAGE_BIDS_LEGACY_PATH` so this test pins the
        // actual URL the tsjs client fetches — asserting a const against itself
        // would still pass if the const's value changed out from under the
        // client.
        ("GET", "/_ts/page-bids"),
        ("OPTIONS", "/_ts/page-bids"),
        ("GET", "/__ts/page-bids"),
        ("OPTIONS", "/__ts/page-bids"),
        ("GET", "/first-party/proxy"),
        ("GET", "/first-party/click"),
        ("GET", "/first-party/sign"),
        ("POST", "/first-party/sign"),
        ("GET", "/first-party/proxy-rebuild"),
        ("POST", "/first-party/proxy-rebuild"),
    ];

    for (method, path) in expected {
        assert_route_registered(method, path);
    }

    for path in ["/admin/keys/rotate", "/admin/keys/deactivate"] {
        for method in LEGACY_ADMIN_DENY_METHODS {
            assert_route_registered(method, path);
        }
    }
}

// ---------------------------------------------------------------------------
// Basic-auth parity tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_routes_return_501() {
    for (path, body) in [
        ("/_ts/admin/keys/rotate", "{}"),
        (
            "/_ts/admin/keys/deactivate",
            r#"{"kid":"test-key","delete":false}"#,
        ),
    ] {
        let req = request_builder()
            .method("POST")
            .uri(path)
            .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
            .header("content-type", "application/json")
            .body(edgezero_core::body::Body::from(body))
            .expect("should build request");
        let resp = route(test_router(), req).await;

        assert_eq!(
            resp.status().as_u16(),
            501,
            "{path} should report that Cloudflare key management is unsupported"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_ec_routes_return_501() {
    // The EC identity graph is Fastly KV backed, so Cloudflare answers the
    // admin EC lookup routes locally with 501 instead of letting them fall
    // through to the publisher fallback.
    let sample_ec_id = format!("{}.abc123", "a".repeat(64));
    for path in [
        "/_ts/admin/ec".to_owned(),
        format!("/_ts/admin/ec/{sample_ec_id}"),
    ] {
        let req = request_builder()
            .method("GET")
            .uri(&path)
            .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = route(test_router(), req).await;

        assert_eq!(
            resp.status().as_u16(),
            501,
            "{path} should report that Cloudflare EC lookup is unsupported"
        );
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_ec_route_without_credentials_returns_401() {
    let req = request_builder()
        .method("GET")
        .uri("/_ts/admin/ec")
        .body(edgezero_core::body::Body::empty())
        .expect("should build unauthenticated admin EC request");
    let resp = route(test_router(), req).await;

    assert_eq!(resp.status().as_u16(), 401);
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "admin EC 401 should include the Basic authentication challenge"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_eids_route_returns_200() {
    // The EIDs echo is pure request inspection (no KV), so this adapter
    // serves the real handler.
    let req = request_builder()
        .method("GET")
        .uri("/_ts/admin/eids")
        .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(test_router(), req).await;

    assert_eq!(
        resp.status().as_u16(),
        200,
        "/_ts/admin/eids should serve the real EIDs echo handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_admin_diagnostic_fallback_is_denied_locally() {
    let ec_id = format!("{}.abc123", "a".repeat(64));
    let valid_paths = [
        "/_ts/admin/ec".to_owned(),
        format!("/_ts/admin/ec/{ec_id}"),
        "/_ts/admin/eids".to_owned(),
    ];

    for path in valid_paths {
        for method in ["POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"] {
            let request = request_builder()
                .method(method)
                .uri(&path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .body(edgezero_core::body::Body::from("sensitive-admin-body"))
                .expect("should build authenticated admin request");
            let response = route(test_router(), request).await;

            assert_eq!(response.status().as_u16(), 405);
            assert_eq!(
                response
                    .headers()
                    .get("allow")
                    .and_then(|v| v.to_str().ok()),
                Some("GET")
            );
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("no-store")
            );
        }
    }

    for path in [
        "/_ts/admin/ec/".to_owned(),
        format!("/_ts/admin/ec/{ec_id}/extra"),
        "/_ts/admin/eids/".to_owned(),
        "/_ts/admin/eids/extra".to_owned(),
        "/_ts/admin/eids.json".to_owned(),
        "/_ts/admin/ec;foo".to_owned(),
        format!("/_ts/admin/ec%2F{ec_id}"),
        // Percent-encoded separators match the `^/_ts/admin` basic-auth
        // handler but not a literal-slash namespace check, so they must be
        // reserved before publisher fallback forwards credentials upstream.
        "/_ts/admin%2Fec".to_owned(),
        "/_ts/admin%2fec".to_owned(),
        // Retired non-`/_ts` alias namespace: only the two exact paths are
        // routed to a local deny, so descendants and encoded separators must
        // be reserved at the shared fallback boundary.
        "/admin/keys".to_owned(),
        "/admin/keys/rotate/extra".to_owned(),
        "/admin/keys%2Frotate".to_owned(),
        "/admin%2fkeys/rotate".to_owned(),
        // Multi-encoded separators survive a single decode, so the reservation
        // decodes to a fixed point before the publisher fallback runs.
        "/admin%252Fkeys/rotate".to_owned(),
        "/_ts%252Fadmin/ec".to_owned(),
    ] {
        for method in ["GET", "POST"] {
            let request = request_builder()
                .method(method)
                .uri(&path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .body(edgezero_core::body::Body::from("sensitive-admin-body"))
                .expect("should build malformed admin request");
            let response = route(test_router(), request).await;

            assert_eq!(response.status().as_u16(), 404);
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("no-store")
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_route_without_credentials_returns_401() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = route(router, req).await;
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
    let resp = route(router, req).await;
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
    let resp = route(router, req).await;
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
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/.well-known/trusted-server.json must not require auth"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_endpoint_answers_anyone_as_data_and_as_a_page() {
    for (path, content_type) in [
        ("/_ts/permissions.json", "application/json"),
        ("/_ts/permissions", "text/html; charset=utf-8"),
    ] {
        let req = request_builder()
            .method("GET")
            .uri(path)
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = route(test_router(), req).await;
        assert_eq!(
            resp.status().as_u16(),
            200,
            "{path} should answer with no credential"
        );
        assert_eq!(
            resp.headers()["content-type"],
            content_type,
            "{path} should answer in its own form"
        );
        assert_eq!(
            resp.headers()["cache-control"],
            "no-store",
            "{path} is one request's own answer"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_endpoint_answers_anyone_and_masks_secrets() {
    for (path, content_type) in [
        ("/_ts/config.json", "application/json"),
        ("/_ts/config", "text/html; charset=utf-8"),
    ] {
        let req = request_builder()
            .method("GET")
            .uri(path)
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = route(test_router(), req).await;
        assert_eq!(
            resp.status().as_u16(),
            200,
            "{path} should answer with no credential"
        );
        assert_eq!(
            resp.headers()["content-type"],
            content_type,
            "{path} should answer in its own form"
        );
        let body = resp.into_body().into_bytes().unwrap_or_default();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("XXXX"),
            "{path} should mask what it does not show"
        );
        assert!(
            !text.contains("admin-pass"),
            "{path} should not carry the admin password"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_endpoint_answers_a_browser_opening_the_page_and_nothing_else() {
    for (opened_as_a_page, expected) in [(false, 403), (true, 200)] {
        let mut builder = request_builder().method("GET").uri("/_ts/data");
        if opened_as_a_page {
            builder = builder
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document");
        }
        let req = builder
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = route(test_router(), req).await;
        assert_eq!(
            resp.status().as_u16(),
            expected,
            "opened as a page: {opened_as_a_page}"
        );
        assert_eq!(
            resp.headers()["cache-control"],
            "no-store, private",
            "should never be stored"
        );
        let body = resp.into_body().into_bytes().unwrap_or_default();
        assert_eq!(
            String::from_utf8_lossy(&body).contains("keeps no record"),
            opened_as_a_page,
            "should say nothing is held to a browser opening the page, and to nothing else"
        );
    }
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
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        401,
        "/auction must not apply admin basic-auth gate"
    );
}

// ---------------------------------------------------------------------------
// Admin key route full path coverage
// ---------------------------------------------------------------------------

// Exercises the auth-fail path with a realistic key body (complements the
// generic `admin_route_without_credentials_returns_401` above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rotate_key_auth_fail_returns_401() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/_ts/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"keyId":"test-key"}"#))
        .expect("should build request");
    let resp = route(router, req).await;
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
    let resp = route(router, req).await;
    assert_eq!(
        resp.status().as_u16(),
        401,
        "admin/keys/deactivate without credentials must return 401"
    );
}

#[tokio::test]
async fn legacy_admin_rotate_alias_returns_404() {
    // The legacy non-`/_ts` alias is denied locally rather than routed to the
    // admin handler or publisher fallback.
    let router = make_router();

    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/rotate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.status().as_u16(),
        404,
        "legacy admin key rotation alias must return local 404"
    );
}

#[tokio::test]
async fn legacy_admin_deactivate_alias_returns_404() {
    let router = make_router();

    let req = request_builder()
        .method("POST")
        .uri("/admin/keys/deactivate")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.status().as_u16(),
        404,
        "legacy admin key deactivation alias must return local 404"
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

    let resp = route(router, req).await;
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
}

// ---------------------------------------------------------------------------
// Edge Cookie module availability
// ---------------------------------------------------------------------------

/// Test settings selecting a vendor Edge Cookie module this adapter does not
/// inject, with the `[ec.acme]` block that module's settings live in.
/// `acme` is a fictional vendor key.
const UNINJECTED_MODULE_TOML: &str = r#"
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
    module = "acme"

    [ec.acme]
    endpoint = "https://ec.acme.example.com"

    # An Edge Cookie module is configured, so single-jurisdiction operation
    # is acknowledged because no geo module is selected.
    [geo]
    assume_single_jurisdiction = true
"#;

/// A module selection this adapter can never supply must fail while the
/// application state is built, before any request is served.
///
/// Configuration validation accepts this selection, because only the adapter
/// that injects a module knows what that module needs, and this adapter
/// injects no vendor Edge Cookie module, so only the composition root can
/// catch it. Without the startup check the deployment would come up and answer
/// every request.
#[test]
fn selecting_a_module_this_adapter_cannot_supply_fails_at_startup() {
    let settings = Settings::from_toml(UNINJECTED_MODULE_TOML)
        .expect("should parse settings selecting an uninjected module");

    // `RouterService` is not `Debug`, so take the error side directly rather
    // than through `expect_err`.
    let error = TrustedServerApp::routes_with_settings(settings)
        .err()
        .expect("building state with an uninjected module should fail");

    assert!(
        error.to_string().contains("acme"),
        "the startup error should name the selected module, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// A module's own endpoint, outside `/integrations/`
// ---------------------------------------------------------------------------

/// The seam probe's route at a path of its own, reached through this
/// adapter's router, and the two things a module route can meet there, being
/// another route on its method and path and a route of the adapter's own.
mod module_endpoints {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use edgezero_core::body::Body;
    use edgezero_core::http::{Method, Request, Response, request_builder};
    use edgezero_core::router::RouterService;
    use error_stack::Report;
    use trusted_server_adapter_cloudflare::app::{
        TrustedServerApp, build_state_with_registrations,
    };
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        IntegrationBuilder, IntegrationBuilderFn, IntegrationEndpoint, IntegrationProxy,
        IntegrationRegistration,
    };
    use trusted_server_core::module_context::ModuleCall;
    use trusted_server_core::settings::Settings;
    use trusted_server_integration_seam_probe as seam_probe;

    /// The inspection page's path, which this adapter answers for `GET`.
    const CONFIG_PAGE_PATH: &str = "/_ts/config";

    /// Integration id of the test's own module.
    const OWN_MODULE_ID: &str = "own_module";

    /// The name a section selects the test's own module by.
    const OWN_MODULE_NAME: &str = "testing.own-module";

    /// Body the test's own module answers every request with.
    const OWN_MODULE_BODY: &str = "answered by the test's own module";

    /// The probe's own section and table.
    const PROBE_BLOCK: &str = r#"
        [testing]
        modules = ["seam-probe"]

        [testing.seam-probe]
        country = "ZZ"
    "#;

    /// How many requests the test's own module answered.
    static OWN_MODULE_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// The test's own module, which answers each route it declares with
    /// [`OWN_MODULE_BODY`] and counts the runs.
    struct OwnModule(fn() -> Vec<IntegrationEndpoint>);

    #[async_trait::async_trait(?Send)]
    impl IntegrationProxy for OwnModule {
        fn integration_name(&self) -> &'static str {
            OWN_MODULE_ID
        }

        fn routes(&self) -> Vec<IntegrationEndpoint> {
            (self.0)()
        }

        async fn handle(
            &self,
            _call: ModuleCall<'_>,
            _req: Request,
        ) -> Result<Response, Report<TrustedServerError>> {
            OWN_MODULE_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(Response::new(Body::from(OWN_MODULE_BODY)))
        }
    }

    /// The registration of the test's own module, declaring `routes`.
    fn declaring(
        routes: fn() -> Vec<IntegrationEndpoint>,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(Some(
            IntegrationRegistration::builder(OWN_MODULE_ID)
                .with_proxy(Arc::new(OwnModule(routes)))
                .build(),
        ))
    }

    /// The test's own module declaring `GET` on the probe's own path.
    fn declaring_the_probe_s_path(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        declaring(|| vec![IntegrationEndpoint::get(seam_probe::SEAM_PROBE_OWN_PATH)])
    }

    /// The test's own module declaring `GET` and `POST` on the inspection
    /// page's path.
    fn declaring_the_config_page(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        declaring(|| {
            vec![
                IntegrationEndpoint::get(CONFIG_PAGE_PATH),
                IntegrationEndpoint::post(CONFIG_PAGE_PATH),
            ]
        })
    }

    /// A builder for the test's own module, built by `register`.
    fn own_module(register: IntegrationBuilderFn) -> IntegrationBuilder {
        IntegrationBuilder::new(OWN_MODULE_ID, "own-module-test", register, |settings| {
            Ok(settings.selects_module(OWN_MODULE_NAME))
        })
        .with_module_name(OWN_MODULE_NAME)
    }

    /// The route tests' settings with `extra` appended.
    fn settings_with(extra: &str) -> Settings {
        Settings::from_toml(&format!(
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
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            {extra}
            "#
        ))
        .expect("should parse route test settings")
        // A loaded domain carries its policy.
        .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
    }

    /// The router with the `builders` composed in.
    fn router_with(extra: &str, builders: &[IntegrationBuilder]) -> RouterService {
        TrustedServerApp::routes_with_registrations(settings_with(extra), builders)
            .expect("should build a router from the composed builders")
    }

    /// Sends one request with `method` to `path`, carrying the probe's
    /// counting header when `token` is given.
    async fn send(
        router: &RouterService,
        method: Method,
        path: &str,
        token: Option<&str>,
    ) -> Response {
        let mut builder = request_builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header(seam_probe::SEAM_PROBE_COUNT_HEADER, token);
        }
        let request = builder.body(Body::empty()).expect("should build request");
        router.oneshot(request).await.expect("should route request")
    }

    /// The response's `Content-Type`.
    fn content_type(response: &Response) -> Option<&str> {
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
    }

    /// The response's body as text.
    fn body_text(response: Response) -> String {
        let body = response.into_body().into_bytes().unwrap_or_default();
        String::from_utf8_lossy(&body).into_owned()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_get_of_the_probe_s_own_path_answers_from_the_module() {
        let router = router_with(PROBE_BLOCK, &[seam_probe::builder()]);

        let response = send(&router, Method::GET, seam_probe::SEAM_PROBE_OWN_PATH, None).await;

        assert_eq!(
            response.status().as_u16(),
            200,
            "the module's route should answer"
        );
        assert_eq!(content_type(&response), Some("text/plain"));
        assert_eq!(
            body_text(response),
            seam_probe::SEAM_PROBE_OWN_BODY,
            "should carry the module's body"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_head_of_the_probe_s_own_path_answers_with_no_body() {
        let router = router_with(PROBE_BLOCK, &[seam_probe::builder()]);

        let response = send(&router, Method::HEAD, seam_probe::SEAM_PROBE_OWN_PATH, None).await;

        assert_eq!(
            response.status().as_u16(),
            200,
            "the module's HEAD route should answer"
        );
        assert_eq!(content_type(&response), Some("text/plain"));
        assert_eq!(body_text(response), "", "should carry no body");
    }

    /// The probe declares no `POST` on its own path, so a `POST` there goes
    /// to the publisher path and the module's handler never runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_post_of_the_probe_s_own_path_never_reaches_the_module() {
        const TOKEN: &str = "cloudflare-own-path-post";
        let router = router_with(PROBE_BLOCK, &[seam_probe::builder()]);

        let response = send(
            &router,
            Method::POST,
            seam_probe::SEAM_PROBE_OWN_PATH,
            Some(TOKEN),
        )
        .await;

        assert_eq!(
            seam_probe::prepare_runs_for(TOKEN),
            1,
            "the preparers should run once, so the token reached the modules"
        );
        assert_eq!(
            seam_probe::own_path_runs_for(TOKEN),
            0,
            "the module's handler should not run for a method it does not declare"
        );
        assert_ne!(
            body_text(response),
            seam_probe::SEAM_PROBE_OWN_BODY,
            "the answer should not be the module's"
        );
    }

    #[test]
    fn a_second_get_route_on_the_probe_s_own_path_is_refused() {
        let settings = settings_with(
            r#"
            [testing]
            modules = ["seam-probe", "own-module"]

            [testing.seam-probe]
            country = "ZZ"
            "#,
        );

        let error = build_state_with_registrations(
            settings,
            &[
                seam_probe::builder(),
                own_module(declaring_the_probe_s_path),
            ],
        )
        .err()
        .expect("should refuse a second GET route on the probe's own path");

        let message = error.to_string();
        assert!(
            message.contains(&format!(
                "Integration route registration failed for GET {}",
                seam_probe::SEAM_PROBE_OWN_PATH
            )),
            "should name the method and the path: {message}"
        );
    }

    /// The adapter answers `GET /_ts/config` itself, so a module's route
    /// there never runs, and it has no route of its own for `POST` there, so
    /// the same module's route for that method runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_module_route_runs_only_where_the_adapter_answers_no_route_of_its_own() {
        let router = router_with(
            r#"
            [testing]
            modules = ["own-module"]
            "#,
            &[own_module(declaring_the_config_page)],
        );

        let page = send(&router, Method::GET, CONFIG_PAGE_PATH, None).await;
        assert_eq!(
            page.status().as_u16(),
            200,
            "the inspection page should answer"
        );
        assert_eq!(content_type(&page), Some("text/html; charset=utf-8"));
        assert_ne!(body_text(page), OWN_MODULE_BODY);
        assert_eq!(
            OWN_MODULE_RUNS.load(Ordering::SeqCst),
            0,
            "the module's GET route should never run"
        );

        let posted = send(&router, Method::POST, CONFIG_PAGE_PATH, None).await;
        assert_eq!(
            posted.status().as_u16(),
            200,
            "the module's POST route should answer"
        );
        assert_eq!(body_text(posted), OWN_MODULE_BODY);
        assert_eq!(
            OWN_MODULE_RUNS.load(Ordering::SeqCst),
            1,
            "the module's POST route should run once"
        );
    }
}

/// Regression test: a Next.js navigation with a pending auction must buffer to
/// the structural body close. The Flight payload carries a literal `</body>`, so
/// a parser-blind seam would inject bids early and split the RSC data.
///
/// This covers the buffered path only. This adapter routes navigations through
/// `buffer_publisher_response_async`, which resolves the body close without the
/// deferred inline seam marker, so the streaming seam token is exercised by the
/// Fastly adapter alone and not by this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nextjs_auction_output_holds_until_the_structural_body_close() {
    use std::sync::Arc;

    use trusted_server_core::test_support::nextjs_auction;

    let client = Arc::new(nextjs_auction::NextJsAuctionOrigin::default());
    let router = TrustedServerApp::routes_with_settings_and_services(
        nextjs_auction::settings(),
        nextjs_auction::services(Arc::clone(&client)),
    )
    .expect("should build router with fixture services");

    let request = edgezero_core::http::request_builder()
        .method("GET")
        .uri("https://test-publisher.example.com/article")
        .header("host", "test-publisher.example.com")
        .header("accept", "text/html")
        .body(edgezero_core::body::Body::empty())
        .expect("should build publisher navigation");
    let response = router
        .oneshot(request)
        .await
        .expect("should serve publisher navigation");
    assert_eq!(response.status(), 200, "should serve fixture HTML");
    let body = response
        .into_body()
        .into_bytes()
        .expect("should buffer adapter output");
    let html = String::from_utf8(body.to_vec()).expect("should emit UTF-8 HTML");

    assert_eq!(
        client.auction_requests(),
        1,
        "should dispatch exactly one auction"
    );
    let bids = html
        .find("var b=JSON.parse(")
        .unwrap_or_else(|| panic!("should inject auction bids: {html}"));
    let close = html
        .rfind("</body>")
        .unwrap_or_else(|| panic!("should retain structural close: {html}"));
    assert!(
        bids < close && html[bids..].ends_with("</script></body></html>"),
        "should inject bids immediately before the structural body close: {html}"
    );
    // The fixture splits the URL across two scripts, so the rewritten payload
    // never appears contiguously. Assert on the recomputed `T` length instead:
    // it shrinks only when the origin URL was actually replaced.
    assert!(
        html.contains(&nextjs_auction::expected_rewritten_flight_header()),
        "should recompute the Flight T length after rewriting the URL: {html}"
    );
    assert!(
        !html.contains(nextjs_auction::ORIGIN_HOST),
        "should leave no origin host in the rewritten payload: {html}"
    );
    assert!(
        !html.contains("__ts_rsc_") && !html.contains("<!--ts-inline-body-close-"),
        "should not leak generated placeholders: {html}"
    );
}
