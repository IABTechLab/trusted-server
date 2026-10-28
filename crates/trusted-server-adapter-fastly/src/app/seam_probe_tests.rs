//! Round-trip tests for the integration seam on the Fastly adapter, driven by
//! a crate `trusted-server-core` knows nothing about.
//!
//! `trusted-server-integration-seam-probe` registers every capability the seam
//! exposes, and each test asserts an observable outcome through this adapter's
//! own router: the bytes served, the JSON a route returns, or the error a
//! startup check produces. The Axum adapter's seam tests assert the same
//! outcomes on the dev server.

use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderMap, Method, Response, request_builder};
use edgezero_core::router::RouterService;
use error_stack::Report;
use futures::executor::block_on;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::integrations::{
    IntegrationBuilder, IntegrationRegistration, IntegrationRequestFilter, RequestFilterDecision,
    RequestFilterEffects, RequestFilterInput,
};
use trusted_server_core::module_context::{ModuleCall, ModuleContext, ModuleRequest};
use trusted_server_core::robots_txt::{Contribution, RobotsTxtContributor};
use trusted_server_core::settings::Settings;
use trusted_server_core::tsjs::tsjs_script_src;
use trusted_server_core::tsjs_bundle::{JsModulePart, compile_time_parts};
use trusted_server_integration_seam_probe as seam_probe;

use super::{
    AppState, EcFinalizeState, TrustedServerApp, build_finalize_services,
    build_state_from_settings, build_state_with_registrations, register_integrations,
};

/// Largest response body these tests read, ample for the served script.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The probe's own section and table, selecting it with the country the geo
/// assertions expect.
const PROBE_BLOCK: &str = r#"
        [testing]
        modules = ["seam-probe"]

        [testing.seam-probe]
        country = "ZZ"
"#;

/// The built-in Edge Cookie module, for the tests that do not select the
/// probe's.
const HMAC_BLOCK: &str = r#"
        [ec]
        module = "hmac"

        [ec.hmac]
        passphrase = "test-secret-key-32-bytes-minimum"
"#;

