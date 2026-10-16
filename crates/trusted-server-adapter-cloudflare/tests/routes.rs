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
/// The settings baked into the binary contain placeholder secrets that
/// `get_settings()` rejects by design, which would turn every route into a
/// startup error page (and its route table into the fallback-only set).
fn test_router() -> RouterService {
    let settings = Settings::from_toml(
        r#"
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
    .expect("should parse route test settings");

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
/// handlers instead of the `startup_error_router` fallback. The settings baked
/// into the binary carry placeholder secrets that `get_settings()` rejects,
/// which would otherwise turn every route into a startup error page.
fn make_router() -> RouterService {
    let settings = trusted_server_core::settings::Settings::from_toml(
        r#"
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

// ---------------------------------------------------------------------------
// Middleware regression tests, which verify FinalizeResponseMiddleware is
// wired so it cannot be removed silently.
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
            let router = test_router();
            let req = request_builder()
                .method(*method)
                .uri(path)
                .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
                .header("content-type", "application/json")
                .body(edgezero_core::body::Body::from("{\"key_id\":\"leak-me\"}"))
                .expect("should build authorized key administration request");

            let resp = route(router, req).await;

            assert_eq!(
                resp.status().as_u16(),
                404,
                "{method} {path} with Authorization must be answered here (404), not proxied to publisher"
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
            !text.contains("route-test-proxy-secret"),
            "{path} should not carry the proxy secret"
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

/// Every route registered beneath an underscore prefix is one of the
/// addresses an attestation endpoint may not take, so a new fixed route
/// cannot leave an endpoint free to register its address twice.
#[test]
fn every_fixed_route_beneath_an_underscore_prefix_is_reserved() {
    use trusted_server_core::attestation::RESERVED_PATHS;

    for route in test_router().routes() {
        let path = route.path();
        if path.starts_with("/_") {
            assert!(
                RESERVED_PATHS.contains(&path),
                "{path} should be listed in attestation::RESERVED_PATHS"
            );
        }
    }
}

/// The attestation pages are answered without the request preparers, so no
/// preparer can change or refuse the request whose host and nonce are
/// signed. The preparer here refuses every request, and the publisher
/// fallback shows that it runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attestation_routes_skip_the_request_preparers() {
    use error_stack::Report;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};

    fn refuse_every_request(
        _settings: &Settings,
        _request: &mut Request,
    ) -> Result<(), Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Forbidden {
            message: "refused by the route test's preparer".to_owned(),
        }))
    }

    fn never_registers(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(None)
    }

    fn selects_nothing(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Ok(false)
    }

    let settings = Settings::from_toml(
        r#"
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

            [attestation]
            endpoint = "/_ex/attestation"
            operator = "Example Operator"
            context = "example-attestation:v1"
            verify_url = "https://verifier.example/verify?host=test-publisher.example.com"
        "#,
    )
    .expect("should parse route test settings with an attestation section");
    let refusing = IntegrationBuilder::new(
        "refusing-preparer",
        "routes-test",
        never_registers,
        selects_nothing,
    )
    .with_module_name("testing.refusing-preparer")
    .with_request_preparer(refuse_every_request);
    let router = TrustedServerApp::routes_with_registrations(settings, &[refusing])
        .expect("should build router with the refusing preparer");
    let get = |uri: &str| {
        request_builder()
            .method("GET")
            .uri(uri)
            .body(edgezero_core::body::Body::empty())
            .expect("should build request")
    };

    let page = route(router.clone(), get("/article")).await;
    assert_eq!(
        page.status(),
        403,
        "the preparer should refuse a request for the publisher's origin"
    );

    for (uri, status) in [
        ("/_ex", 200),
        ("/_ex/attestation.json", 503),
        ("/_ts/attestation?nonce=abc123", 301),
    ] {
        let response = route(router.clone(), get(uri)).await;
        assert_eq!(
            response.status(),
            status,
            "{uri} should be answered without the preparers"
        );
    }
}

// ---------------------------------------------------------------------------
// robots.txt
// ---------------------------------------------------------------------------

/// The route test settings with a `[robots-txt]` section that refuses every
/// crawler and keeps one path open.
fn router_refusing_every_crawler() -> RouterService {
    let settings = Settings::from_toml(
        r#"
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

            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]
        "#,
    )
    .expect("should parse route test settings");

    TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings")
}

fn robots_request(method: &str, path: &str) -> Request {
    request_builder()
        .method(method)
        .uri(path)
        .body(edgezero_core::body::Body::empty())
        .expect("should build request")
}

fn robots_tag(response: &Response) -> Option<String> {
    response
        .headers()
        .get("x-robots-tag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusing_every_crawler_serves_the_refusal_and_tags_every_response() {
    let file = route(
        router_refusing_every_crawler(),
        robots_request("GET", "/robots.txt"),
    )
    .await;

    assert_eq!(
        file.status().as_u16(),
        200,
        "the refusal should be served here"
    );
    assert_eq!(robots_tag(&file).as_deref(), Some("noindex, nofollow"));
    let body = file.into_body().into_bytes().unwrap_or_default();
    assert_eq!(&body[..], b"User-agent: *\nAllow: /ads.txt\nDisallow: /\n");

    let other = route(
        router_refusing_every_crawler(),
        robots_request("GET", "/.well-known/trusted-server.json"),
    )
    .await;

    assert_eq!(
        robots_tag(&other).as_deref(),
        Some("noindex, nofollow"),
        "no route may be the one page a crawler is allowed to index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_of_robots_txt_is_answered_here_with_no_body() {
    let response = route(
        router_refusing_every_crawler(),
        robots_request("HEAD", "/robots.txt"),
    )
    .await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "a HEAD should be answered here as a GET is"
    );
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    let body = response.into_body().into_bytes().unwrap_or_default();
    assert!(body.is_empty(), "a HEAD carries no body");
}

/// A method that is neither `GET` nor `HEAD` is the publisher's, as it is on
/// any other path, so it is not answered with the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_method_on_robots_txt_is_not_answered_with_the_file() {
    let response = route(
        router_refusing_every_crawler(),
        robots_request("POST", "/robots.txt"),
    )
    .await;

    let body = response.into_body().into_bytes().unwrap_or_default();
    assert!(
        !body.starts_with(b"User-agent:"),
        "a POST should reach the publisher fallback and not be given the file"
    );
}

#[test]
fn robots_txt_is_this_servers_only_when_the_settings_carry_the_section() {
    let registered = |router: RouterService| -> Vec<(String, String)> {
        router
            .routes()
            .into_iter()
            .map(|route| (route.method().to_string(), route.path().to_string()))
            .collect()
    };

    let without = registered(test_router());
    assert!(
        !without.iter().any(|(_, path)| path == "/robots.txt"),
        "without the section the path is the publisher's: {without:?}"
    );

    let with = registered(router_refusing_every_crawler());
    for method in ["GET", "HEAD"] {
        assert!(
            with.iter()
                .any(|(registered, path)| registered == method && path == "/robots.txt"),
            "{method} /robots.txt should be answered here: {with:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_response_carries_no_blanket_rule_without_the_section() {
    let response = route(
        test_router(),
        robots_request("GET", "/.well-known/trusted-server.json"),
    )
    .await;

    assert_eq!(robots_tag(&response), None);
}
