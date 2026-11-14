//! Route tests for the Cloudflare adapter.
//!
//! Runs on the host target (no Workers runtime). Covers route registration,
//! middleware, auth, and the publisher origin's input cap and decoding. Every
//! outbound call goes through a scripted client, so nothing reaches the
//! network.

// Test crates may buffer bodies with `Body::into_bytes`.
#![allow(clippy::disallowed_methods)]

use edgezero_adapter_cloudflare::CLOUDFLARE_PLATFORM;
use edgezero_core::app::{App, Hooks as _};
use edgezero_core::body::Body;
use edgezero_core::http::{
    HeaderValue, Method, Request, Response, StatusCode, header, request_builder,
};
use edgezero_core::ingress::{IngressDispatchOutcome, IngressFraming, IngressHeadAccounting};
use edgezero_core::outbound::{ResponseDecoding, ResponseMode};
use edgezero_core::response_egress_framing::ResponseEgressFramingMode;
use edgezero_core::router::RouterService;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_core::constants::TS_GIT_VERSION;
use trusted_server_core::platform::HttpClient;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::publisher_input_cap::{self, Served};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};
use trusted_server_core::test_support::{gzip, insert_outbound_client};

const LEGACY_ADMIN_DENY_METHODS: &[&str] =
    &["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// Build the full application router from explicit test settings.
///
/// The settings baked into the binary contain placeholder secrets that
/// `get_settings()` rejects by design, which would turn every route into a
/// startup error page (and its route table into the fallback-only set).
/// The handler regex is the production-shaped `^/_ts/admin`, matching
/// `Settings::ADMIN_ENDPOINTS` and the default config, so the canonical
/// `/_ts/admin/keys/*` routes are auth-gated exactly as in production.
fn test_router() -> RouterService {
    TrustedServerApp::routes_with_settings(test_settings())
        .expect("should build router from test settings")
}

/// The settings [`test_router`] builds its router from.
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
            proxy_secret = "route-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
        "#,
    )
    .expect("should parse route test settings")
}

/// Return the set of (METHOD, path) pairs explicitly registered on the router.
fn registered_routes() -> Vec<(String, String)> {
    test_router()
        .routes()
        .into_iter()
        .map(|r| (r.method().to_string(), r.path().to_string()))
        .collect()
}

async fn route(router: RouterService, mut req: Request) -> Response {
    // `oneshot` skips `EdgeZero`'s request conversion, which installs the
    // adapter's default client, so install one that fails every request.
    insert_outbound_client(
        &mut req,
        HttpClient::with_client(ScriptedHttpClient::forbidding()),
    );
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
/// handlers. The settings baked into the binary carry placeholder secrets that
/// `get_settings()` rejects, which would otherwise fail startup.
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
fn routes_report_missing_worker_config_as_internal_error() {
    // Host targets have no Worker config binding, so startup fails and
    // `routes()` returns the error EdgeZero answers with.
    let Err(error) = TrustedServerApp::routes() else {
        panic!("should fail startup without a Worker config binding");
    };

    assert_eq!(
        error.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "should map the configuration error to a 500"
    );
}

// ---------------------------------------------------------------------------
// Synthesized headers on EdgeZero's canonical responses
// ---------------------------------------------------------------------------

/// Dispatches one request through the app `run_app` assembles, so the
/// policies `TrustedServerApp::configure` installs apply, and returns the
/// response head as egress would write it.
async fn dispatch_through_app(method: Method, target: &str) -> Response {
    let app = App::from_router::<TrustedServerApp>(test_router(), CLOUDFLARE_PLATFORM)
        .expect("should assemble the app with its edge policies");
    let request = request_builder()
        .method(method)
        .uri(target)
        .body(Body::empty())
        .expect("should build request");
    let outcome = app
        .dispatch_ingress(
            request,
            app.monotonic_now(),
            IngressHeadAccounting::HostManaged,
            IngressFraming::HostManaged,
        )
        .await
        .expect("should admit the request");
    let envelope = match outcome {
        IngressDispatchOutcome::Response(envelope) => Some(envelope),
        _ => None,
    }
    .expect("should answer with a response");
    let (prepared, _policy, mut attempt, clock) = envelope
        .begin(ResponseEgressFramingMode::ExplicitBufferedLength)
        .map_err(|_failure| "response egress failed before conversion")
        .expect("should begin response egress");
    assert!(attempt.begin_writing(), "should start writing the response");
    assert!(
        attempt.complete(clock.now()),
        "should complete the response"
    );
    prepared.into_response()
}

/// Returns the git version compiled into this build.
///
/// # Panics
///
/// Panics when the build has none, so a version assertion cannot pass on an
/// absent header.
fn compiled_git_version() -> &'static str {
    TS_GIT_VERSION.expect("should compile a git version into the test build")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_not_found_carries_x_ts_version() {
    // The publisher fallback claims every origin-form path, so only an
    // asterisk-form target has no route and gets EdgeZero's canonical 404.
    let response = dispatch_through_app(Method::OPTIONS, "*").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "should answer an unrouted target with the canonical 404"
    );
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|value| value.to_str().ok()),
        Some(compiled_git_version()),
        "should add the compiled-in git version to the canonical 404"
    );
}