/// Settings shaped like this adapter's route tests', with `extra` appended.
fn settings_with(extra: &str) -> Settings {
    // A deployment that runs an Edge Cookie module with no geo module
    // acknowledges that every request resolves at the top of the rules tree.
    // Tests that select a geo module write their own `[geo]` table.
    let geo = if extra.contains("[geo]") {
        ""
    } else {
        "
        [geo]
        assume_single_jurisdiction = true
"
    };
    Settings::from_toml(&format!(
        r#"
        [publisher]
        domain = "test-publisher.com"
        cookie_domain = ".test-publisher.com"
        origin_url = "https://origin.test-publisher.com"
        proxy_secret = "seam-probe-test-proxy-secret"

        {extra}
        {geo}
        "#
    ))
    .expect("should parse seam probe test settings")
    // A loaded domain carries its policy.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// Builds this adapter's application state with the supplied builders
/// composed in.
fn state_with(settings: Settings, integrations: &[IntegrationBuilder]) -> Arc<AppState> {
    build_state_with_registrations(settings, integrations)
        .expect("should build state from the composed builders")
}

/// Builds this adapter's router with the supplied builders composed in.
fn router_with(settings: Settings, integrations: &[IntegrationBuilder]) -> RouterService {
    TrustedServerApp::routes_for_state(&state_with(settings, integrations))
}

/// Sends one GET and returns the response. A `token` carries the probe's
/// counting header, so each test counts into an entry of its own.
fn get(router: &RouterService, path: &str, token: Option<&str>) -> Response {
    let mut builder = request_builder()
        .method(Method::GET)
        .uri(format!("https://test-publisher.com{path}"));
    if let Some(token) = token {
        builder = builder.header(seam_probe::SEAM_PROBE_COUNT_HEADER, token);
    }
    let request = builder.body(Body::empty()).expect("should build request");
    block_on(router.oneshot(request)).expect("should route request")
}

/// Reads a response body as a string.
fn body_text(response: Response) -> String {
    let bytes = block_on(response.into_body().into_bytes_bounded(MAX_BODY_BYTES))
        .expect("should read the response body");
    String::from_utf8(bytes.to_vec()).expect("should be UTF-8")
}

/// The parts the unified bundle is composed from when the probe is the only
/// selected module: the always-on `creative` module, then the module the
/// probe's registration carries. Composition puts core first.
fn expected_bundle_parts() -> Vec<JsModulePart> {
    let mut parts = compile_time_parts(&["creative"]);
    parts.push(JsModulePart {
        id: seam_probe::SEAM_PROBE_ID,
        source: seam_probe::PROBE_JS,
        sha256: seam_probe::PROBE_JS_SHA256,
    });
    parts
}

/// A module a crate outside core carries is served in the unified bundle,
/// under the hash composed from its content, and marked immutable when the
/// request's `?v=` matches that hash.
#[test]
fn carried_module_is_served_in_the_unified_bundle_under_its_composed_hash() {
    let router = router_with(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &[seam_probe::builder()],
    );
    let source = tsjs_script_src(&expected_bundle_parts());

    let response = get(&router, &source, None);

    assert_eq!(
        response.status().as_u16(),
        200,
        "should serve the unified bundle"
    );
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        cache_control.contains("immutable"),
        "a `?v=` matching the composed hash should be served immutable, got `{cache_control}`"
    );

    let body = body_text(response);
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

/// The probe's proxy route reports the country its own geo module resolved,
/// selected by `[geo] module`, and that its request preparer ran exactly once
/// before the route was dispatched.
#[test]
fn proxy_route_reports_the_modules_geo_and_that_the_preparer_ran() {
    let settings = settings_with(&format!(
        r#"
        [geo]
        module = "testing.seam-probe"
        {HMAC_BLOCK}
        {PROBE_BLOCK}
        "#
    ));
    let router = router_with(settings, &[seam_probe::builder()]);

    let response = get(&router, seam_probe::SEAM_PROBE_REPORT_PATH, None);

    assert_eq!(
        response.status().as_u16(),
        200,
        "the probe's proxy route should be dispatched"
    );

    let body = body_text(response);
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
        "the adapter should run the module's request preparer exactly once: {body}"
    );
}

/// `[robots-txt] modules` naming the probe serves the rules the probe gives,
/// asked for the request's host and path and never its query, because the
/// answer is held and served to every crawler.
#[test]
fn robots_txt_selection_naming_a_module_serves_the_rules_it_gives() {
    let settings = settings_with(&format!(
        r#"
        [robots-txt]
        modules = ["testing.seam-probe", "allow_all"]
        {HMAC_BLOCK}
        {PROBE_BLOCK}
        "#
    ));
    let router = router_with(settings, &[seam_probe::builder()]);

    let response = get(&router, "/robots.txt?crawler=one", None);

    assert_eq!(
        response.status().as_u16(),
        200,
        "the file should be served here and not by the origin"
    );
    assert!(
        response.headers().get("x-robots-tag").is_none(),
        "a file made from contributors puts no blanket rule on responses"
    );
    let file = body_text(response);
    let agent = seam_probe::SEAM_PROBE_ROBOTS_AGENT;
    assert!(
        file.starts_with(&format!("User-Agent: {agent}\nDisallow: /")),
        "the probe's rules should come first, as `modules` orders them: {file}"
    );
    assert!(
        file.contains("/robots.txt?\n"),
        "the probe should be asked for the path with no query: {file}"
    );
    assert!(
        !file.contains("crawler=one"),
        "nothing one crawler put in the address should reach a held answer: {file}"
    );
    assert!(
        file.ends_with("User-Agent: *\nAllow: /\n"),
        "core's own rules should follow: {file}"
    );
}

