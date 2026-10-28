//! Smoke tests for the Cloudflare adapter route wiring.
//!
//! Runs on the host target (no Workers runtime). Verifies that
//! `TrustedServerApp::routes()` builds without panicking. Does not exercise
//! the platform layer or outbound network calls.

use std::sync::Arc;

use edgezero_core::app::Hooks as _;
use edgezero_core::http::{Request, Response, request_builder};
use edgezero_core::router::RouterService;
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
use trusted_server_core::addressing::ByHostAddressing;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::addressing::{FirstSegment, status_and_type};

const LEGACY_ADMIN_DENY_METHODS: &[&str] =
    &["GET", "POST", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// Build the full application router from explicit test settings.
///
/// A Worker reads its settings from its config store when it builds its
/// application, and `TrustedServerApp::routes()`, which cannot read the store,
/// serves the startup error router on every route (and its route table is the
/// fallback-only set).
fn test_router() -> RouterService {
    TrustedServerApp::routes_with_settings(test_settings())
        .expect("should build router from test settings")
}

/// Explicit settings for the route tests.
fn test_settings() -> Settings {
    Settings::from_toml(
        r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.example.com"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
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
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// A router whose settings add one operator header, which only
/// `FinalizeResponseMiddleware` writes, so a response carrying it passed
/// through that middleware.
fn router_writing_an_operator_header() -> RouterService {
    let mut settings = test_settings();
    settings
        .response_headers
        .insert("X-Finalized".to_owned(), "1".to_owned());
    TrustedServerApp::routes_with_settings(settings)
        .expect("should build router from test settings")
}

/// The `x-geo-*` headers on `response`, by name.
fn geo_header_names(response: &Response) -> Vec<String> {
    response
        .headers()
        .keys()
        .map(|name| name.as_str().to_owned())
        .filter(|name| name.starts_with("x-geo-"))
        .collect()
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
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.example.com"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
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
// Middleware regression tests, which verify FinalizeResponseMiddleware is
// wired so it cannot be removed silently.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_middleware_writes_the_operators_headers_and_no_geo_header() {
    // The operator's header is written by FinalizeResponseMiddleware alone, so
    // its absence on a response means the middleware was not wired. No
    // x-geo-* header is written there, because a serve middleware writes them.
    let router = router_writing_an_operator_header();

    let req = request_builder()
        .method("GET")
        .uri("/.well-known/trusted-server.json")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");

    let resp = route(router, req).await;

    assert_eq!(
        resp.headers()
            .get("x-finalized")
            .and_then(|value| value.to_str().ok()),
        Some("1"),
        "FinalizeResponseMiddleware must write the operator's headers on every response"
    );
    assert!(
        geo_header_names(&resp).is_empty(),
        "no adapter writes an x-geo-* header: {:?}",
        geo_header_names(&resp)
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
    [source]
    module = "origin"

    [source.origin]
    url = "https://origin.test-publisher.example.com"

    [publisher]
    domain = "test-publisher.example.com"
    cookie_domain = ".test-publisher.example.com"
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
    use trusted_server_testing_iabtl as seam_probe;

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
        modules = ["iabtl"]

        [testing.iabtl]
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
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.example.com"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
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
            modules = ["iabtl", "own-module"]

            [testing.iabtl]
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

// ---------------------------------------------------------------------------
// robots.txt
// ---------------------------------------------------------------------------

/// The route test settings with a `[robots-txt]` section that refuses every
/// crawler and keeps one path open.
fn settings_refusing_every_crawler() -> Settings {
    Settings::from_toml(
        r#"
            [source]
            module = "origin"

            [source.origin]
            url = "https://origin.test-publisher.example.com"

            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
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
    .expect("should parse route test settings")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

fn router_refusing_every_crawler() -> RouterService {
    TrustedServerApp::routes_with_settings(settings_refusing_every_crawler())
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

/// `/robots.txt` is the publisher path's, answered against an origin that
/// answers each request in turn. A `GET` has the origin's file replaced by
/// the refusal, a `HEAD` has the file made where the origin has none and
/// gets no body, and a `POST` keeps the origin's `404`, because a file is
/// made only for a `GET` or a `HEAD`. Each carries the refusal's
/// `X-Robots-Tag`, the file's own answer included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robots_txt_is_served_through_the_publisher_path_with_its_tag() {
    use std::sync::Arc;

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
        request_builder()
            .method(method)
            .uri("https://test-publisher.example.com/robots.txt")
            .header("host", "test-publisher.example.com")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request")
    };
    let body = |response: Response| {
        let bytes = response
            .into_body()
            .into_bytes()
            .expect("should hold the answer in memory");
        String::from_utf8(bytes.to_vec()).expect("the body is text")
    };

    let get = route(router.clone(), robots_txt("GET")).await;
    assert_eq!(get.status(), 200);
    assert_eq!(robots_tag(&get).as_deref(), Some("noindex, nofollow"));
    assert_eq!(body(get), "User-agent: *\nAllow: /ads.txt\nDisallow: /\n");

    let head = route(router.clone(), robots_txt("HEAD")).await;
    assert_eq!(
        head.status(),
        200,
        "the file is made where the origin has none"
    );
    assert_eq!(robots_tag(&head).as_deref(), Some("noindex, nofollow"));
    assert_eq!(body(head), "", "a HEAD gets no body");

    let post = route(router, robots_txt("POST")).await;
    assert_eq!(
        post.status(),
        404,
        "a file is made only for a GET or a HEAD"
    );
    assert_eq!(robots_tag(&post).as_deref(), Some("noindex, nofollow"));

    assert_eq!(
        origin.recorded_request_methods(),
        ["GET", "HEAD", "POST"],
        "each request reaches the origin"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_addressing_continues_the_request_at_its_path_before_routing() {
    let router =
        TrustedServerApp::routes_with_addressing(test_settings(), &[], Arc::new(FirstSegment))
            .expect("should build router with a request addressing");

    assert_eq!(
        status_and_type(router, "/publisher.example/_ts/config.json?view=1").await,
        (200, "application/json".to_owned()),
        "the configuration endpoint should answer at the path the addressing continues with"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_addressed_by_host_is_routed_as_it_is() {
    let router =
        TrustedServerApp::routes_with_addressing(test_settings(), &[], Arc::new(ByHostAddressing))
            .expect("should build router with a request addressing");

    assert_eq!(
        status_and_type(router, "/_ts/config.json").await,
        status_and_type(test_router(), "/_ts/config.json").await,
        "should answer as the router with no addressing answers"
    );
}
