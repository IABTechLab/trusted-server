//! Smoke tests for the Spin adapter route wiring.
//!
//! Runs on the host target (no Spin runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! Spin runtime bindings or outbound network calls. Tests with business logic
//! that depends on external stores assert routing only; deterministic auth and
//! method gates assert exact status codes.

use edgezero_core::app::Hooks as _;
use edgezero_core::http::{Request, Response, request_builder};
use edgezero_core::router::RouterService;
use trusted_server_adapter_spin::app::TrustedServerApp;
use trusted_server_core::settings::Settings;

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

async fn route(router: RouterService, req: Request) -> Response {
    router.oneshot(req).await.expect("should route request")
}

#[test]
fn routes_build_without_panic() {
    // build_state() may fail (no real settings in CI) — startup_error_router
    // is the fallback. Either way, routes() must not panic.
    let _router = TrustedServerApp::routes();
}

#[test]
fn edgezero_manifest_loads_and_resolves_spin_stores() {
    let loader = edgezero_core::manifest::ManifestLoader::load_from_str(include_str!(
        "../../../edgezero.toml"
    ));
    let manifest = loader.manifest();

    assert!(
        manifest.stores.config.is_some(),
        "Spin EdgeZero manifest must enable config store injection"
    );
    assert_eq!(
        manifest
            .stores
            .kv
            .as_ref()
            .expect("should declare a KV store")
            .default_id(),
        "trusted_server_kv",
        "Spin KV declaration must expose its default logical store id"
    );
    assert!(
        manifest.secret_store_enabled(edgezero_core::app::SPIN_ADAPTER),
        "Spin EdgeZero manifest must enable secret handle injection"
    );
}

// ---------------------------------------------------------------------------
// Middleware regression tests, which verify FinalizeResponseMiddleware is
// wired so it cannot be removed silently.
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
async fn health_route_returns_ok() {
    // Parity with the Fastly/Axum adapters: GET /health is a cheap liveness probe
    // answering 200 "ok", not routed through publisher handling.
    let router = test_router();

    let req = request_builder()
        .method("GET")
        .uri("/health")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.status().as_u16(),
        200,
        "health probe should return 200"
    );

    let body = resp.into_body().into_bytes().unwrap_or_default();
    assert_eq!(&body[..], b"ok", "health probe should return the body `ok`");
}

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
async fn the_middleware_chain_runs_for_a_routed_request() {
    // Asserts that the finalizing middleware ran (X-Geo-Info-Available present)
    // and that the route is actually reached (status != 404).
    let router = test_router();

    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");

    let resp = route(router, req).await;

    assert!(
        resp.headers().contains_key("x-geo-info-available"),
        "middleware chain must inject X-Geo-Info-Available"
    );
    assert_ne!(
        resp.status().as_u16(),
        404,
        "auction endpoint must be routed"
    );
}

