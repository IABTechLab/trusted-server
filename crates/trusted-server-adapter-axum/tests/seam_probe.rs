//! Round-trip tests for the integration seam, driven from outside
//! `trusted-server-core`.
//!
//! Every capability the seam exposes is registered here by
//! `trusted-server-integration-seam-probe`, a fixture crate core knows nothing
//! about, and asserted through the Axum adapter's real router. Each test
//! turns on an observable outcome (the bytes served, the JSON a route
//! returns, the error a startup or deploy check produces) rather than on a
//! function having been called.

use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::http::{HeaderMap, Request};
use edgezero_adapter_axum::service::EdgeZeroAxumService;
use error_stack::Report;
use tower::{Service as _, ServiceExt as _};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::config::validate_settings_for_deploy_with;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistration, IntegrationRegistry,
};
use trusted_server_core::module_context::{ModuleContext, ModuleRequest};
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec,
    PlatformConfigStore, PlatformError, PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableHttpClient, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::tsjs::tsjs_script_src;
use trusted_server_core::tsjs_bundle::{JsModulePart, compile_time_parts};
use trusted_server_integration_seam_probe as seam_probe;

/// Largest response body these tests read, ample for the served script.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Settings shaped like the route tests', with `extra` appended.
///
/// The settings baked into the binary carry placeholder secrets that
/// `get_settings()` rejects by design, so every test states its own.
fn settings_with(extra: &str) -> Settings {
    // A request the geo module leaves unmatched resolves at the top of the
    // permissions.yaml rules tree, and a deployment that runs an Edge Cookie
    // module with no geo module acknowledges that explicitly. Tests that
    // select a geo module write their own `[geo]` table, so only supply one
    // when the caller has not.
    let geo = if extra.contains("[geo]") {
        String::new()
    } else {
        "
[geo]
assume_single_jurisdiction = true
"
        .to_owned()
    };
    Settings::from_toml(&format!(
        r#"
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "seam-probe-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"

            {extra}
            {geo}
        "#
    ))
    .expect("should parse seam probe test settings")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// The probe's own section and table, selecting it with the country the geo
/// assertions expect.
const PROBE_BLOCK: &str = r#"
            [testing]
            modules = ["seam-probe"]

            [testing.seam-probe]
            country = "ZZ"
"#;

/// Builds a service from the router composed with the supplied builders.
fn service_with(settings: Settings, integrations: &[IntegrationBuilder]) -> EdgeZeroAxumService {
    let router = TrustedServerApp::routes_with_registrations(settings, integrations)
        .expect("should build a router from the composed builders");
    EdgeZeroAxumService::new(router)
}

/// Builds a registry with the probe registered, over the auction plan the
/// settings compile to, the way an adapter builds one at startup.
fn registry_with_probe(settings: &Settings) -> IntegrationRegistry {
    let plan = Arc::new(compile_auction_plan(settings).expect("should compile the auction plan"));
    IntegrationRegistry::with_plan_and_registrations(settings, plan, &[seam_probe::builder()])
        .expect("should build a registry with the probe registered")
}

/// Sends one GET carrying the probe's counting header and returns the
/// response. Each caller passes its own `token` so tests running in parallel
/// count into separate entries.
async fn get_counted(
    service: &mut EdgeZeroAxumService,
    uri: &str,
    token: &str,
) -> axum::response::Response {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header(seam_probe::SEAM_PROBE_COUNT_HEADER, token)
        .body(AxumBody::empty())
        .expect("should build request");

    service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should respond")
}

/// Sends one GET and returns the response.
async fn get(service: &mut EdgeZeroAxumService, uri: &str) -> axum::response::Response {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(AxumBody::empty())
        .expect("should build request");

    service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should respond")
}

/// Reads a response body as a string.
async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY_BYTES)
        .await
        .expect("should read the response body");
    String::from_utf8(bytes.to_vec()).expect("should be UTF-8")
}

/// The parts the unified bundle is expected to be composed from when the probe
/// is the only enabled module: the always-on `creative` module, then the
/// module the probe's registration carries. `compose` puts core first.
fn expected_bundle_parts() -> Vec<JsModulePart> {
    let mut parts = compile_time_parts(&["creative"]);
    parts.push(JsModulePart {
        id: seam_probe::SEAM_PROBE_ID,
        source: seam_probe::PROBE_JS,
        sha256: seam_probe::PROBE_JS_SHA256,
    });
    parts
}