/// A handler `Err` still passes through the finalize middleware.
///
/// `dispatch_ingress` installs no outbound client, so the auction handler
/// fails before it runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_error_carries_x_ts_version() {
    let response = dispatch_through_app(Method::POST, "/auction").await;

    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "should answer the failed Cloudflare handler with 500"
    );
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|value| value.to_str().ok()),
        Some(compiled_git_version()),
        "should add the compiled-in git version to the handler error response"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_method_not_allowed_carries_allow_and_x_ts_version() {
    let response = dispatch_through_app(Method::TRACE, "/auction").await;

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "should answer an unrouted method on a known path with the canonical 405"
    );
    assert_eq!(
        response
            .headers()
            .get(header::ALLOW)
            .and_then(|value| value.to_str().ok()),
        Some("DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"),
        "should list the path's methods in sorted order"
    );
    assert_eq!(
        response
            .headers()
            .get("x-ts-version")
            .and_then(|value| value.to_str().ok()),
        Some(compiled_git_version()),
        "should add the compiled-in git version to the canonical 405"
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
    let src = trusted_server_core::tsjs::tsjs_script_src(&["creative"]);
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
    use trusted_server_core::test_support::nextjs_auction;

    let client = nextjs_auction::NextJsAuctionOrigin::default();
    let router = TrustedServerApp::routes_with_settings_and_services(
        nextjs_auction::settings(),
        nextjs_auction::services(&client),
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

// ---------------------------------------------------------------------------
// Publisher origin through EdgeZero's client
// ---------------------------------------------------------------------------

/// Origin of the route test settings.
const ORIGIN: &str = "https://origin.test-publisher.example.com";

/// Cloudflare's origin input cap with the default 16 MiB `max_buffered_body_bytes`.
const CLOUDFLARE_INPUT_CAP: usize = 10 * 1024 * 1024;

/// Sends `req` through `router` with `origin` as the request's outbound client.
async fn route_to_origin(
    router: RouterService,
    mut req: Request,
    origin: &ScriptedHttpClient,
) -> Response {
    insert_outbound_client(&mut req, HttpClient::with_client(origin.clone()));
    router.oneshot(req).await.expect("should route request")
}

/// A publisher GET for `path` on the test publisher host.
fn publisher_get(path: &str) -> Request {
    request_builder()
        .method(Method::GET)
        .uri(format!("https://test-publisher.example.com{path}"))
        .header(header::HOST, "test-publisher.example.com")
        .body(Body::empty())
        .expect("should build publisher request")
}

/// An identity HTML document exactly `len` bytes long.
fn html_document(len: usize) -> Vec<u8> {
    let mut document = b"<html><body>".to_vec();
    document.resize(len - b"</body></html>".len(), b'a');
    document.extend_from_slice(b"</body></html>");
    document
}

/// A 200 gzip origin reply carrying `body` with `content_type`.
fn gzip_origin_reply(content_type: &str, body: &[u8]) -> ScriptedReply {
    ScriptedReply::status(StatusCode::OK)
        .header("content-type", content_type)
        .header("content-encoding", "gzip")
        .body(gzip(body))
}

/// Sends a publisher GET for `path` that accepts gzip through `router`, with
/// `origin` as its outbound client.
async fn fetch_publisher(router: RouterService, origin: &ScriptedHttpClient, path: &str) -> Served {
    let mut request = publisher_get(path);
    request
        .headers_mut()
        .insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    Served::from_router_response(route_to_origin(router, request, origin).await)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_input_cap_matrix_cloudflare() {
    let origin = ScriptedHttpClient::new(&TestClock::new());

    publisher_input_cap::assert_input_cap_matrix(
        &origin,
        CLOUDFLARE_INPUT_CAP,
        ResponseDecoding::Decode,
        |path| fetch_publisher(test_router(), &origin, path),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn processed_output_keeps_its_own_bound_cloudflare() {
    let mut settings = test_settings();
    settings.publisher.max_buffered_body_bytes = publisher_input_cap::OUTPUT_BOUND_BYTES;
    let origin = ScriptedHttpClient::new(&TestClock::new());

    publisher_input_cap::assert_output_bound(&origin, |path| {
        fetch_publisher(
            TrustedServerApp::routes_with_settings(settings.clone())
                .expect("should build router from output-bound settings"),
            &origin,
            path,
        )
    })
    .await;
}

/// Serves `path` from a gzip origin to a client that sends no `Accept-Encoding`.
async fn fetch_gzip_origin(path: &str, content_type: &str, body: &[u8]) -> (Response, Vec<u8>) {
    let origin = ScriptedHttpClient::new(&TestClock::new());
    origin.respond_to(
        Method::GET,
        &format!("{ORIGIN}{path}"),
        gzip_origin_reply(content_type, body),
    );
    let request = publisher_get(path);
    assert!(
        !request.headers().contains_key(header::ACCEPT_ENCODING),
        "should send no Accept-Encoding"
    );
    let mut response = route_to_origin(test_router(), request, &origin).await;
    let served = std::mem::replace(response.body_mut(), Body::empty())
        .into_bytes()
        .expect("should buffer the served body");
    (response, served.to_vec())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gzip_origin_html_reaches_a_client_without_accept_encoding_as_identity_cloudflare() {
    let page = b"<html><head><title>Example</title></head><body><p>example</p></body></html>";

    let (response, served) =
        fetch_gzip_origin("/gzip-page", "text/html; charset=utf-8", page).await;

    assert_eq!(
        (
            response.status(),
            response.headers().get(header::CONTENT_ENCODING)
        ),
        (StatusCode::OK, None),
        "should serve the processed page as identity"
    );
    let html = String::from_utf8(served).expect("should serve UTF-8 HTML");
    assert!(
        html.contains("/static/tsjs=") && html.contains("<p>example</p>"),
        "should serve the decoded page with the TSJS tag: {html}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gzip_origin_raw_body_reaches_a_client_without_accept_encoding_as_identity_cloudflare() {
    let bytes = b"example raw origin bytes";

    let (response, served) =
        fetch_gzip_origin("/asset.bin", "application/octet-stream", bytes).await;

    assert_eq!(
        (
            response.status(),
            response.headers().get(header::CONTENT_ENCODING),
            served.as_slice()
        ),
        (StatusCode::OK, None, bytes.as_slice()),
        "should relay the raw body decoded, as identity"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_origin_streams_its_decoded_response_under_the_input_cap_cloudflare() {
    let origin = ScriptedHttpClient::new(&TestClock::new());
    origin.respond_to(
        Method::GET,
        &format!("{ORIGIN}/page"),
        gzip_origin_reply("text/html", &html_document(64)),
    );

    let response = route_to_origin(test_router(), publisher_get("/page"), &origin).await;

    assert_eq!(response.status(), StatusCode::OK, "should serve the page");
    let requests = origin.requests();
    let [forwarded] = requests.as_slice() else {
        panic!("should send one origin request, got {requests:?}");
    };
    assert_eq!(
        (
            forwarded.response_mode,
            forwarded.response_decoding,
            forwarded.max_encoded_response_bytes,
        ),
        (
            ResponseMode::Streamed,
            ResponseDecoding::Decode,
            Some(u64::try_from(CLOUDFLARE_INPUT_CAP).expect("should fit the cap in u64")),
        ),
        "should stream the decoded origin response and cap its encoded bytes"
    );
}