/// With `refuse_all` selected the file is the refusal, and every response
/// this adapter finalizes says not to index, the response of another route
/// included.
#[test]
fn refusing_every_crawler_tags_every_response() {
    let settings = settings_with(&format!(
        r#"
        [robots-txt]
        modules = "refuse_all"
        always_allow = ["/ads.txt"]
        {HMAC_BLOCK}
        {PROBE_BLOCK}
        "#
    ));
    let router = router_with(settings, &[seam_probe::builder()]);
    let tag = |response: &Response| {
        response
            .headers()
            .get("x-robots-tag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };

    let file = get(&router, "/robots.txt", None);

    assert_eq!(file.status().as_u16(), 200, "the refusal should be served");
    assert_eq!(tag(&file).as_deref(), Some("noindex, nofollow"));
    assert_eq!(
        body_text(file),
        "User-agent: *\nAllow: /ads.txt\nDisallow: /\n"
    );

    let other = get(&router, seam_probe::SEAM_PROBE_REPORT_PATH, None);

    assert_eq!(
        tag(&other).as_deref(),
        Some("noindex, nofollow"),
        "no route may be the one page a crawler is allowed to index"
    );
}

/// The methods this adapter registers for `/robots.txt`.
fn robots_txt_methods(router: &RouterService) -> Vec<String> {
    router
        .routes()
        .into_iter()
        .filter(|route| route.path() == "/robots.txt")
        .map(|route| route.method().to_string())
        .collect()
}

/// With no `[robots-txt]` section the path is the publisher's, and the probe
/// being on offer changes nothing. With the section the path is this
/// server's for `GET` and `HEAD`.
#[test]
fn robots_txt_is_this_servers_only_when_the_settings_carry_the_section() {
    let without = router_with(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &[seam_probe::builder()],
    );

    assert_eq!(
        robots_txt_methods(&without),
        Vec::<String>::new(),
        "without the section no route of this server names the path"
    );
    let other = get(&without, seam_probe::SEAM_PROBE_REPORT_PATH, None);
    assert!(
        other.headers().get("x-robots-tag").is_none(),
        "no blanket rule is placed on responses without the section"
    );

    let with = router_with(
        settings_with(&format!(
            r#"
            [robots-txt]
            modules = "refuse_all"
            {HMAC_BLOCK}
            {PROBE_BLOCK}
            "#
        )),
        &[seam_probe::builder()],
    );

    let methods = robots_txt_methods(&with);
    for method in ["GET", "HEAD"] {
        assert!(
            methods.iter().any(|registered| registered == method),
            "{method} /robots.txt should be answered here: {methods:?}"
        );
    }
}

/// The id of a module whose request filter answers every request with a
/// challenge, as bot protection does.
const CHALLENGE_ID: &str = "robots-txt-challenge";

/// The module name that module is selected by.
const CHALLENGE_MODULE: &str = "testing.robots-txt-challenge";

/// Answers every request with a `403` challenge.
struct Challenging;

#[async_trait::async_trait(?Send)]
impl IntegrationRequestFilter for Challenging {
    fn integration_id(&self) -> &'static str {
        CHALLENGE_ID
    }

    async fn filter_request(
        &self,
        _input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        let response = edgezero_core::http::response_builder()
            .status(403)
            .body(Body::from("a challenge"))
            .expect("should build the challenge");
        Ok(RequestFilterDecision::Respond {
            response: Box::new(response),
            effects: RequestFilterEffects::default(),
        })
    }
}

fn register_challenge(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(CHALLENGE_ID)
            .with_request_filter(Arc::new(Challenging))
            .build(),
    ))
}

fn accept_challenge(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(true)
}