// ---------------------------------------------------------------------------
// 1. The carried module reaches the browser through the unified bundle
// ---------------------------------------------------------------------------

/// A module a crate outside core carries is served in the unified bundle,
/// under the hash composed from its content, and marked immutable when the
/// request's `?v=` matches that hash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carried_module_is_served_in_the_unified_bundle_under_its_composed_hash() {
    let mut service = service_with(settings_with(PROBE_BLOCK), &[seam_probe::builder()]);
    let source = tsjs_script_src(&expected_bundle_parts());

    let response = get(&mut service, &source).await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "should serve the unified bundle"
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=31536000, s-maxage=31536000, immutable"),
        "a `?v=` matching the composed hash should be served immutable"
    );

    let body = body_text(response).await;
    let core = JsModulePart::compile_time("core").expect("should compile core in");

    assert!(
        body.starts_with(core.source),
        "the bundle should start with core"
    );
    assert!(
        body.contains(seam_probe::PROBE_JS),
        "the bundle should carry the probe's module source"
    );
}

// ---------------------------------------------------------------------------
// 2. A proxy route sees the module's geo and the preparer's marker
// ---------------------------------------------------------------------------

/// The probe's proxy route reports the country its own geo module resolved,
/// selected by `[geo] module`, and the marker its request preparer left in
/// the request extensions.
///
/// This is the first end-to-end proof that an adapter runs registry preparers
/// on the request path.
///
/// The run count is asserted exactly, and the invariant is that a request
/// prepares exactly once: the adapter prepares at a single point that covers
/// every route, so a module's preparer sees each request one time and always
/// before routing. A preparer that appends a header, counts, or emits
/// telemetry can only be written against that guarantee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_route_reports_the_modules_geo_and_that_the_preparer_ran() {
    let settings = settings_with(&format!(
        r#"
            [geo]
            module = "testing.seam-probe"
            {PROBE_BLOCK}
        "#
    ));
    let mut service = service_with(settings, &[seam_probe::builder()]);

    let response = get(&mut service, seam_probe::SEAM_PROBE_REPORT_PATH).await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "the probe's proxy route should be dispatched"
    );

    let body = body_text(response).await;
    let report: serde_json::Value =
        serde_json::from_str(&body).expect("the route should return JSON");

    assert_eq!(
        report["module"],
        seam_probe::module_name(),
        "the route should identify the module: {body}"
    );
    assert_eq!(
        report["geo_country"], "ZZ",
        "the route should see the country the module's own geo module resolved: {body}"
    );
    assert_eq!(
        report["request_preparer_runs"], 1,
        "the adapter should have run the module's request preparer exactly once on the request path: {body}"
    );
}

/// `[shared.seam-probe]` runs the builder the probe shares, whose route
/// answers the table's marker. Without the table the builder is not built,
/// and the table with no builder reading it is refused at startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vendors_shared_table_runs_its_builder() {
    let mut with = service_with(
        settings_with(&format!(
            "[shared.seam-probe]\nmarker = \"from the shared table\"\n{PROBE_BLOCK}"
        )),
        &[seam_probe::builder(), seam_probe::shared_builder()],
    );
    let response = get(&mut with, seam_probe::SEAM_PROBE_SHARED_PATH).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "the table runs the builder"
    );
    assert_eq!(body_text(response).await, "from the shared table");

    let mut without = service_with(
        settings_with(PROBE_BLOCK),
        &[seam_probe::builder(), seam_probe::shared_builder()],
    );
    let response = get(&mut without, seam_probe::SEAM_PROBE_SHARED_PATH).await;
    assert_ne!(
        response.status().as_u16(),
        200,
        "without the table the builder is not built, so the route is not this server's"
    );

    let refused = TrustedServerApp::routes_with_registrations(
        settings_with("[shared.seam-probe]\nmarker = \"x\"\n"),
        &[seam_probe::builder()],
    )
    .err()
    .expect("nothing reads the table");
    assert!(
        format!("{refused:?}").contains("[shared.seam-probe] is read by no module"),
        "{refused:?}"
    );
}

