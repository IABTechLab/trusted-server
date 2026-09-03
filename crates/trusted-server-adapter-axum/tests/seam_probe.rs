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
use trusted_server_core::auction::AuctionProviderBuilder;
use trusted_server_core::config::validate_settings_for_deploy_with;
use trusted_server_core::ec::provider::IdentityInput;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::OwnedRequestInfo;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::platform::{
    ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
    PlatformError, PlatformSecretStore, RuntimeServices, StoreId, StoreName, UnavailableHttpClient,
    UnavailableKvStore,
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
    // A request the geo provider leaves unmatched resolves at the top of the
    // permissions.yaml rules tree, and a deployment that runs an Edge Cookie
    // provider with no geo provider acknowledges that explicitly. Tests that
    // select a geo provider write their own `[geo]` table, so only supply one
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
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

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
}

/// The probe's own configuration block, enabling it with the country the geo
/// assertions expect.
const PROBE_BLOCK: &str = r#"
            [integrations.seam_probe]
            country = "ZZ"
"#;

/// Builds a service from the router composed with the supplied builders.
fn service_with(
    settings: Settings,
    integrations: &[IntegrationBuilder],
    auction_providers: &[AuctionProviderBuilder],
) -> EdgeZeroAxumService {
    let router =
        TrustedServerApp::routes_with_registrations(settings, integrations, auction_providers)
            .expect("should build a router from the composed builders");
    EdgeZeroAxumService::new(router)
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
    let mut service = service_with(settings_with(PROBE_BLOCK), &[seam_probe::builder()], &[]);
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

/// The probe's proxy route reports the country its own geo provider resolved,
/// selected by `[geo] provider`, and the marker its request preparer left in
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
            provider = "seam_probe"
            {PROBE_BLOCK}
        "#
    ));
    let mut service = service_with(settings, &[seam_probe::builder()], &[]);

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
        report["module"], "seam_probe",
        "the route should identify the module: {body}"
    );
    assert_eq!(
        report["geo_country"], "ZZ",
        "the route should see the country the module's own geo provider resolved: {body}"
    );
    assert_eq!(
        report["request_preparer_runs"], 1,
        "the adapter should have run the module's request preparer exactly once on the request path: {body}"
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
    let mut service = service_with(settings, &[seam_probe::builder()], &[]);

    // `/admin/keys/rotate` is a named route (the legacy alias denied locally
    // with a 404), reached through `named_route_handler`, and it is not
    // covered by the `^/_ts/admin` auth handler, so the request reaches the
    // preparer rather than being turned back with a 401.
    let named = get_counted(&mut service, "/admin/keys/rotate", "named-route").await;

    assert_eq!(
        named.status().as_u16(),
        404,
        "the named route should serve the local deny, not fall through to the fallback"
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
            [integrations.seam_probe]
            country = "ZZZ"
        "#,
    );

    let error = validate_settings_for_deploy_with(&settings, &[seam_probe::builder()], &[])
        .expect_err("should reject the probe's own rule violation");

    assert!(
        error
            .to_string()
            .contains(seam_probe::SEAM_PROBE_COUNTRY_MESSAGE),
        "should reject with the module's own message: {error}"
    );
    assert!(
        validate_settings_for_deploy_with(&settings, &[], &[]).is_ok(),
        "the same settings should pass without the module's builder, so the rejection is the module's and not core's"
    );
}

// ---------------------------------------------------------------------------
// 4. A geo selector naming a module that supplies no provider fails at startup
// ---------------------------------------------------------------------------

/// `[geo] provider` naming an enabled module that declares no geo provider is
/// a startup error, raised where the adapter builds its routes.
#[test]
fn geo_selector_naming_a_module_without_a_provider_fails_at_startup() {
    let settings = settings_with(
        r#"
            [geo]
            provider = "seam_probe"

            [integrations.seam_probe]
            country = "ZZ"
            declares_geo = false
        "#,
    );

    let error =
        TrustedServerApp::routes_with_registrations(settings, &[seam_probe::builder()], &[])
            .err()
            .expect("should refuse to start when the selected module declares no geo provider");

    let message = error.to_string();
    assert!(
        message.contains("seam_probe") && message.contains("declares no geo provider"),
        "should name the module and the missing capability: {message}"
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

    let error =
        TrustedServerApp::routes_with_registrations(settings_with(PROBE_BLOCK), &extra, &[])
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
// 6. An outside builder's provider name satisfies `[auction] providers`
// ---------------------------------------------------------------------------

/// A provider name declared by a builder outside core satisfies
/// `[auction] providers`, and the same settings are refused when the builder
/// is not supplied, so the name is genuinely coming from the outside crate.
#[test]
fn auction_provider_name_from_an_outside_builder_satisfies_the_configured_list() {
    let auction_settings = r#"
            [auction]
            enabled = true
            providers = ["seam_probe"]
        "#;
    // The two cases differ only in whether the outside builder is supplied,
    // so the settings are built twice from the same text.
    let for_the_composed_case = settings_with(&format!("{auction_settings}{PROBE_BLOCK}"));
    let for_the_bare_case = settings_with(&format!("{auction_settings}{PROBE_BLOCK}"));

    TrustedServerApp::routes_with_registrations(
        for_the_composed_case,
        &[],
        &[seam_probe::auction_builder()],
    )
    .expect("the outside builder's provider name should satisfy [auction] providers");

    let error = TrustedServerApp::routes_with_registrations(for_the_bare_case, &[], &[])
        .err()
        .expect("should refuse a configured provider name nothing provides");

    assert!(
        error.to_string().contains("seam_probe"),
        "should name the provider nothing provides: {error}"
    );
}

/// Settings selecting a module-supplied provider.
///
/// These cannot use [`settings_with`], because that fixture carries the
/// deprecated `[ec] passphrase`, and configuration validation rejects the old
/// key and a provider selection together.
fn settings_selecting_module(extra: &str) -> Settings {
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
            proxy_secret = "seam-probe-test-proxy-secret"

            [geo]
            assume_single_jurisdiction = true

            {extra}

            {PROBE_BLOCK}
        "#
    ))
    .expect("should parse settings selecting a module-supplied provider")
}