/// A crawler asking for robots.txt is answered before the Edge Cookie steps
/// and the request filters, so a filter that answers every other request with
/// a challenge never stands in for the file, and no Edge Cookie state rides
/// on the answer.
#[test]
fn robots_txt_is_answered_before_the_edge_cookie_steps_and_the_filters() {
    let settings = settings_with(&format!(
        r#"
        [robots-txt]
        modules = "refuse_all"
        {HMAC_BLOCK}

        [testing]
        modules = ["robots-txt-challenge"]
        "#
    ));
    let challenge = IntegrationBuilder::new(
        CHALLENGE_ID,
        CHALLENGE_ID,
        register_challenge,
        accept_challenge,
    )
    .with_module_name(CHALLENGE_MODULE);
    let router = router_with(settings, &[challenge]);

    let other = get(&router, "/.well-known/trusted-server.json", None);
    assert_eq!(
        other.status().as_u16(),
        403,
        "the filter should answer another named route"
    );

    let file = get(&router, "/robots.txt", None);
    assert_eq!(
        file.status().as_u16(),
        200,
        "the file should be served, not the filter's challenge"
    );
    assert!(
        file.extensions().get::<EcFinalizeState>().is_none(),
        "no Edge Cookie state should ride on the file"
    );
    assert_eq!(body_text(file), "User-agent: *\nDisallow: /\n");
}

/// Sends one request with `method` and returns the response.
fn send(router: &RouterService, method: Method, path: &str) -> Response {
    let request = request_builder()
        .method(method)
        .uri(format!("https://test-publisher.com{path}"))
        .body(Body::empty())
        .expect("should build request");
    block_on(router.oneshot(request)).expect("should route request")
}

/// The value of the header `name`, as text.
fn header_text<'r>(response: &'r Response, name: &str) -> Option<&'r str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

/// A `HEAD` for the file is answered here with the headers a `GET` is given
/// and no body.
#[test]
fn a_head_of_robots_txt_is_answered_with_the_headers_and_no_body() {
    let router = router_with(
        settings_with(&format!(
            r#"
            [robots-txt]
            modules = "refuse_all"
            {HMAC_BLOCK}
            {PROBE_BLOCK}
            "#
        )),
        &[seam_probe::builder()],
    );

    let response = send(&router, Method::HEAD, "/robots.txt");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        header_text(&response, "content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        header_text(&response, "cache-control"),
        Some("public, max-age=3600")
    );
    assert!(body_text(response).is_empty(), "a HEAD carries no body");
}

/// The id of the module that answers once.
const ANSWERS_ONCE_ID: &str = "robots-txt-answers-once";

/// A robots.txt contributor that answers the first time it is asked and fails
/// every time after, whose answer is due for a refresh at once.
#[derive(Debug, Default)]
struct AnswersOnce {
    asked: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait(?Send)]
impl RobotsTxtContributor for AnswersOnce {
    fn refresh(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn fingerprint(&self) -> String {
        "answers-once".to_owned()
    }

    async fn contribute(
        &self,
        _call: ModuleCall<'_>,
    ) -> Result<Contribution, Report<TrustedServerError>> {
        if self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(Contribution::parse("User-Agent: OnceBot\nDisallow: /"))
        } else {
            Err(Report::new(TrustedServerError::Proxy {
                message: "the rules service is down".to_owned(),
            }))
        }
    }
}

fn register_answers_once(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(ANSWERS_ONCE_ID)
            .with_robots_txt_contributor(
                "robots-txt.answers-once",
                Arc::new(AnswersOnce::default()),
            )
            .build(),
    ))
}

fn accept_answers_once(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(true)
}

/// The id of the module that never answers.
const NEVER_ANSWERS_ID: &str = "robots-txt-never-answers";

/// A robots.txt contributor that fails every time it is asked.
#[derive(Debug)]
struct NeverAnswers;

#[async_trait::async_trait(?Send)]
impl RobotsTxtContributor for NeverAnswers {
    fn refresh(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn fingerprint(&self) -> String {
        "never-answers".to_owned()
    }

    async fn contribute(
        &self,
        _call: ModuleCall<'_>,
    ) -> Result<Contribution, Report<TrustedServerError>> {
        Err(Report::new(TrustedServerError::Proxy {
            message: "the rules service is down".to_owned(),
        }))
    }
}

fn register_never_answers(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(NEVER_ANSWERS_ID)
            .with_robots_txt_contributor("robots-txt.never-answers", Arc::new(NeverAnswers))
            .build(),
    ))
}