/// With `refuse_all` selected every response this adapter finalizes says not
/// to index, and the file itself is the publisher path's, made by the
/// `robots_txt` middleware the entry names, so no route of this server names
/// the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusing_every_crawler_tags_every_response_and_registers_no_route() {
    let settings = || {
        settings_with(&format!(
            r#"
            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
            {PROBE_BLOCK}
        "#
        ))
    };
    let registered: Vec<String> =
        TrustedServerApp::routes_with_registrations(settings(), &[seam_probe::builder()])
            .expect("should build the routes")
            .routes()
            .into_iter()
            .map(|route| route.path().to_string())
            .collect();
    assert!(
        !registered.iter().any(|path| path == "/robots.txt"),
        "the file is the publisher path's: {registered:?}"
    );

    let mut service = service_with(settings(), &[seam_probe::builder()]);
    let other = get(&mut service, seam_probe::SEAM_PROBE_REPORT_PATH).await;

    assert_eq!(
        other
            .headers()
            .get("x-robots-tag")
            .and_then(|value| value.to_str().ok()),
        Some("noindex, nofollow"),
        "no route may be the one page a crawler is allowed to index"
    );
}

/// A name `[robots-txt] modules` gives that nothing the deployment runs
/// supplies stops the adapter at startup, so the file does not fail on the
/// first crawler to ask for it.
#[test]
fn robots_txt_selection_naming_a_module_that_is_not_running_fails_at_startup() {
    let settings = settings_with(
        r#"
            [robots-txt]
            modules = ["testing.seam-probe"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
        "#,
    );

    let error = TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()])
        .err()
        .expect("should refuse a contributor whose module no section selects");
    let message = format!("{error:?}");

    assert!(
        message.contains("`[robots-txt] modules` names `testing.seam-probe`"),
        "should name the selection: {message}"
    );
    assert!(
        message.contains("refuse_all") && message.contains("allow_all"),
        "should say what can be named: {message}"
    );
}

/// This adapter leaves `Forwarded` and `X-Forwarded-Host` in place, and the
/// probe's robots.txt rule names the host it was asked for, so the file
/// shows that a claimed host never reaches a contributor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forwarded_host_never_reaches_a_robots_txt_module() {
    use trusted_server_core::platform::PlatformHttpClient;
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };

    let settings = settings_with(&format!(
        r#"
            [robots-txt]
            modules = ["{module}"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
            {PROBE_BLOCK}
        "#,
        module = seam_probe::module_name(),
    ));
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        404,
        b"not found".to_vec(),
        vec![("content-type", "text/html")],
    );
    let router = TrustedServerApp::routes_with_registrations_and_services(
        settings,
        &[seam_probe::builder()],
        build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>),
    )
    .expect("should build the router with a stub origin");
    let mut service = EdgeZeroAxumService::new(router);
    let request = Request::builder()
        .method("GET")
        .uri("/robots.txt")
        .header("host", "test-publisher.example.com")
        .header("x-forwarded-host", "elsewhere.example")
        .header("forwarded", "host=elsewhere.example")
        .body(AxumBody::empty())
        .expect("should build request");

    let response = service
        .ready()
        .await
        .expect("should be ready")
        .call(request)
        .await
        .expect("should respond");
    assert_eq!(response.status().as_u16(), 200);
    let file = body_text(response).await;

    assert!(
        file.contains("Disallow: /test-publisher.example.com/robots.txt?"),
        "the probe should be asked for the host addressed: {file}"
    );
    assert!(
        !file.contains("elsewhere.example"),
        "a claimed host should never reach the file: {file}"
    );
}

/// A request prepares exactly once whether it is served by a named route or by
/// the fallback, so the invariant holds across the whole route table rather
/// than only on the path the probe's own proxy route sits on.
///
/// The count comes from the probe's counter rather than the request
/// extensions, because a named route returns a fixed response and reports
/// nothing about the request it was given.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_route_prepares_the_request_exactly_once() {
    let settings = settings_with(PROBE_BLOCK);
    let mut service = service_with(settings, &[seam_probe::builder()]);

    // `/first-party/proxy` is a named route, reached through
    // `named_route_handler`, whose handler refuses a request naming no target,
    // so the request reaches the preparer and no backend.
    let named = get_counted(&mut service, "/first-party/proxy", "named-route").await;

    assert_eq!(
        named.status().as_u16(),
        502,
        "the named route should refuse a request that names no target"
    );
    assert_eq!(
        seam_probe::prepare_runs_for("named-route"),
        1,
        "a request served by a named route should prepare exactly once"
    );

    // The probe's own proxy route is served by the fallback dispatcher.
    let fallback = get_counted(
        &mut service,
        seam_probe::SEAM_PROBE_REPORT_PATH,
        "fallback-route",
    )
    .await;

    assert_eq!(
        fallback.status().as_u16(),
        200,
        "the probe's proxy route should be dispatched by the fallback"
    );
    assert_eq!(
        seam_probe::prepare_runs_for("fallback-route"),
        1,
        "a request served by the fallback should prepare exactly once"
    );
}

