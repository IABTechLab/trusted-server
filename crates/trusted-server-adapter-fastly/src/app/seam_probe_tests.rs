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
use trusted_server_core::ec::module::IdentityInput;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::settings::Settings;
use trusted_server_core::tsjs::tsjs_script_src;
use trusted_server_core::tsjs_bundle::{JsModulePart, compile_time_parts};
use trusted_server_integration_seam_probe as seam_probe;

use super::{
    AppState, TrustedServerApp, build_finalize_services, build_state_from_settings,
    build_state_with_registrations, register_integrations,
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
        [[handlers]]
        path = "^/_ts/admin"
        username = "admin"
        password = "admin-pass"

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

/// A request prepares exactly once whether a named route or the fallback
/// serves it.
#[test]
fn a_named_route_and_the_fallback_each_prepare_the_request_exactly_once() {
    let router = router_with(
        settings_with(&format!("{HMAC_BLOCK}{PROBE_BLOCK}")),
        &[seam_probe::builder()],
    );

    // `/admin/keys/rotate` is a named route that answers with a local 404,
    // and the `^/_ts/admin` handler does not cover it, so the request reaches
    // the preparer rather than being turned back with a 401.
    let named = get(&router, "/admin/keys/rotate", Some("fastly-named-route"));

    assert_eq!(
        named.status().as_u16(),
        404,
        "the named route should serve the local deny"
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

    let services = build_finalize_services(&state.settings, Arc::clone(&state.default_kv_store));
    let request_info = OwnedRequestInfo::new("192.0.2.1".to_owned(), HeaderMap::new());
    let generated = block_on(module.generate(&request_info, &IdentityInput::default(), &services))
        .expect("the module's Edge Cookie module should generate an identifier");

    assert_eq!(
        generated.id.as_deref(),
        Some("seam-probe-192.0.2.1"),
        "the module's Edge Cookie module should derive the identifier from the request evidence"
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