fn accept_never_answers(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(true)
}

/// A module whose answer is due for a refresh at once is asked again on the
/// next request, and when it fails the last answer it gave is served past its
/// age, because yesterday's rules are a better answer than none.
#[test]
fn a_module_that_fails_after_answering_has_its_last_answer_served() {
    let settings = settings_with(&format!(
        r#"
        [robots-txt]
        modules = ["answers-once"]
        {HMAC_BLOCK}

        [testing]
        modules = ["robots-txt-answers-once"]
        "#
    ));
    let builder = IntegrationBuilder::new(
        ANSWERS_ONCE_ID,
        ANSWERS_ONCE_ID,
        register_answers_once,
        accept_answers_once,
    )
    .with_module_name("testing.robots-txt-answers-once");
    let router = router_with(settings, &[builder]);

    let first = get(&router, "/robots.txt", None);
    assert_eq!(first.status().as_u16(), 200);
    assert_eq!(body_text(first), "User-Agent: OnceBot\nDisallow: /\n");

    let second = get(&router, "/robots.txt", None);
    assert_eq!(
        second.status().as_u16(),
        200,
        "the module fails, so the answer it gave last is served"
    );
    assert_eq!(body_text(second), "User-Agent: OnceBot\nDisallow: /\n");
}

/// A module that fails with no answer kept makes the file a `503` that tells
/// a crawler to come back in ten minutes and is not stored.
#[test]
fn with_nothing_kept_a_module_that_fails_makes_the_file_unavailable() {
    let settings = settings_with(&format!(
        r#"
        [robots-txt]
        modules = ["never-answers"]
        {HMAC_BLOCK}

        [testing]
        modules = ["robots-txt-never-answers"]
        "#
    ));
    let builder = IntegrationBuilder::new(
        NEVER_ANSWERS_ID,
        NEVER_ANSWERS_ID,
        register_never_answers,
        accept_never_answers,
    )
    .with_module_name("testing.robots-txt-never-answers");
    let router = router_with(settings, &[builder]);

    let response = get(&router, "/robots.txt", None);
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(header_text(&response, "retry-after"), Some("600"));
    assert_eq!(header_text(&response, "cache-control"), Some("no-store"));
}

/// A request prepares exactly once whether a named route or the fallback
/// serves it.
#[test]
fn a_named_route_and_the_fallback_each_prepare_the_request_exactly_once() {
    let router = router_with(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &[seam_probe::builder()],
    );

    // `/first-party/proxy` is a named route whose handler refuses a request
    // naming no target, so the request reaches the preparer and no backend.
    let named = get(&router, "/first-party/proxy", Some("fastly-named-route"));

    assert_eq!(
        named.status().as_u16(),
        502,
        "the named route should refuse a request that names no target"
    );
    assert_eq!(
        seam_probe::prepare_runs_for("fastly-named-route"),
        1,
        "a request served by a named route should prepare exactly once"
    );

    let fallback = get(
        &router,
        seam_probe::SEAM_PROBE_REPORT_PATH,
        Some("fastly-fallback-route"),
    );

    assert_eq!(
        fallback.status().as_u16(),
        200,
        "the probe's proxy route should be dispatched by the fallback"
    );
    assert_eq!(
        seam_probe::prepare_runs_for("fastly-fallback-route"),
        1,
        "a request served by the fallback should prepare exactly once"
    );
}