/// A closed path is answered before any module's preparer runs, as the other
/// adapters answer it, so no preparer sees or answers a request to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_path_is_answered_before_any_preparer_runs() {
    let settings = settings_with(PROBE_BLOCK);
    let mut service = service_with(settings, &[seam_probe::builder()]);

    let response = get_counted(&mut service, "/_ts/admin/keys/rotate", "closed-path").await;

    assert_eq!(
        response.status().as_u16(),
        404,
        "the closed path should be answered here"
    );
    assert_eq!(
        seam_probe::prepare_runs_for("closed-path"),
        0,
        "no preparer should run for a closed path"
    );
}

// ---------------------------------------------------------------------------
// 3. Deploy validation reaches a vendor's own rule
// ---------------------------------------------------------------------------

/// `validate_settings_for_deploy_with` runs the probe's own validation, so a
/// country that breaks the probe's two-letter rule is rejected with the
/// probe's message.
#[test]
fn deploy_validation_rejects_a_violation_of_the_modules_own_rule() {
    let settings = settings_with(
        r#"
            [testing]
            modules = ["seam-probe"]

            [testing.seam-probe]
            country = "ZZZ"
        "#,
    );

    let error = validate_settings_for_deploy_with(&settings, &[seam_probe::builder()])
        .expect_err("should reject the probe's own rule violation");

    assert!(
        error
            .to_string()
            .contains(seam_probe::SEAM_PROBE_COUNTRY_MESSAGE),
        "should reject with the module's own message: {error}"
    );
    assert!(
        validate_settings_for_deploy_with(&settings, &[]).is_ok(),
        "the same settings should pass without the module's builder, so the rejection is the module's and not core's"
    );
}

// ---------------------------------------------------------------------------
// 4. A geo selector naming a module that supplies no module fails at startup
// ---------------------------------------------------------------------------

/// `[geo] module` naming a selected module that supplies no geo module is a
/// startup error, raised where the adapter builds its routes.
#[test]
fn geo_selector_naming_a_module_without_a_geo_module_fails_at_startup() {
    let settings = settings_with(
        r#"
            [geo]
            module = "testing.seam-probe"

            [testing]
            modules = ["seam-probe"]

            [testing.seam-probe]
            country = "ZZ"
            declares_geo = false
        "#,
    );

    let error = TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()])
        .err()
        .expect("should refuse to start when the selected module supplies no geo module");

    let message = error.to_string();
    assert!(
        message.contains("`[geo] module` names `testing.seam-probe`")
            && message.contains("is selected and supplies no geo module"),
        "should name the module and the missing capability: {message}"
    );
}

/// A `[[fetch]]` entry naming the probe's middleware starts the adapter, and
/// the registry runs that middleware on the paths the entry covers.
#[test]
fn fetch_entry_naming_a_modules_middleware_starts_the_adapter() {
    use trusted_server_core::middleware::{HTML_MEDIA_TYPE, MiddlewarePhase};

    let settings = || {
        settings_with(&format!(
            r#"
            {PROBE_BLOCK}

            [[fetch]]
            media_type = "text/html"
            path = "/news/"
            middleware = ["testing.seam-probe"]
            "#
        ))
    };

    let registry = registry_with_probe(&settings());
    let chain_for = |path: &str| {
        registry
            .middleware_chain(
                &settings().fetch,
                MiddlewarePhase::Fetch,
                HTML_MEDIA_TYPE,
                path,
            )
            .ids()
    };
    assert_eq!(
        chain_for("/news/today"),
        [seam_probe::module_name()],
        "should run the module's middleware under the entry's path"
    );
    assert!(
        chain_for("/sport/today").is_empty(),
        "should run nothing on a path no entry covers"
    );

    TrustedServerApp::routes_with_registrations(settings(), &[seam_probe::builder()])
        .expect("should start with an entry naming a middleware the module supplies");
}

