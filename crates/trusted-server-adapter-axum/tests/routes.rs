//! Integration tests for the Axum dev server.
//!
//! Uses `EdgeZeroAxumService` directly (no live TCP server) so tests remain fast
//! and self-contained. Each test builds the full `TrustedServerApp` router and
//! drives it through the Tower `Service` interface.

use axum::body::Body as AxumBody;
use axum::http::Request;
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp;

const LEGACY_ADMIN_DENY_METHODS: &[&str] =
    &["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// Explicit test settings for the route tests.
///
/// The process reads its settings from its config store when it starts,
/// which a test does not set up, so `TrustedServerApp::routes()` would turn
/// every route into a startup error page (and its route table into the
/// fallback-only set).
fn test_settings() -> trusted_server_core::settings::Settings {
    test_settings_with("")
}

/// The route tests' settings with `extra` appended.
fn test_settings_with(extra: &str) -> trusted_server_core::settings::Settings {
    trusted_server_core::settings::Settings::from_toml(&format!(
        r#"
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "integration-test-proxy-secret"

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
    // A loaded domain carries its policy, and these tests decide under the
    // repository's sample.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// Build the full application router from explicit test settings.
fn test_router() -> edgezero_core::router::RouterService {
    TrustedServerApp::routes_with_settings(test_settings())
        .expect("should build router from test settings")
}

fn make_service() -> EdgeZeroAxumService {
    EdgeZeroAxumService::new(test_router())
}

fn registered_routes() -> Vec<(String, String)> {
    test_router()
        .routes()
        .into_iter()
        .map(|r| (r.method().to_string(), r.path().to_string()))
        .collect()
}

fn assert_route_registered(method: &str, path: &str) {
    let routes = registered_routes();
    assert!(
        routes.iter().any(|(m, p)| m == method && p == path),
        "{method} {path} must be explicitly registered; registered routes: {routes:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aps_demand_source_serves_the_renderer_through_adapter_fallback() {
    let mut settings = test_settings();
    let table = serde_json::Map::from_iter([
        (
            "implementation".to_string(),
            serde_json::json!("auction.aps"),
        ),
        (
            "endpoint".to_string(),
            serde_json::json!("https://aps.example/e/pb/bid"),
        ),
        ("routing".to_string(), serde_json::json!("all_eligible")),
        (
            "account_id".to_string(),
            serde_json::json!("example-account"),
        ),
    ]);
    settings.demand = trusted_server_core::provider_table::ProviderList::new(
        vec!["aps_main".to_string()],
        std::collections::BTreeMap::from([("aps_main".to_string(), table)]),
    );
    let router = TrustedServerApp::routes_with_settings(settings)
        .expect("should build a router with an APS demand source");
    let mut service = EdgeZeroAxumService::new(router);
    let request = Request::builder()
        .method("GET")
        .uri("/integrations/aps/renderer")
        .body(AxumBody::empty())
        .expect("should build APS renderer request");

    let response = service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should serve APS renderer");
    assert_eq!(response.status().as_u16(), 200);
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
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify routing (not business logic correctness)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_endpoint_is_routed() {
    // Verifies the route exists — 5xx from missing signing keys is acceptable;
    // 404 is not (that would mean the route was not registered).
    let mut svc = make_service();

    let req = Request::builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_ne!(
        resp.status().as_u16(),
        404,
        "discovery endpoint must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_endpoint_is_routed() {
    let mut svc = make_service();

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_ne!(
        resp.status().as_u16(),
        404,
        "verify-signature must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_prefix_is_handled_not_5xx() {
    let mut svc = make_service();

    // /static/tsjs= is a GET /{*rest} catch-all path. The handler returns 404
    // for an unknown hash, which is correct application behaviour (not a routing 404).
    // This test verifies the handler is reached (no 5xx/panic) and that routing works.
    let req = Request::builder()
        .method("GET")
        .uri("/static/tsjs=0000000000000000")
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    let status = resp.status().as_u16();

    assert!(
        status < 500,
        "tsjs catch-all handler must not return 5xx: got {status}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_matching_hash_uses_s_maxage_fallback() {
    let mut svc = make_service();
    let src = trusted_server_core::tsjs::tsjs_script_src(
        &trusted_server_core::tsjs_bundle::compile_time_parts(&["creative"]),
    );
    let req = Request::builder()
        .method("GET")
        .uri(src)
        .body(AxumBody::empty())
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_eq!(
        resp.status().as_u16(),
        200,
        "matching TSJS hash should serve OK"
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=31536000, s-maxage=31536000, immutable"),
        "Axum adapter should render the portable s-maxage fallback"
    );
    assert!(
        resp.headers().get("surrogate-control").is_none(),
        "s-maxage fallback must not emit Fastly Surrogate-Control"
    );
}

// ---------------------------------------------------------------------------
// Middleware tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_writes_the_operators_headers_and_no_geo_header() {
    let mut settings = test_settings();
    settings
        .response_headers
        .insert("X-Finalized".to_owned(), "1".to_owned());
    let mut svc = EdgeZeroAxumService::new(
        TrustedServerApp::routes_with_settings(settings)
            .expect("should build router from test settings"),
    );

    let req = Request::builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");

    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");

    assert_eq!(
        resp.headers()
            .get("x-finalized")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "the finalize middleware should write the operator's headers on every response"
    );
    let geo_headers: Vec<&str> = resp
        .headers()
        .keys()
        .map(axum::http::HeaderName::as_str)
        .filter(|name| name.starts_with("x-geo-"))
        .collect();
    assert!(
        geo_headers.is_empty(),
        "the finalize middleware should write no x-geo-* header: {geo_headers:?}"
    );
}

// ---------------------------------------------------------------------------
// Closed paths and the inspection endpoints
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_authenticated_request_to_a_closed_path_is_answered_here() {
    let ec_id = format!("{}.abc123", "a".repeat(64));
    for path in [
        "/_ts/admin/ec".to_owned(),
        format!("/_ts/admin/ec/{ec_id}"),
        "/_ts/admin/eids".to_owned(),
        "/_ts/admin/ec/".to_owned(),
        format!("/_ts/admin/ec/{ec_id}/extra"),
        "/_ts/admin/eids/".to_owned(),
        "/_ts/admin/eids/extra".to_owned(),
        "/_ts/admin/eids.json".to_owned(),
        "/_ts/admin/ec;foo".to_owned(),
        format!("/_ts/admin/ec%2F{ec_id}"),
        // A check for a literal slash would miss a percent-encoded separator,
        // so these are closed before the publisher fallback forwards
        // credentials upstream.
        "/_ts/admin%2Fec".to_owned(),
        "/_ts/admin%2fec".to_owned(),
        // The alias outside `/_ts`, with its descendants and encoded
        // separators.
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
            let request = Request::builder()
                .method(method)
                .uri(&path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .body(AxumBody::from("sensitive-admin-body"))
                .expect("should build malformed admin request");
            let response = make_service()
                .ready()
                .await
                .expect("should be ready")
                .call(request)
                .await
                .expect("should respond");

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
async fn key_administration_paths_are_answered_here_and_never_proxied() {
    // A request to a key administration path carries an operator's
    // credentials and a key payload. Every publisher-fallback method must be
    // answered here with 404 and never proxied to the publisher origin, which
    // would be handed both. A publisher-fallback proxy without a backend would
    // surface as a 5xx, so a 404 proves the request was answered here.
    for path in [
        "/_ts/admin/keys/rotate",
        "/_ts/admin/keys/deactivate",
        "/_ts/admin/cache/purge",
        "/admin/keys/rotate",
        "/admin/keys/deactivate",
    ] {
        for method in LEGACY_ADMIN_DENY_METHODS {
            let mut svc = make_service();
            let req = Request::builder()
                .method(*method)
                .uri(path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .header("content-type", "application/json")
                .body(AxumBody::from("{\"key_id\":\"leak-me\"}"))
                .expect("should build authorized key administration request");
            let resp = svc
                .ready()
                .await
                .expect("should be ready")
                .call(req)
                .await
                .expect("should respond");
            assert_eq!(
                resp.status().as_u16(),
                404,
                "{method} {path} with Authorization must be answered here (404), not proxied to publisher"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_endpoint_answers_anyone_as_data_and_as_a_page() {
    for (path, content_type) in [
        ("/_ts/permissions.json", "application/json"),
        ("/_ts/permissions", "text/html; charset=utf-8"),
    ] {
        let mut svc = make_service();
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(AxumBody::empty())
            .expect("should build request");
        let resp = svc
            .ready()
            .await
            .expect("should be ready")
            .call(req)
            .await
            .expect("should respond");
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
        let mut svc = make_service();
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(AxumBody::empty())
            .expect("should build request");
        let resp = svc
            .ready()
            .await
            .expect("should be ready")
            .call(req)
            .await
            .expect("should respond");
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("should read the body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("XXXX"),
            "{path} should mask what it does not show"
        );
        assert!(
            !text.contains("integration-test-proxy-secret"),
            "{path} should not carry the proxy secret"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_endpoint_answers_a_browser_opening_the_page_and_nothing_else() {
    for (opened_as_a_page, expected) in [(false, 403), (true, 200)] {
        let mut svc = make_service();
        let mut builder = Request::builder().method("GET").uri("/_ts/data");
        if opened_as_a_page {
            builder = builder
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document");
        }
        let req = builder
            .body(AxumBody::empty())
            .expect("should build request");
        let resp = svc
            .ready()
            .await
            .expect("should be ready")
            .call(req)
            .await
            .expect("should respond");
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("should read the body");
        assert_eq!(
            String::from_utf8_lossy(&body).contains("keeps no record"),
            opened_as_a_page,
            "should say nothing is held to a browser opening the page, and to nothing else"
        );
    }
}

// ---------------------------------------------------------------------------
// First-party route smoke tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_is_routed() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("GET")
        .uri("/first-party/proxy")
        .body(AxumBody::empty())
        .expect("should build request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_click_is_routed() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("GET")
        .uri("/first-party/click")
        .body(AxumBody::empty())
        .expect("should build request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/click must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_is_routed() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("GET")
        .uri("/first-party/sign")
        .body(AxumBody::empty())
        .expect("should build request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        404,
        "GET /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_is_routed() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/first-party/sign")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        404,
        "POST /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_is_routed() {
    let mut svc = make_service();
    let req = Request::builder()
        .method("POST")
        .uri("/first-party/proxy-rebuild")
        .header("content-type", "application/json")
        .body(AxumBody::from("{}"))
        .expect("should build request");
    let resp = svc
        .ready()
        .await
        .expect("should be ready")
        .call(req)
        .await
        .expect("should respond");
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy-rebuild must be routed"
    );
}

// ---------------------------------------------------------------------------
// Edge Cookie module availability
// ---------------------------------------------------------------------------

/// Test settings selecting a vendor Edge Cookie module this adapter does not
/// inject, with the `[ec.acme]` block that module's settings live in.
/// `acme` is a fictional vendor key.
const UNINJECTED_MODULE_TOML: &str = r#"
    [publisher]
    domain = "test-publisher.example.com"
    cookie_domain = ".test-publisher.example.com"
    origin_url = "https://origin.test-publisher.example.com"
    proxy_secret = "integration-test-proxy-secret"

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
/// that injects a module knows what that module needs, and the Axum dev
/// server injects no vendor Edge Cookie module, so only the composition root
/// can catch it. Without the startup check the deployment would come up and
/// answer every request.
#[test]
fn selecting_a_module_this_adapter_cannot_supply_fails_at_startup() {
    let settings = trusted_server_core::settings::Settings::from_toml(UNINJECTED_MODULE_TOML)
        .expect("should parse settings selecting an uninjected module");

    // `RouterService` is not `Debug`, so take the error side directly rather
    // than through `expect_err`.
    let error = trusted_server_adapter_axum::app::TrustedServerApp::routes_with_settings(settings)
        .err()
        .expect("building state with an uninjected module should fail");

    assert!(
        error.to_string().contains("acme"),
        "the startup error should name the selected module, got: {error}"
    );
}

/// `[geo] module = "platform"` selects the host's own geo lookup, which the
/// Axum dev server does not have, so startup refuses it rather than come up
/// locating no request with the single-jurisdiction check skipped.
#[test]
fn selecting_the_platform_geo_lookup_fails_at_startup() {
    let error =
        TrustedServerApp::routes_with_settings(test_settings_with(r#"module = "platform""#))
            .err()
            .expect("should refuse the host's geo lookup on a host that has none");
    let message = error.to_string();

    assert!(
        message.contains(r#"`[geo] module = "platform"`"#) && message.contains("has no geo lookup"),
        "should name the setting and say the host has no geo lookup: {message}"
    );
}

/// Builds a registration claiming an id a stock module already owns.
fn duplicate_lockr_registration(
    _settings: &trusted_server_core::settings::Settings,
) -> Result<
    Option<trusted_server_core::integrations::IntegrationRegistration>,
    error_stack::Report<trusted_server_core::error::TrustedServerError>,
> {
    Ok(Some(
        trusted_server_core::integrations::IntegrationRegistration::builder("lockr").build(),
    ))
}

fn validate_nothing(
    _settings: &trusted_server_core::settings::Settings,
) -> Result<bool, error_stack::Report<trusted_server_core::error::TrustedServerError>> {
    Ok(true)
}

#[test]
fn routes_with_registrations_rejects_a_duplicate_integration_id_naming_both_sources() {
    let extra = [trusted_server_core::integrations::IntegrationBuilder::new(
        "lockr",
        "seam-probe",
        duplicate_lockr_registration,
        validate_nothing,
    )];

    let error = TrustedServerApp::routes_with_registrations(test_settings(), &extra)
        .err()
        .expect("should reject a duplicate integration id supplied through the adapter");

    let message = error.to_string();
    assert!(
        message.contains("lockr")
            && message.contains("trusted-server-identity-lockr")
            && message.contains("seam-probe"),
        "error should name the id and both sources: {message}"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_carries_no_version() {
    let mut service = make_service();
    let request = Request::builder()
        .method("GET")
        .uri("/health")
        .body(AxumBody::empty())
        .expect("should build health request");

    let response = service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should serve health");

    assert_eq!(
        response.status().as_u16(),
        200,
        "should return 200 on /health"
    );
    assert!(
        response.headers().get("x-ts-version").is_none(),
        "should write no version on /health, which the service_version middleware writes on a page"
    );
}

/// A publisher whose origin has no `ads.txt` gets one made of the sellers
/// lines, through this adapter's router and the stock sellers module.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sellers_module_makes_an_ads_txt_the_origin_does_not_have() {
    use std::sync::Arc;

    use trusted_server_core::platform::PlatformHttpClient;
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };

    const SELLERS: &str = r#"
        [ads-txt]
        modules = ["sellers"]

        [ads-txt.sellers]
        lines = ["ssp.example, 12345, DIRECT, f08c47fec0942fa0"]

        [[fetch]]
        media_type = "text/plain"
        path = "/ads.txt"
        middleware = ["ads-txt.sellers"]
    "#;

    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        404,
        b"<html><body>Not found</body></html>".to_vec(),
        vec![
            ("content-type", "text/html; charset=utf-8"),
            ("set-cookie", "session=origin; Path=/"),
            ("cache-control", "public, max-age=600"),
        ],
    );
    let router = TrustedServerApp::routes_with_settings_and_services(
        test_settings_with(SELLERS),
        build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>),
    )
    .expect("should build the router with the sellers module");
    let request = edgezero_core::http::request_builder()
        .method("GET")
        .uri("https://test-publisher.example.com/ads.txt")
        .header("host", "test-publisher.example.com")
        .body(edgezero_core::body::Body::empty())
        .expect("should build the ads.txt request");

    let response = router
        .oneshot(request)
        .await
        .expect("should answer the ads.txt request");

    assert_eq!(
        response.status(),
        200,
        "should make the file the origin does not have"
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert!(
        response.headers().get("set-cookie").is_none()
            && response.headers().get("cache-control").is_none(),
        "should carry none of the headers the origin sent with its 404: {:?}",
        response.headers()
    );
    let body = response
        .into_body()
        .into_bytes()
        .expect("should buffer the made file");
    assert_eq!(
        body.as_ref(),
        b"ssp.example, 12345, DIRECT, f08c47fec0942fa0\n",
        "should be the sellers lines alone"
    );
}

// ---------------------------------------------------------------------------
// robots.txt
// ---------------------------------------------------------------------------

/// The route test settings with a `[robots-txt]` section that refuses every
/// crawler and keeps one path open, and the entry that places the file.
fn settings_refusing_every_crawler() -> trusted_server_core::settings::Settings {
    trusted_server_core::settings::Settings::from_toml(
        r#"
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "integration-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
        "#,
    )
    .expect("should parse robots.txt test settings")
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// `/robots.txt` is the publisher path's, answered against an origin that
/// answers each request in turn. A `GET` has the origin's file replaced by
/// the refusal, a `HEAD` has the file made where the origin has none and
/// gets no body, and a `POST` keeps the origin's `404`, because a file is
/// made only for a `GET` or a `HEAD`. Each carries the refusal's
/// `X-Robots-Tag`, the file's own answer included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robots_txt_is_served_through_the_publisher_path_with_its_tag() {
    use std::sync::Arc;

    use edgezero_core::http::Response;
    use trusted_server_core::platform::PlatformHttpClient;
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };

    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"User-agent: *\nAllow: /\n".to_vec(),
        vec![("content-type", "text/plain")],
    );
    for _ in 0..2 {
        origin.push_response_with_headers(
            404,
            b"<html><body>not found</body></html>".to_vec(),
            vec![("content-type", "text/html")],
        );
    }
    let router = TrustedServerApp::routes_with_settings_and_services(
        settings_refusing_every_crawler(),
        build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>),
    )
    .expect("should build router with a stub origin");
    let robots_txt = |method: &str| {
        edgezero_core::http::request_builder()
            .method(method)
            .uri("https://test-publisher.example.com/robots.txt")
            .header("host", "test-publisher.example.com")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request")
    };
    let tag = |response: &Response| {
        response
            .headers()
            .get("x-robots-tag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let body = |response: Response| {
        let bytes = response
            .into_body()
            .into_bytes()
            .expect("should hold the answer in memory");
        String::from_utf8(bytes.to_vec()).expect("the body is text")
    };

    let get = router
        .clone()
        .oneshot(robots_txt("GET"))
        .await
        .expect("should serve the GET");
    assert_eq!(get.status(), 200);
    assert_eq!(tag(&get).as_deref(), Some("noindex, nofollow"));
    assert_eq!(body(get), "User-agent: *\nAllow: /ads.txt\nDisallow: /\n");

    let head = router
        .clone()
        .oneshot(robots_txt("HEAD"))
        .await
        .expect("should serve the HEAD");
    assert_eq!(
        head.status(),
        200,
        "the file is made where the origin has none"
    );
    assert_eq!(tag(&head).as_deref(), Some("noindex, nofollow"));
    assert_eq!(body(head), "", "a HEAD gets no body");

    let post = router
        .clone()
        .oneshot(robots_txt("POST"))
        .await
        .expect("should serve the POST");
    assert_eq!(
        post.status(),
        404,
        "a file is made only for a GET or a HEAD"
    );
    assert_eq!(tag(&post).as_deref(), Some("noindex, nofollow"));

    assert_eq!(
        origin.recorded_request_methods(),
        ["GET", "HEAD", "POST"],
        "each request reaches the origin"
    );
}