// ---------------------------------------------------------------------------
// Route smoke tests — verify all adapter routes are registered. Some handlers
// depend on platform stores or outbound proxy settings, so those tests assert
// "not the 404 route miss" rather than a full business result.
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
    // The tsjs route is matched by the /{*rest} catch-all. The handler returns
    // 404 for an unknown hash; that is application behaviour, not a route miss.
    assert!(status < 500, "tsjs route must not 5xx: got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tsjs_route_matching_hash_uses_s_maxage_fallback() {
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
        Some("public, max-age=31536000, s-maxage=31536000, immutable"),
        "Spin adapter should render the portable s-maxage fallback"
    );
    assert!(
        resp.headers().get("surrogate-control").is_none(),
        "s-maxage fallback must not emit Fastly Surrogate-Control"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/verify-signature")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/verify-signature must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_put_falls_through_to_publisher_fallback() {
    // PUT is one of the publisher-fallback methods, and /verify-signature only
    // handles POST directly. Mirroring Fastly/Axum, a non-primary method on a
    // named path must fall through to the publisher/integration fallback (which
    // 502s here without a live origin) rather than returning a router-level 405.
    let router = test_router();
    let req = request_builder()
        .method("PUT")
        .uri("/verify-signature")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;

    let status = resp.status().as_u16();
    assert_ne!(
        status, 405,
        "PUT /verify-signature must fall through to publisher fallback, not 405"
    );
    assert_ne!(
        status, 404,
        "PUT /verify-signature must be routed to the fallback, not a route miss"
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
        for method in ["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"] {
            let router = test_router();
            let req = request_builder()
                .method(method)
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
            assert!(
                !resp.headers().contains_key("www-authenticate"),
                "{method} {path} must not issue an admin auth challenge"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/auction")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(r#"{"adUnits":[]}"#))
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(resp.status().as_u16(), 404, "/auction must be routed");
}

/// `GET` on the SPA re-auction endpoint must reach the page-bids handler on
/// both the canonical path and its deprecated `/__ts/` alias.
///
/// The alias is what pre-rename tsjs bundles still request, and on a SPA that
/// path is what delivers ads for in-session navigations — so a dropped or
/// misspelled registration silently costs revenue rather than erroring loudly.
/// Spin registers `GET` and `OPTIONS` separately, so the preflight-denial parity
/// test does not imply the `GET` side is wired.
///
/// Paths are literals rather than `PAGE_BIDS_PATH` / `PAGE_BIDS_LEGACY_PATH`:
/// this pins the actual URL the client fetches, which asserting a const against
/// itself would not.
///
/// These test settings configure no creative opportunities, so the handler's own
/// deterministic answer is a 404 `Creative opportunities not configured`. That
/// body is the anchor: an unregistered path would instead fall through to the
/// publisher fallback and attempt an outbound fetch to the (nonexistent) test
/// origin, which cannot produce this message. A bare `!= 404` check would be
/// wrong here — the handler legitimately returns 404 under this config.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_bids_get_is_routed_on_canonical_path_and_alias() {
    let mut responses = Vec::new();

    for path in ["/_ts/page-bids", "/__ts/page-bids"] {
        let req = request_builder()
            .method("GET")
            .uri(path)
            .header("sec-fetch-site", "same-origin")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let resp = route(test_router(), req).await;
        let status = resp.status().as_u16();
        let body = String::from_utf8_lossy(&resp.into_body().into_bytes().unwrap_or_default())
            .into_owned();

        assert!(
            body.contains("Creative opportunities not configured"),
            "GET {path} must reach the page-bids handler, \
             got status {status} body {body:?}"
        );

        responses.push((status, body));
    }

    assert_eq!(
        responses[0], responses[1],
        "the deprecated alias must answer identically to the canonical path"
    );
}

// ---------------------------------------------------------------------------
// Publisher fallback method parity — non-GET/POST methods must reach the
// publisher origin fallback (not a router-level 405), matching Fastly/Axum.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_root_reaches_publisher_fallback() {
    let router = test_router();
    let req = request_builder()
        .method("HEAD")
        .uri("/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "HEAD / must reach the publisher fallback, not return 405"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn options_page_reaches_publisher_fallback() {
    let router = test_router();
    let req = request_builder()
        .method("OPTIONS")
        .uri("/some/page")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "OPTIONS /some/page (CORS preflight) must reach the publisher fallback, not 405"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_named_get_route_reaches_publisher_fallback() {
    // /first-party/proxy handles GET directly; HEAD is non-primary and must fall
    // through to the publisher fallback rather than returning a router-level 405.
    let router = test_router();
    let req = request_builder()
        .method("HEAD")
        .uri("/first-party/proxy")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        405,
        "HEAD /first-party/proxy must reach the publisher fallback, not 405"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/proxy")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_click_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/click")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/click must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/sign")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "GET /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_post_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/sign")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "POST /first-party/sign must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_is_routed() {
    let router = test_router();
    let req = request_builder()
        .method("POST")
        .uri("/first-party/proxy-rebuild")
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from("{}"))
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "/first-party/proxy-rebuild must be routed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_rebuild_get_is_routed() {
    // The opaque-origin creative click guard recovers via GET navigation, so the
    // route must be registered for GET and must not fall through to the
    // publisher origin. This asserts routing only; the 302 and its rebuilt
    // Location are covered by `proxy_rebuild_get_with_origin_form_uri_redirects`
    // in the core crate, which can sign a real `tsclick`.
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/proxy-rebuild?tsclick=%2Ffirst-party%2Fclick%3Ftsurl%3Dhttps%253A%252F%252Fexample.com")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_ne!(
        resp.status().as_u16(),
        404,
        "GET /first-party/proxy-rebuild must be routed"
    );
}

// ---------------------------------------------------------------------------
// First-party absolute-URI regression — Spin delivers a path-only request URI
// (built from IncomingRequest::path_with_query), so the shared proxy/click/sign
// handlers, which parse `req.uri()` with `url::Url::parse`, would fail with
// "Invalid URL" unless the adapter rebuilds an absolute URI from spin-full-url.
// ---------------------------------------------------------------------------

/// Extract a top-level JSON string field value. The sign response only contains
/// url-encoded values (no quotes or backslash escapes), so a substring scan is
/// sufficient and avoids pulling in a JSON dependency for the test crate.
fn json_string_field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_sign_get_with_path_only_uri_signs_target() {
    // The router sees a path-only URI (as Spin produces); spin-full-url carries
    // the trusted absolute URL the adapter uses to reconstruct req.uri(). Without
    // the reconstruction, the GET sign handler cannot parse its own ?url= query
    // and returns a 400 "Invalid URL" instead of a signed href.
    let router = test_router();
    let req = request_builder()
        .method("GET")
        .uri("/first-party/sign?url=https://cdn.example/a.png")
        .header(
            "spin-full-url",
            "https://www.publisher.example/first-party/sign?url=https://cdn.example/a.png",
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = route(router, req).await;
    assert_eq!(
        resp.status().as_u16(),
        200,
        "GET /first-party/sign must parse its query from the reconstructed absolute URI"
    );
    let body = String::from_utf8(resp.into_body().into_bytes().unwrap_or_default().to_vec())
        .expect("sign response body should be UTF-8");
    assert!(
        body.contains("\"href\""),
        "sign response must contain a signed href, got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_proxy_round_trip_through_spin_router() {
    // Sign a target, then route the emitted /first-party/proxy?... href back
    // through the Spin router. With a path-only URI the proxy handler would fail
    // signed-target reconstruction with a 400 "Invalid URL"; with the absolute
    // URI it validates the token and proceeds to the (native-unavailable)
    // outbound fetch, so the status is anything but the 400/404 it would be if
    // the request never passed validation/routing.
    let router = test_router();

    let sign_req = request_builder()
        .method("GET")
        .uri("/first-party/sign?url=https://cdn.example/a.png")
        .header(
            "spin-full-url",
            "https://www.publisher.example/first-party/sign?url=https://cdn.example/a.png",
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let sign_resp = route(router, sign_req).await;
    assert_eq!(
        sign_resp.status().as_u16(),
        200,
        "sign step must succeed before the proxy round-trip"
    );
    let sign_body = String::from_utf8(
        sign_resp
            .into_body()
            .into_bytes()
            .unwrap_or_default()
            .to_vec(),
    )
    .expect("sign response body should be UTF-8");
    let href = json_string_field(&sign_body, "href")
        .expect("sign response must include a signed href path");
    assert!(
        href.starts_with("/first-party/proxy?"),
        "signed href must target the proxy path, got: {href}"
    );

    let router = test_router();
    let proxy_req = request_builder()
        .method("GET")
        .uri(href.clone())
        .header(
            "spin-full-url",
            format!("https://www.publisher.example{href}"),
        )
        .body(edgezero_core::body::Body::empty())
        .expect("should build proxy request");
    let proxy_resp = route(router, proxy_req).await;
    let status = proxy_resp.status().as_u16();
    assert_ne!(
        status, 400,
        "proxy must pass signed-target validation, not fail URL parsing (400)"
    );
    assert_ne!(status, 404, "proxy path must be routed, not a route miss");
}

// ---------------------------------------------------------------------------
// Closed paths and the inspection endpoints
// ---------------------------------------------------------------------------

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

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
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

/// The file itself is the publisher path's, made by the `robots_txt`
/// middleware the entry names, so no route of this server names the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusing_every_crawler_tags_every_response_and_registers_no_route() {
    let registered: Vec<String> = router_refusing_every_crawler()
        .routes()
        .into_iter()
        .map(|route| route.path().to_string())
        .collect();
    assert!(
        !registered.iter().any(|path| path == "/robots.txt"),
        "the file is the publisher path's: {registered:?}"
    );

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
async fn a_response_carries_no_blanket_rule_without_the_section() {
    let response = route(
        test_router(),
        robots_request("GET", "/.well-known/trusted-server.json"),
    )
    .await;

    assert_eq!(robots_tag(&response), None);
}