/// A `[[serve]]` entry starts the adapter when it names a middleware that
/// runs in that phase, and is a startup error when it names one that does
/// not.
#[test]
fn serve_entry_starts_the_adapter_only_with_a_middleware_of_that_phase() {
    let settings = |name: &str| {
        settings_with(&format!(
            r#"
            {PROBE_BLOCK}

            [[serve]]
            media_type = "text/html"
            middleware = ["{name}"]
            "#
        ))
    };

    TrustedServerApp::routes_with_registrations(
        settings(seam_probe::reader_middleware_name()),
        &[seam_probe::builder()],
    )
    .expect("should start with a serve entry naming the module's serve middleware");

    let error = TrustedServerApp::routes_with_registrations(
        settings(seam_probe::module_name()),
        &[seam_probe::builder()],
    )
    .err()
    .expect("should refuse to start when a serve entry names a fetch middleware");

    let message = error.to_string();
    assert!(
        message.contains("[[serve]] entry 1 names `testing.seam-probe`")
            && message.contains("does not run in that phase. It runs in [[fetch]]"),
        "should say which phase the middleware runs in: {message}"
    );
}

/// A `[[fetch]]` entry naming the middleware of a module no section selects
/// is a startup error, raised where the adapter builds its routes.
#[test]
fn fetch_entry_naming_the_middleware_of_an_unselected_module_fails_at_startup() {
    let settings = settings_with(
        r#"
            [[fetch]]
            media_type = "text/html"
            middleware = ["testing.seam-probe"]
        "#,
    );

    let error = TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()])
        .err()
        .expect("should refuse to start when an entry names a middleware that does not run");

    let message = error.to_string();
    assert!(
        message.contains("[[fetch]] entry 1 names `testing.seam-probe`")
            && message.contains("`testing.seam-probe` is a module no section selects"),
        "should name the entry and say why the middleware is not running: {message}"
    );
}

// ---------------------------------------------------------------------------
// 5. Two builders claiming one id are rejected, naming both sources
// ---------------------------------------------------------------------------

/// A second builder claiming the probe's id is rejected at startup, and the
/// error names the id and both sources so an operator can tell which two
/// crates collided.
#[test]
fn duplicate_integration_id_is_rejected_naming_both_sources() {
    /// Source label of the builder that collides with the probe's.
    const OTHER_SOURCE: &str = "another-vendor-crate";

    fn register_nothing(
        _settings: &Settings,
    ) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
        Ok(None)
    }

    fn validate_nothing(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
        Ok(false)
    }

    let extra = [
        seam_probe::builder(),
        IntegrationBuilder::new(
            seam_probe::SEAM_PROBE_ID,
            OTHER_SOURCE,
            register_nothing,
            validate_nothing,
        ),
    ];

    let error = TrustedServerApp::routes_with_registrations(settings_with(PROBE_BLOCK), &extra)
        .err()
        .expect("should reject two builders claiming one integration id");

    let message = error.to_string();
    assert!(
        message.contains(seam_probe::SEAM_PROBE_ID)
            && message.contains(seam_probe::SEAM_PROBE_SOURCE)
            && message.contains(OTHER_SOURCE),
        "should name the id and both sources: {message}"
    );
}

// ---------------------------------------------------------------------------
// 6. A demand source and an ad server the probe supplies are known to the
//    plan and to validation
// ---------------------------------------------------------------------------

/// `[demand]` selecting the probe's demand source under a label of the
/// operator's own, with nothing else of the probe's selected.
const PROBE_DEMAND_BLOCK: &str = r#"
            [demand]
            modules = ["probe"]

            [demand.probe]
            implementation = "testing.seam-probe"
            endpoint = "https://demand.example/openrtb2/auction"
"#;

/// A demand source a crate outside core supplies starts the adapter, and the
/// same settings are refused without that crate's builder, by the adapter and
/// by deploy validation alike.
#[test]
fn demand_source_a_module_supplies_is_known_only_with_its_builder() {
    let started = TrustedServerApp::routes_with_registrations(
        settings_with(PROBE_DEMAND_BLOCK),
        &[seam_probe::builder()],
    );
    assert!(
        started.is_ok(),
        "the adapter should start with the probe's demand source selected: {:?}",
        started.err()
    );
    validate_settings_for_deploy_with(&settings_with(PROBE_DEMAND_BLOCK), &[seam_probe::builder()])
        .expect("should accept the probe's demand source when given the probe's builder");

    let error = TrustedServerApp::routes_with_registrations(settings_with(PROBE_DEMAND_BLOCK), &[])
        .err()
        .expect("should refuse a demand source no builder in the deployment supplies");
    let message = error.to_string();
    assert!(
        message.contains("[demand] `probe` uses implementation `testing.seam-probe`"),
        "should name the demand source and the implementation nothing supplies: {message}"
    );
    let error = validate_settings_for_deploy_with(&settings_with(PROBE_DEMAND_BLOCK), &[])
        .expect_err("should refuse the probe's demand source without the probe's builder");
    assert!(
        error.to_string().contains("testing.seam-probe"),
        "should name the implementation nothing supplies: {error}"
    );
}