/// A `[[fetch]]` entry naming the probe's middleware builds this adapter's
/// state, and one naming the middleware of a module no section selects is a
/// startup error.
#[test]
fn fetch_entry_builds_state_only_when_its_middleware_is_running() {
    const ENTRY: &str = r#"
        [[fetch]]
        media_type = "text/html"
        middleware = ["testing.seam-probe"]
"#;

    let selected = settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}{ENTRY}"));
    assert!(
        build_state_with_registrations(selected, &[seam_probe::builder()]).is_ok(),
        "should build state with an entry naming a middleware the module supplies"
    );

    let unselected = settings_with(&format!("{HMAC_BLOCK}{ENTRY}"));
    let error = build_state_with_registrations(unselected, &[seam_probe::builder()])
        .err()
        .expect("should refuse to start when an entry names a middleware that does not run");

    let message = error.to_string();
    assert!(
        message.contains("[[fetch]] entry 1 names `testing.seam-probe`")
            && message.contains("`testing.seam-probe` is a module no section selects"),
        "should name the entry and say why the middleware is not running: {message}"
    );
}

/// A `[[serve]]` entry builds this adapter's state when it names a
/// middleware that runs in that phase, and is a startup error when it names
/// one that does not.
#[test]
fn serve_entry_builds_state_only_with_a_middleware_of_that_phase() {
    let settings = |name: &str| {
        settings_with(&format!(
            r#"{HMAC_BLOCK}{PROBE_BLOCK}
        [[serve]]
        media_type = "text/html"
        middleware = ["{name}"]
"#
        ))
    };

    assert!(
        build_state_with_registrations(
            settings(seam_probe::reader_middleware_name()),
            &[seam_probe::builder()]
        )
        .is_ok(),
        "should build state with a serve entry naming the module's serve middleware"
    );

    let error = build_state_with_registrations(
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

/// `[geo] module` naming a selected module that supplies no geo module is a
/// startup error, raised where this adapter builds its state.
#[test]
fn geo_selector_naming_a_module_without_a_geo_module_fails_at_startup() {
    let settings = settings_with(&format!(
        r#"
        [geo]
        module = "testing.seam-probe"
        {HMAC_BLOCK}

        [testing]
        modules = ["seam-probe"]

        [testing.seam-probe]
        country = "ZZ"
        declares_geo = false
        "#
    ));

    let error = build_state_with_registrations(settings, &[seam_probe::builder()])
        .err()
        .expect("should refuse to start when the selected module supplies no geo module");

    let message = error.to_string();
    assert!(
        message.contains("`[geo] module` names `testing.seam-probe`")
            && message.contains("is selected and supplies no geo module"),
        "should name the module and the missing capability: {message}"
    );
}

/// A second builder claiming the probe's id is rejected at startup, and the
/// error names the id and both sources.
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

    let error = build_state_with_registrations(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &extra,
    )
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

/// The probe's route at a path of its own, reached through this adapter's
/// router, and the two things a module route can meet there, being another
/// route on its method and path and a route of the adapter's own.
mod module_endpoints {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use edgezero_core::body::Body;
    use edgezero_core::http::{Method, Request, Response, request_builder};
    use edgezero_core::router::RouterService;
    use error_stack::Report;
    use futures::executor::block_on;
    use trusted_server_core::error::TrustedServerError;
    use trusted_server_core::integrations::{
        IntegrationBuilder, IntegrationBuilderFn, IntegrationEndpoint, IntegrationProxy,
        IntegrationRegistration,
    };
    use trusted_server_core::module_context::ModuleCall;
    use trusted_server_core::settings::Settings;

    use super::{
        HMAC_BLOCK, PROBE_BLOCK, body_text, build_state_with_registrations, router_with,
        seam_probe, settings_with,
    };

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

    /// Sends one request with `method` to `path`, carrying the probe's
    /// counting header when `token` is given.
    fn send(router: &RouterService, method: Method, path: &str, token: Option<&str>) -> Response {
        let mut builder = request_builder()
            .method(method)
            .uri(format!("https://test-publisher.com{path}"));
        if let Some(token) = token {
            builder = builder.header(seam_probe::SEAM_PROBE_COUNT_HEADER, token);
        }
        let request = builder.body(Body::empty()).expect("should build request");
        block_on(router.oneshot(request)).expect("should route request")
    }

    /// The response's `Content-Type`.
    fn content_type(response: &Response) -> Option<&str> {
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
    }

    /// A router with the probe selected.
    fn probe_router() -> RouterService {
        router_with(
            settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
            &[seam_probe::builder()],
        )
    }

    #[test]
    fn a_get_of_the_probe_s_own_path_answers_from_the_module() {
        let response = send(
            &probe_router(),
            Method::GET,
            seam_probe::SEAM_PROBE_OWN_PATH,
            None,
        );

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

    #[test]
    fn a_head_of_the_probe_s_own_path_answers_with_no_body() {
        let response = send(
            &probe_router(),
            Method::HEAD,
            seam_probe::SEAM_PROBE_OWN_PATH,
            None,
        );

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
    #[test]
    fn a_post_of_the_probe_s_own_path_never_reaches_the_module() {
        const TOKEN: &str = "fastly-own-path-post";

        let response = send(
            &probe_router(),
            Method::POST,
            seam_probe::SEAM_PROBE_OWN_PATH,
            Some(TOKEN),
        );

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
        let settings = settings_with(&format!(
            r#"{HMAC_BLOCK}
        [testing]
        modules = ["seam-probe", "own-module"]

        [testing.seam-probe]
        country = "ZZ"
        "#
        ));

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
    #[test]
    fn a_module_route_runs_only_where_the_adapter_answers_no_route_of_its_own() {
        let router = router_with(
            settings_with(&format!(
                r#"{HMAC_BLOCK}
        [testing]
        modules = ["own-module"]
        "#
            )),
            &[own_module(declaring_the_config_page)],
        );

        let page = send(&router, Method::GET, CONFIG_PAGE_PATH, None);
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

        let posted = send(&router, Method::POST, CONFIG_PAGE_PATH, None);
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

/// A demand source a crate outside core supplies builds this adapter's state,
/// and the same settings are refused without that crate's builder.
#[test]
fn demand_source_a_module_supplies_builds_state_only_with_its_builder() {
    let settings = || {
        settings_with(&format!(
            r#"
        [demand]
        modules = ["probe"]

        [demand.probe]
        implementation = "testing.seam-probe"
        endpoint = "https://demand.example/openrtb2/auction"
        {HMAC_BLOCK}
        "#
        ))
    };

    let built = build_state_with_registrations(settings(), &[seam_probe::builder()]);
    assert!(
        built.is_ok(),
        "the state should build with the probe's demand source selected: {:?}",
        built.err()
    );

    let error = build_state_with_registrations(settings(), &[])
        .err()
        .expect("should refuse a demand source no builder in the deployment supplies");
    let message = error.to_string();
    assert!(
        message.contains("[demand] `probe` uses implementation `testing.seam-probe`"),
        "should name the demand source and the implementation nothing supplies: {message}"
    );
}

/// This adapter compiles the auction plan with the builders it is given, so
/// an ad server a crate outside core supplies builds its state, and the same
/// settings are refused without that crate's builder.
#[test]
fn ad_server_a_module_supplies_builds_state_only_with_its_builder() {
    let settings = || {
        settings_with(&format!(
            r#"
        [ad-server]
        module = "probe"

        [ad-server.probe]
        implementation = "testing.seam-probe"
        {HMAC_BLOCK}
        "#
        ))
    };

    let built = build_state_with_registrations(settings(), &[seam_probe::builder()]);
    assert!(
        built.is_ok(),
        "the state should build with the probe's ad server selected: {:?}",
        built.err()
    );

    let error = build_state_with_registrations(settings(), &[])
        .err()
        .expect("should refuse an ad server no builder in the deployment supplies");
    let message = error.to_string();
    assert!(
        message.contains("[ad-server] `probe` uses implementation `testing.seam-probe`"),
        "should name the ad server and the implementation nothing supplies: {message}"
    );
}

/// With `[ec] module` and `[device] module` naming the probe, this adapter
/// starts and resolves the Edge Cookie and device modules the probe declares,
/// and the Edge Cookie module creates an identifier the built-in module
/// cannot make.
#[test]
fn ec_and_device_selectors_naming_a_module_resolve_the_modules_it_declares() {
    let settings = settings_with(&format!(
        r#"
        [ec]
        module = "testing.seam-probe"

        [device]
        module = "testing.seam-probe"
        {PROBE_BLOCK}
        "#
    ));

    let state = state_with(settings, &[seam_probe::builder()]);

    let device = state.registry.device_module().expect(
        "`[device] module = \"testing.seam-probe\"` should resolve the module's device module",
    );
    assert_eq!(
        device.id(),
        seam_probe::module_name(),
        "the device module should be the one the module declared"
    );

    let module = state.registry.ec_module().expect(
        "`[ec] module = \"testing.seam-probe\"` should resolve the module's Edge Cookie module",
    );
    assert_eq!(
        module.id(),
        seam_probe::module_name(),
        "the Edge Cookie module should be the one the module declared"
    );

    let services = build_finalize_services(
        &state.settings,
        Arc::clone(&state.default_kv_store),
        Arc::default(),
    );
    let request_info = OwnedRequestInfo::new("192.0.2.1".to_owned(), HeaderMap::new());
    let method = Method::GET;
    let context = ModuleContext::new(ModuleRequest::new(
        &method,
        "publisher.example",
        "https",
        "/",
    ))
    .with_evidence(&request_info)
    .with_services(&services);
    let generated =
        block_on(module.generate(context.call(module.id(), module.required_permissions())))
            .expect("the module's Edge Cookie module should generate an identifier");

    assert_eq!(
        generated.id.as_deref(),
        Some("seam-probe-192.0.2.1"),
        "the module's Edge Cookie module should derive the identifier from the request evidence"
    );
}

/// With `[device] module` naming the probe, the function this adapter's entry
/// point calls for every request classifies the request with the probe's
/// device module, and with the built-in module when the selector is unset.
#[test]
fn device_selector_naming_a_module_classifies_a_request_with_it() {
    let request = || {
        let mut request = fastly::Request::get("https://test-publisher.com/");
        request.set_header("user-agent", "Mozilla/5.0 (X11; Linux x86_64) Chrome/140.0");
        request
    };

    let selected = state_with(
        settings_with(&format!(
            r#"
        [device]
        module = "testing.seam-probe"
        {HMAC_BLOCK}
        {PROBE_BLOCK}
        "#
        )),
        &[seam_probe::builder()],
    );
    let services = build_finalize_services(
        &selected.settings,
        Arc::clone(&selected.default_kv_store),
        Arc::default(),
    );
    let signals = block_on(crate::derive_device_signals(
        &selected.settings,
        selected.registry.device_module(),
        &request(),
        &services,
    ));
    assert_eq!(
        signals.platform_class.as_deref(),
        Some("seam-probe"),
        "the probe's device module should classify the request"
    );

    let unset = state_with(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &[seam_probe::builder()],
    );
    let services = build_finalize_services(
        &unset.settings,
        Arc::clone(&unset.default_kv_store),
        Arc::default(),
    );
    let signals = block_on(crate::derive_device_signals(
        &unset.settings,
        unset.registry.device_module(),
        &request(),
        &services,
    ));
    assert_eq!(
        signals.platform_class.as_deref(),
        Some("linux"),
        "the built-in module should classify the request when the selector is unset"
    );
}

/// The builders a deployment registers through `run_with` reach the state the
/// application hooks build, which takes no builders of its own.
///
/// Registration is set once for the process, so the probe stays on offer to
/// the tests that run after this one. An offered builder runs only when the
/// settings select its module, which none of them do.
#[test]
fn registered_integrations_are_composed_into_the_state_the_hooks_build() {
    register_integrations(vec![seam_probe::builder()]);

    let state = build_state_from_settings(settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")))
        .expect("should build state with the registered builder composed in");

    assert!(
        state.registry.integration_runs(seam_probe::SEAM_PROBE_ID),
        "the registered module should run when the settings select it"
    );
}