// ---------------------------------------------------------------------------
// Identity and device declared on the registration
// ---------------------------------------------------------------------------

/// `[ec] provider` naming a module resolves to that module's Edge Cookie
/// provider, so identity reaches core through the integration registration
/// rather than through a second extension mechanism.
///
/// This test proves the selection resolves the module's provider by its id.
/// `ec_provider_generates_an_identifier_with_the_modules_prefix` then drives
/// that resolved provider through `generate` and asserts the `seam-probe-`
/// prefix the built-in HMAC provider can never produce, so the round trip is
/// proven there.
#[test]
fn ec_selector_naming_a_module_resolves_that_modules_provider() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            provider = "seam_probe"

            [ec.providers.seam_probe]
        "#,
    );

    let registry = trusted_server_core::integrations::IntegrationRegistry::with_registrations(
        &settings,
        &[seam_probe::builder()],
    )
    .expect("should build a registry with the probe registered");

    let provider = registry
        .ec_provider()
        .expect("`[ec] provider = \"seam_probe\"` should resolve the module's provider");

    assert_eq!(
        provider.id(),
        "seam_probe",
        "the resolved provider should be the one the module declared"
    );
}

/// `[device] provider` naming a module resolves that module's device provider
/// the same way, so all three capabilities travel one route.
#[test]
fn device_selector_naming_a_module_resolves_that_modules_provider() {
    let settings = settings_selecting_module(
        r#"
            [device]
            provider = "seam_probe"
        "#,
    );

    let registry = trusted_server_core::integrations::IntegrationRegistry::with_registrations(
        &settings,
        &[seam_probe::builder()],
    )
    .expect("should build a registry with the probe registered");

    let provider = registry
        .device_provider()
        .expect("`[device] provider = \"seam_probe\"` should resolve the module's provider");

    assert_eq!(
        provider.id(),
        "seam_probe",
        "the resolved provider should be the one the module declared"
    );
}

/// `[ec] provider` naming a module resolves a provider that, when driven
/// through `generate`, produces an identifier the built-in HMAC provider can
/// never make.
///
/// The selection tests above prove the resolved provider is the module's by
/// its id. This one drives that provider end to end: it hands the resolved
/// provider a request and asserts the identifier it returns starts
/// `seam-probe-`, a prefix only the module's provider produces, so the
/// module's provider genuinely ran and read the evidence rather than core's
/// built-in one answering.
#[tokio::test]
async fn ec_provider_generates_an_identifier_with_the_modules_prefix() {
    let settings = settings_selecting_module(
        r#"
            [ec]
            provider = "seam_probe"

            [ec.providers.seam_probe]
        "#,
    );

    let registry = trusted_server_core::integrations::IntegrationRegistry::with_registrations(
        &settings,
        &[seam_probe::builder()],
    )
    .expect("should build a registry with the probe registered");

    let provider = registry
        .ec_provider()
        .expect("`[ec] provider = \"seam_probe\"` should resolve the module's provider");

    let request_info = OwnedRequestInfo::new("192.0.2.1".to_owned(), HeaderMap::new());
    let generated = provider
        .generate(&request_info, &IdentityInput::default(), &stub_services())
        .await
        .expect("the module's provider should generate an identifier");

    let id = generated
        .id
        .expect("the module's provider should return an identifier");
    assert!(
        id.starts_with("seam-probe-"),
        "the identifier should carry the module provider's prefix, got `{id}`"
    );
    assert_eq!(
        id, "seam-probe-192.0.2.1",
        "the module's provider should derive the identifier from the request evidence"
    );
}

/// Config store that answers nothing, so a test can build [`RuntimeServices`]
/// without a real platform.
///
/// The probe's Edge Cookie provider derives its identifier from the request
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
    fn predict_name(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

/// Builds a minimal [`RuntimeServices`] to hand a provider under test. Every
/// service is a stub or a core-provided unavailable implementation because the
/// probe's Edge Cookie provider reads only the request evidence.
fn stub_services() -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(StubConfigStore))
        .secret_store(Arc::new(StubSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(StubBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(DisabledGeo))
        .client_info(ClientInfo::default())
        .build()
}