/// `[ad-server]` selecting the probe's ad server under a label of the
/// operator's own, with nothing else of the probe's selected.
const PROBE_ADSERVER_BLOCK: &str = r#"
            [ad-server]
            module = "probe"

            [ad-server.probe]
            implementation = "testing.seam-probe"
"#;

/// The adapter compiles the auction plan with the builders it is given, so an
/// ad server a crate outside core supplies starts the adapter, and the same
/// settings are refused without that crate's builder.
#[test]
fn ad_server_a_module_supplies_starts_the_adapter_only_with_its_builder() {
    let started = TrustedServerApp::routes_with_registrations(
        settings_with(PROBE_ADSERVER_BLOCK),
        &[seam_probe::builder()],
    );
    assert!(
        started.is_ok(),
        "the adapter should start with the probe's ad server selected: {:?}",
        started.err()
    );

    let error =
        TrustedServerApp::routes_with_registrations(settings_with(PROBE_ADSERVER_BLOCK), &[])
            .err()
            .expect("should refuse an ad server no builder in the deployment supplies");
    let message = error.to_string();
    assert!(
        message.contains("[ad-server] `probe` uses implementation `testing.seam-probe`"),
        "should name the ad server and the implementation nothing supplies: {message}"
    );
}

/// Deploy validation compiles the plan with the builders it is given too, so
/// it agrees with the running adapter about the probe's ad server.
#[test]
fn deploy_validation_accepts_a_modules_ad_server_only_with_its_builder() {
    let settings = settings_with(PROBE_ADSERVER_BLOCK);

    validate_settings_for_deploy_with(&settings, &[seam_probe::builder()])
        .expect("should accept the probe's ad server when given the probe's builder");

    let error = validate_settings_for_deploy_with(&settings, &[])
        .expect_err("should refuse the probe's ad server without the probe's builder");
    assert!(
        error.to_string().contains("testing.seam-probe"),
        "should name the implementation nothing supplies: {error}"
    );
}

/// Settings selecting a module-supplied module.
///
/// These cannot use [`settings_with`], because that fixture carries the
/// deprecated `[ec] passphrase`, and configuration validation rejects the old
/// key and a module selection together.
fn settings_selecting_module(extra: &str) -> Settings {
    Settings::from_toml(&format!(
        r#"
            [publisher]
            domain = "test-publisher.example.com"
            cookie_domain = ".test-publisher.example.com"
            origin_url = "https://origin.test-publisher.example.com"
            proxy_secret = "seam-probe-test-proxy-secret"

            [geo]
            assume_single_jurisdiction = true

            {extra}

            {PROBE_BLOCK}
        "#
    ))
    .expect("should parse settings selecting a module-supplied module")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

// ---------------------------------------------------------------------------
// Identity and device declared on the registration
// ---------------------------------------------------------------------------

/// `[ec] module` naming a module resolves to that module's Edge Cookie
/// module, so identity reaches core through the integration registration
/// rather than through a second extension mechanism.
///
/// This test proves the selection resolves the module's module by its name.
/// `ec_module_generates_an_identifier_with_the_modules_prefix` then drives
/// that resolved module through `generate` and asserts the `seam-probe-`
/// prefix the built-in HMAC module can never produce, so the round trip is
/// proven there.
#[test]
fn ec_selector_naming_a_module_resolves_the_edge_cookie_module_it_declares() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            module = "testing.seam-probe"
        "#,
    );

    let registry = registry_with_probe(&settings);

    let module = registry
        .ec_module()
        .expect("`[ec] module = \"testing.seam-probe\"` should resolve the module's module");

    assert_eq!(
        module.id(),
        seam_probe::module_name(),
        "the resolved module should be the one the module declared"
    );
}

/// `[ec] module` may name a label of the operator's own, whose block names
/// the module, and the adapter starts with that module resolved. A label is
/// how a module named in full, such as the probe's, takes a settings block.
#[test]
fn ec_selector_naming_a_label_resolves_the_module_its_block_names() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            module = "probe"

            [ec.probe]
            implementation = "testing.seam-probe"
        "#,
    );

    let registry = registry_with_probe(&settings);
    assert_eq!(
        registry
            .ec_module()
            .expect("should resolve the module the label's block names")
            .id(),
        seam_probe::module_name(),
        "the resolved module should be the one the block names"
    );

    let started = TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()]);
    assert!(
        started.is_ok(),
        "the adapter should start with the module selected under a label: {:?}",
        started.err()
    );
}

/// `[ec] module` naming a module starts the adapter, because the startup
/// check is handed the module the module declared.
///
/// The selector tests around this one build the registry directly, so they
/// cannot see a startup check in the adapter that runs before the registry
/// exists and refuses a selection only a module can satisfy.
#[test]
fn ec_selector_naming_a_module_starts_the_adapter() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            module = "testing.seam-probe"
        "#,
    );

    let started = TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()]);

    assert!(
        started.is_ok(),
        "the adapter should start with a module's Edge Cookie module selected: {:?}",
        started.err()
    );
}

/// `[device] module` naming a module resolves that module's device module
/// the same way, so all three capabilities travel one route.
#[test]
fn device_selector_naming_a_module_resolves_the_device_module_it_declares() {
    let settings = settings_selecting_module(
        r#"
            [device]
            module = "testing.seam-probe"
        "#,
    );

    let registry = registry_with_probe(&settings);

    let module = registry
        .device_module()
        .expect("`[device] module = \"testing.seam-probe\"` should resolve the module's module");

    assert_eq!(
        module.id(),
        seam_probe::module_name(),
        "the resolved module should be the one the module declared"
    );
}

/// `[ec] module` naming a module resolves a module that, when driven
/// through `generate`, produces an identifier the built-in HMAC module can
/// never make.
///
/// The selection tests above prove the resolved module is the module's by
/// its name. This one drives that module end to end: it hands the resolved
/// module a request and asserts the identifier it returns starts
/// `seam-probe-`, a prefix only the module's module produces, so the
/// module's module genuinely ran and read the evidence rather than core's
/// built-in one answering.
#[tokio::test]
async fn ec_module_generates_an_identifier_with_the_modules_prefix() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            module = "testing.seam-probe"
        "#,
    );

    let registry = registry_with_probe(&settings);

    let module = registry
        .ec_module()
        .expect("`[ec] module = \"testing.seam-probe\"` should resolve the module's module");

    let request_info = OwnedRequestInfo::new("192.0.2.1".to_owned(), HeaderMap::new());
    let services = stub_services();
    let method = axum::http::Method::GET;
    let context = ModuleContext::new(ModuleRequest::new(
        &method,
        "publisher.example",
        "https",
        "/",
    ))
    .with_evidence(&request_info)
    .with_services(&services);
    let generated = module
        .generate(context.call(module.id(), module.required_permissions()))
        .await
        .expect("the module's module should generate an identifier");

    let id = generated
        .id
        .expect("the module's module should return an identifier");
    assert!(
        id.starts_with("seam-probe-"),
        "the identifier should carry the module module's prefix, got `{id}`"
    );
    assert_eq!(
        id, "seam-probe-192.0.2.1",
        "the module's module should derive the identifier from the request evidence"
    );
}

// ---------------------------------------------------------------------------
// A module's own endpoint, outside `/integrations/`
// ---------------------------------------------------------------------------

/// The probe's route at a path of its own, reached through this adapter's
/// router, and the two things a module route can meet there, being another
/// route on its method and path and a route of the adapter's own.
mod module_endpoints {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body as AxumBody;
    use axum::http::Request;
    use edgezero_adapter_axum::service::EdgeZeroAxumService;
    use error_stack::Report;
    use tower::{Service as _, ServiceExt as _};
    use trusted_server_adapter_axum::app::build_state_with_registrations;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        IntegrationBuilder, IntegrationBuilderFn, IntegrationEndpoint, IntegrationProxy,
        IntegrationRegistration,
    };
    use trusted_server_core::module_context::ModuleCall;
    use trusted_server_core::settings::Settings;

    use super::{PROBE_BLOCK, body_text, seam_probe, service_with, settings_with};

    /// The inspection page's path, which this adapter answers for `GET`.
    const CONFIG_PAGE_PATH: &str = "/_ts/config";

    /// Integration id of the test's own module.
    const OWN_MODULE_ID: &str = "own_module";

    /// The name a section selects the test's own module by.
    const OWN_MODULE_NAME: &str = "testing.own-module";

    /// Body the test's own module answers every request with.
    const OWN_MODULE_BODY: &str = "answered by the test's own module";

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
            _req: edgezero_core::http::Request,
        ) -> Result<edgezero_core::http::Response, Report<TrustedServerError>> {
            OWN_MODULE_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(edgezero_core::http::Response::new(
                edgezero_core::body::Body::from(OWN_MODULE_BODY),
            ))
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

    /// Sends one request with `method` to `uri`, carrying the probe's
    /// counting header when `token` is given.
    async fn send(
        service: &mut EdgeZeroAxumService,
        method: &str,
        uri: &str,
        token: Option<&str>,
    ) -> axum::response::Response {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(seam_probe::SEAM_PROBE_COUNT_HEADER, token);
        }
        let request = builder
            .body(AxumBody::empty())
            .expect("should build request");

        service
            .ready()
            .await
            .expect("should be ready")
            .call(request)
            .await
            .expect("should respond")
    }

    /// The response's `Content-Type`.
    fn content_type(response: &axum::response::Response) -> Option<&str> {
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_get_of_the_probe_s_own_path_answers_from_the_module() {
        let mut service = service_with(settings_with(PROBE_BLOCK), &[seam_probe::builder()]);

        let response = send(&mut service, "GET", seam_probe::SEAM_PROBE_OWN_PATH, None).await;

        assert_eq!(
            response.status().as_u16(),
            200,
            "the module's route should answer"
        );
        assert_eq!(content_type(&response), Some("text/plain"));
        assert_eq!(
            body_text(response).await,
            seam_probe::SEAM_PROBE_OWN_BODY,
            "should carry the module's body"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_head_of_the_probe_s_own_path_answers_with_no_body() {
        let mut service = service_with(settings_with(PROBE_BLOCK), &[seam_probe::builder()]);

        let response = send(&mut service, "HEAD", seam_probe::SEAM_PROBE_OWN_PATH, None).await;

        assert_eq!(
            response.status().as_u16(),
            200,
            "the module's HEAD route should answer"
        );
        assert_eq!(content_type(&response), Some("text/plain"));
        assert_eq!(body_text(response).await, "", "should carry no body");
    }

    /// The probe declares no `POST` on its own path, so a `POST` there goes
    /// to the publisher path and the module's handler never runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_post_of_the_probe_s_own_path_never_reaches_the_module() {
        const TOKEN: &str = "axum-own-path-post";
        let mut service = service_with(settings_with(PROBE_BLOCK), &[seam_probe::builder()]);

        let response = send(
            &mut service,
            "POST",
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
            body_text(response).await,
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
        let settings = settings_with(
            r#"
            [testing]
            modules = ["own-module"]
            "#,
        );
        let mut service = service_with(settings, &[own_module(declaring_the_config_page)]);

        let page = send(&mut service, "GET", CONFIG_PAGE_PATH, None).await;
        assert_eq!(
            page.status().as_u16(),
            200,
            "the inspection page should answer"
        );
        assert_eq!(content_type(&page), Some("text/html; charset=utf-8"));
        assert_ne!(body_text(page).await, OWN_MODULE_BODY);
        assert_eq!(
            OWN_MODULE_RUNS.load(Ordering::SeqCst),
            0,
            "the module's GET route should never run"
        );

        let posted = send(&mut service, "POST", CONFIG_PAGE_PATH, None).await;
        assert_eq!(
            posted.status().as_u16(),
            200,
            "the module's POST route should answer"
        );
        assert_eq!(body_text(posted).await, OWN_MODULE_BODY);
        assert_eq!(
            OWN_MODULE_RUNS.load(Ordering::SeqCst),
            1,
            "the module's POST route should run once"
        );
    }
}

/// Config store that answers nothing, so a test can build [`RuntimeServices`]
/// without a real platform.
///
/// The probe's Edge Cookie module derives its identifier from the request
/// evidence and reads none of the services, so the store is never queried.
struct StubConfigStore;

impl PlatformConfigStore for StubConfigStore {
    fn get(&self, _store_name: &StoreName, _key: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// Secret store that answers nothing, paired with [`StubConfigStore`].
struct StubSecretStore;

impl PlatformSecretStore for StubSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// Backend manager that registers nothing, paired with the stub stores.
struct StubBackend;

impl PlatformBackend for StubBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// Builds a minimal [`RuntimeServices`] to hand a module under test. Every
/// service is a stub or a core-provided unavailable implementation because the
/// probe's Edge Cookie module reads only the request evidence.
fn stub_services() -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(StubConfigStore))
        .secret_store(Arc::new(StubSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(StubBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(DisabledGeo))
        .client_info(ClientInfo::default())
        .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
        .build()
}
