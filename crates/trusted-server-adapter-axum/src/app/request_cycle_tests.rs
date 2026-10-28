//! The request cycle this adapter runs for a routed request, checked through
//! its router: the request filters run before a page and before every named
//! route in the cycle, and their response effects after the finalize headers,
//! without making the response less private, the route's module call carries
//! the device classification, an Edge Cookie context that cannot be read
//! answers before the filters, robots.txt and the inspection endpoints are
//! answered without them, the Edge Cookie is created and written for a real
//! browser's page view, set by the resolve route for a client-side module and
//! expired on a withdrawal, and a request under an asset route is answered
//! from the route's own source, with the route's cache rule written after the
//! finalize headers, an error that no cache stores and no Edge Cookie.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, Request, Response, StatusCode, request_builder};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::addressing::addressed_router;
use trusted_server_core::ec::device::{DeviceModule, DeviceSignals};
use trusted_server_core::ec::module::{EdgeCookieModule, GeneratedEdgeCookie, ModuleCode};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::integrations::{
    HeaderMutation, IntegrationBuilder, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration, IntegrationRequestFilter, RequestFilterDecision, RequestFilterEffects,
    RequestFilterInput,
};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permissions::{Permission, PermissionSet};
use trusted_server_core::platform::test_support::{
    NoopConfigStore, NoopSecretStore, StubBackend, StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformError, PlatformGeo, PlatformGeoModule, PlatformHttpClient,
    RuntimeServices, UnavailableKvStore,
};
use trusted_server_core::publisher::{PAGE_BIDS_LEGACY_PATH, PAGE_BIDS_PATH};
use trusted_server_core::response_privacy::enforce_terminal_private_cache_privacy;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::addressing::FirstSegment;

use super::TrustedServerApp;

/// The id and module name of the probe, which registers a filter that lets
/// the request continue, a device module and a route reporting what its
/// module call was handed.
const PROBE_ID: &str = "cycle_probe";
const PROBE_MODULE: &str = "testing.cycle-probe";

/// The id and module name of a filter that answers every request.
const ANSWERING_ID: &str = "answering_filter";
const ANSWERING_MODULE: &str = "testing.answering-filter";

/// The response header both filters write as an effect.
const EFFECT_HEADER: &str = "x-cycle-probe";
/// The request header the continuing filter adds.
const FILTERED_HEADER: &str = "x-cycle-probe-filtered";
/// What the answering filter answers.
const ANSWER: &str = "answered by the filter";

/// The id and module name of the privacy probe, whose routes answer with a
/// cache policy and whose filter names a response header for each of them.
const PRIVACY_ID: &str = "privacy_probe";
const PRIVACY_MODULE: &str = "testing.privacy-probe";

/// The privacy probe's routes. The first and the last answer with core's
/// marker for a response made for one reader, the second is public, and the
/// third is private by its own policy alone. The filter names a header for
/// each but the last.
const PRIVATE_PATH: &str = "/integrations/privacy_probe/private";
const PUBLIC_PATH: &str = "/integrations/privacy_probe/public";
const UNCACHEABLE_PATH: &str = "/integrations/privacy_probe/uncacheable";
const UNFILTERED_PATH: &str = "/integrations/privacy_probe/unfiltered";

/// The policy the public route answers with, and the one the filter writes
/// on the route core marks private.
const PUBLIC_POLICY: &str = "public, max-age=600";
/// The cookie the filter sets on the public route.
const FILTER_COOKIE: &str = "probe=1; Path=/";

/// Cookies sent to robots.txt and to a module route, each found nowhere else.
const CRAWLER_COOKIE: &str = "crawler_session=robots";
const READER_COOKIE: &str = "reader_session=route";

/// The id and module name of a module whose preparer leaves the request for
/// [`SPOILED_PATH`] or [`DISCOVERY_PATH`] with a `Cookie` header the Edge
/// Cookie context cannot read.
const SPOILING_ID: &str = "spoiling_preparer";
const SPOILING_MODULE: &str = "testing.spoiling-preparer";
const SPOILED_PATH: &str = "/spoiled";

/// The discovery route, a named route in the request cycle.
const DISCOVERY_PATH: &str = "/.well-known/trusted-server.json";

/// Every `Cookie` header the marking device module was shown, by every test
/// in this file, so a test can tell whether a request's cookies reached it.
static COOKIES_SHOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn cookies_shown() -> Vec<String> {
    COOKIES_SHOWN
        .lock()
        .expect("the record of the cookies shown should be readable")
        .clone()
}

struct AnsweringFilter;

#[async_trait::async_trait(?Send)]
impl IntegrationRequestFilter for AnsweringFilter {
    fn integration_id(&self) -> &'static str {
        ANSWERING_ID
    }

    async fn filter_request(
        &self,
        _input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        let mut response = Response::new(Body::from(ANSWER));
        *response.status_mut() = StatusCode::FORBIDDEN;
        Ok(RequestFilterDecision::Respond {
            response: Box::new(response),
            effects: RequestFilterEffects {
                request_headers: Vec::new(),
                response_headers: vec![HeaderMutation::set(EFFECT_HEADER, "answered")],
            },
        })
    }
}

struct ContinuingFilter;

#[async_trait::async_trait(?Send)]
impl IntegrationRequestFilter for ContinuingFilter {
    fn integration_id(&self) -> &'static str {
        PROBE_ID
    }

    async fn filter_request(
        &self,
        _input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        Ok(RequestFilterDecision::Continue(RequestFilterEffects {
            request_headers: vec![HeaderMutation::set(FILTERED_HEADER, "1")],
            response_headers: vec![HeaderMutation::set(EFFECT_HEADER, "continued")],
        }))
    }
}

/// A device module that marks its classification with the cookies it was
/// shown, and records them, so a test can tell that it ran and what it saw.
struct MarkingDevice;

impl MarkingDevice {
    fn mark(&self, request_info: &dyn RequestInfo) -> DeviceSignals {
        let cookie = request_info.header("cookie").unwrap_or_default();
        COOKIES_SHOWN
            .lock()
            .expect("the record of the cookies shown should be writable")
            .push(cookie.to_owned());
        let mut signals = DeviceSignals::derive_ua_only(request_info.user_agent());
        signals.platform_class = Some(format!("marked:{cookie}"));
        signals
    }
}

#[async_trait::async_trait(?Send)]
impl DeviceModule for MarkingDevice {
    fn id(&self) -> &'static str {
        PROBE_MODULE
    }

    async fn detect(&self, call: ModuleCall<'_>) -> DeviceSignals {
        call.inject(self, Self::mark)
            .unwrap_or_else(|_| DeviceSignals::unknown())
    }
}

/// A route answering with the device classification its module call was
/// handed and whether the continuing filter's request header reached it.
struct ReportRoute;

impl ReportRoute {
    fn report(
        &self,
        req: Request,
        device: Option<&DeviceSignals>,
    ) -> Result<Response, Report<TrustedServerError>> {
        let (parts, _body) = req.into_parts();
        let report = serde_json::json!({
            "device": device.and_then(|signals| signals.platform_class.clone()),
            "filtered": parts
                .headers
                .get(FILTERED_HEADER)
                .and_then(|value| value.to_str().ok()),
        });
        Ok(Response::new(Body::from(report.to_string())))
    }
}

#[async_trait::async_trait(?Send)]
impl IntegrationProxy for ReportRoute {
    fn integration_name(&self) -> &'static str {
        PROBE_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/report")]
    }

    async fn handle(
        &self,
        call: ModuleCall<'_>,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::report)?
    }
}

/// A filter that names, for each of the privacy probe's routes, a response
/// header that would let a shared cache store the route's answer.
struct LooseningFilter;

#[async_trait::async_trait(?Send)]
impl IntegrationRequestFilter for LooseningFilter {
    fn integration_id(&self) -> &'static str {
        PRIVACY_ID
    }

    async fn filter_request(
        &self,
        input: RequestFilterInput<'_>,
    ) -> Result<RequestFilterDecision, Report<TrustedServerError>> {
        let response_headers = match input.request.uri().path() {
            PRIVATE_PATH => vec![HeaderMutation::set("cache-control", PUBLIC_POLICY)],
            PUBLIC_PATH => vec![HeaderMutation::append("set-cookie", FILTER_COOKIE)],
            UNCACHEABLE_PATH => vec![HeaderMutation::set("surrogate-control", "max-age=3600")],
            _ => Vec::new(),
        };
        Ok(RequestFilterDecision::Continue(RequestFilterEffects {
            request_headers: Vec::new(),
            response_headers,
        }))
    }
}

/// The privacy probe's routes, each answering with its own cache policy.
struct PrivacyRoutes;

#[async_trait::async_trait(?Send)]
impl IntegrationProxy for PrivacyRoutes {
    fn integration_name(&self) -> &'static str {
        PRIVACY_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            self.get("/private"),
            self.get("/public"),
            self.get("/uncacheable"),
            self.get("/unfiltered"),
        ]
    }

    async fn handle(
        &self,
        _call: ModuleCall<'_>,
        req: Request,
    ) -> Result<Response, Report<TrustedServerError>> {
        let path = req.uri().path();
        let policy = if path == UNCACHEABLE_PATH {
            "private, max-age=600"
        } else {
            PUBLIC_POLICY
        };
        let mut response = Response::new(Body::from("probe"));
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static(policy));
        if path == PRIVATE_PATH || path == UNFILTERED_PATH {
            enforce_terminal_private_cache_privacy(&mut response);
        }
        Ok(response)
    }
}

fn register_probe(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(PROBE_ID)
            .without_js()
            .with_request_filter(Arc::new(ContinuingFilter))
            .with_device_module(PROBE_MODULE, Arc::new(MarkingDevice))
            .with_proxy(Arc::new(ReportRoute))
            .build(),
    ))
}

fn register_answering(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(ANSWERING_ID)
            .without_js()
            .with_request_filter(Arc::new(AnsweringFilter))
            .build(),
    ))
}

fn register_privacy(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(PRIVACY_ID)
            .without_js()
            .with_request_filter(Arc::new(LooseningFilter))
            .with_proxy(Arc::new(PrivacyRoutes))
            .build(),
    ))
}

fn register_nothing(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(None)
}

/// Leaves the request for [`SPOILED_PATH`], a page, or [`DISCOVERY_PATH`], a
/// named route, with an unreadable `Cookie` header. A deployment's own
/// modules prepare a request after the modules the adapter links, so this
/// runs after every one of them.
fn spoil_cookie(
    _settings: &Settings,
    request: &mut Request,
) -> Result<(), Report<TrustedServerError>> {
    if matches!(request.uri().path(), SPOILED_PATH | DISCOVERY_PATH) {
        request.headers_mut().insert("cookie", unreadable_cookie());
    }
    Ok(())
}

fn always(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(true)
}

fn never(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(false)
}

fn probe_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(PROBE_ID, "adapter-tests", register_probe, always)
        .with_module_name(PROBE_MODULE)
}

fn answering_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(ANSWERING_ID, "adapter-tests", register_answering, always)
        .with_module_name(ANSWERING_MODULE)
}

fn privacy_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(PRIVACY_ID, "adapter-tests", register_privacy, always)
        .with_module_name(PRIVACY_MODULE)
}

fn spoiling_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(SPOILING_ID, "adapter-tests", register_nothing, never)
        .with_module_name(SPOILING_MODULE)
        .with_request_preparer(spoil_cookie)
}

/// Settings for the publisher the requests are for, with `extra` appended.
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
            proxy_secret = "request-cycle-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true

            {extra}
        "#
    ))
    .expect("should parse the request cycle test settings")
    // A loaded domain carries its policy, and these tests decide under the
    // repository's sample.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

/// The router built through the composition root with `builders`, every
/// request handed `services`.
fn router_for(
    settings: Settings,
    builders: &[IntegrationBuilder],
    services: RuntimeServices,
) -> RouterService {
    TrustedServerApp::routes_with_registrations_and_services(settings, builders, services)
        .expect("should build the router")
}

/// The router built through the composition root with `builders`, every
/// request sent to `origin`.
fn router_with(
    settings: Settings,
    builders: &[IntegrationBuilder],
    origin: &Arc<StubHttpClient>,
) -> RouterService {
    router_for(
        settings,
        builders,
        build_services_with_http_client(Arc::clone(origin) as Arc<dyn PlatformHttpClient>),
    )
}

fn get(path: &str, cookie: Option<&str>) -> Request {
    let mut builder = request_builder()
        .method("GET")
        .uri(format!("https://test-publisher.example.com{path}"))
        .header("host", "test-publisher.example.com");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder
        .body(Body::empty())
        .expect("should build the request")
}

/// A request for `path` by `method`, with no cookie.
fn request_for(method: &Method, path: &str) -> Request {
    request_builder()
        .method(method.clone())
        .uri(format!("https://test-publisher.example.com{path}"))
        .header("host", "test-publisher.example.com")
        .body(Body::empty())
        .expect("should build the request")
}

/// Every named route the request cycle runs, by method and path.
fn named_routes_in_the_cycle() -> [(Method, &'static str); 13] {
    [
        (Method::GET, DISCOVERY_PATH),
        (Method::POST, "/verify-signature"),
        (Method::POST, "/auction"),
        (Method::GET, PAGE_BIDS_PATH),
        (Method::OPTIONS, PAGE_BIDS_PATH),
        (Method::GET, PAGE_BIDS_LEGACY_PATH),
        (Method::OPTIONS, PAGE_BIDS_LEGACY_PATH),
        (Method::GET, "/first-party/proxy"),
        (Method::GET, "/first-party/click"),
        (Method::GET, "/first-party/sign"),
        (Method::POST, "/first-party/sign"),
        (Method::GET, "/first-party/proxy-rebuild"),
        (Method::POST, "/first-party/proxy-rebuild"),
    ]
}

/// A `Cookie` header holding an accented letter in UTF-8, which a header
/// value read as text refuses.
fn unreadable_cookie() -> HeaderValue {
    HeaderValue::from_bytes(b"session=caf\xc3\xa9").expect("should build the header")
}

/// A `GET` of `path` from the browser `user_agent` names.
fn get_from(path: &str, user_agent: &'static str) -> Request {
    let mut request = get(path, None);
    request
        .headers_mut()
        .insert("user-agent", HeaderValue::from_static(user_agent));
    request
}

async fn route(router: &RouterService, request: Request) -> Response {
    router
        .oneshot(request)
        .await
        .expect("should route the request")
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn text(response: Response) -> String {
    let bytes = response
        .into_body()
        .into_bytes()
        .expect("should hold the body in memory");
    String::from_utf8(bytes.to_vec()).expect("the body should be text")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_that_answers_stands_in_for_the_page() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"answering-filter\"]"),
        &[answering_builder()],
        &origin,
    );

    let response = route(&router, get("/article", None)).await;

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "the filter's answer should stand in for the page"
    );
    assert_eq!(
        header(&response, EFFECT_HEADER),
        Some("answered"),
        "the effects the answering filter left should be applied"
    );
    assert_eq!(text(response), ANSWER);
    assert!(
        origin.recorded_request_uris().is_empty(),
        "the publisher's origin should not be asked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_that_answers_stands_in_for_every_named_route_in_the_cycle() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"answering-filter\"]"),
        &[answering_builder()],
        &origin,
    );

    for (method, path) in named_routes_in_the_cycle() {
        let response = route(&router, request_for(&method, path)).await;

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "the filter's answer should stand in for {method} {path}"
        );
        assert_eq!(
            header(&response, EFFECT_HEADER),
            Some("answered"),
            "the effects the answering filter left should be applied to {method} {path}"
        );
        assert_eq!(text(response), ANSWER, "{method} {path}");
    }
    assert!(
        origin.recorded_request_uris().is_empty(),
        "no request should be sent on, because no route runs"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_s_request_header_reaches_the_route_and_its_effect_is_written_last() {
    let origin = Arc::new(StubHttpClient::new());
    let mut settings = settings_with("[testing]\nmodules = [\"cycle-probe\"]");
    settings
        .response_headers
        .insert(EFFECT_HEADER.to_owned(), "operator".to_owned());
    let router = router_with(settings, &[probe_builder()], &origin);

    let response = route(&router, get("/integrations/cycle_probe/report", None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, EFFECT_HEADER),
        Some("continued"),
        "the filter's response effect should be written after the operator's headers"
    );
    let report: serde_json::Value =
        serde_json::from_str(&text(response)).expect("the report should be JSON");
    assert_eq!(
        report["filtered"], "1",
        "the filter's request header should reach the route"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_device_module_s_classification_reaches_the_module_call() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with(&format!(
            "[device]\nmodule = \"{PROBE_MODULE}\"\n\n[testing]\nmodules = [\"cycle-probe\"]"
        )),
        &[probe_builder()],
        &origin,
    );

    let response = route(
        &router,
        get(
            "/integrations/cycle_probe/report",
            Some("example_width=1280"),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let report: serde_json::Value =
        serde_json::from_str(&text(response)).expect("the report should be JSON");
    assert_eq!(
        report["device"], "marked:example_width=1280",
        "the selected device module should classify the request, shown its cookies"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robots_txt_is_answered_before_the_request_filters() {
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"User-agent: *\nAllow: /\n".to_vec(),
        vec![("content-type", "text/plain")],
    );
    let router = router_with(
        settings_with(
            r#"
            [testing]
            modules = ["answering-filter"]

            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
            "#,
        ),
        &[answering_builder()],
        &origin,
    );

    let response = route(&router, get("/robots.txt", None)).await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "no filter's answer should stand in for robots.txt"
    );
    assert_eq!(
        header(&response, EFFECT_HEADER),
        None,
        "the filters should not run for robots.txt"
    );
    assert_eq!(
        text(response),
        "User-agent: *\nAllow: /ads.txt\nDisallow: /\n"
    );
    assert_eq!(origin.recorded_request_methods(), ["GET"]);
}

// ---------------------------------------------------------------------------
// The Edge Cookie
// ---------------------------------------------------------------------------

/// The `User-Agent` of a desktop browser, which the bot gate passes.
const BROWSER: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                       (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// The `User-Agent` of a crawler, which the bot gate stops.
const CRAWLER: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

/// A page the publisher's origin answers.
const PAGE: &[u8] = b"<html><head></head><body>article</body></html>";

/// An Edge Cookie identifier in the shape the `hmac` module gives one.
const CARRIED_EC: &str =
    "hmac~0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.abc123";

/// A TC String that consents to no purpose, so it refuses storage, Purpose 1.
/// It is version 2 from CMP 1, in English, for vendor list 1 and no vendor,
/// last updated at time zero, so the settings that read it turn the TCF
/// module's expiry check off.
const STORAGE_REFUSING_TC_STRING: &str = "CAAAAAAAAAAAAABABAENABCAAAAAAAAAAAhoAAA";

/// A location in the US, where the sample permission rules grant the Edge
/// Cookie with no signal.
struct UsGeo;

#[async_trait::async_trait(?Send)]
impl PlatformGeo for UsGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        }))
    }
}

/// The services of a reader in the US with an address, carrying the
/// permission signal modules this adapter links, every request sent to
/// `origin`.
fn services_for_a_reader(origin: &Arc<StubHttpClient>) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(StubBackend))
        .http_client(Arc::clone(origin) as Arc<dyn PlatformHttpClient>)
        .geo(Arc::new(PlatformGeoModule::new(
            "platform",
            Arc::new(UsGeo),
        )))
        .client_info(ClientInfo {
            client_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
            ..ClientInfo::default()
        })
        .permission_signal_modules(super::shipped_signal_modules().into())
        .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
        .build()
}

/// Settings whose `[geo] module = "platform"` leaves the location the
/// reader's services give standing, where an unset selector resolves the
/// disabled module.
fn settings_for_a_reader() -> Settings {
    let mut settings = settings_with("");
    settings.geo.module = Some("platform".to_owned());
    settings
}

/// An origin answering each of `pages` requests with [`PAGE`].
fn origin_answering(pages: usize) -> Arc<StubHttpClient> {
    let origin = Arc::new(StubHttpClient::new());
    for _ in 0..pages {
        origin.push_response_with_headers(
            200,
            PAGE.to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
    }
    origin
}

/// A page navigation to `path` by a client with `user_agent`, carrying
/// `cookie` when there is one.
fn page_view(path: &str, user_agent: &str, cookie: Option<&str>) -> Request {
    let mut builder = request_builder()
        .method("GET")
        .uri(format!("https://test-publisher.example.com{path}"))
        .header("host", "test-publisher.example.com")
        .header("user-agent", user_agent)
        .header("accept", "text/html")
        .header("sec-fetch-dest", "document")
        .header("sec-fetch-mode", "navigate");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder
        .body(Body::empty())
        .expect("should build the page view")
}

/// The `Set-Cookie` line that writes the Edge Cookie, if the response has one.
/// A response writes it at most once, because core alone writes it.
fn edge_cookie(response: &Response) -> Option<String> {
    let lines: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter(|line| line.starts_with("ts-ec="))
        .map(str::to_owned)
        .collect();
    assert!(
        lines.len() <= 1,
        "the Edge Cookie should be written at most once: {lines:?}"
    );
    lines.into_iter().next()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_page_view_gets_the_edge_cookie_under_the_public_prefix_and_path() {
    let origin = origin_answering(1);
    let router = addressed_router(
        router_for(settings_for_a_reader(), &[], services_for_a_reader(&origin)),
        Arc::new(FirstSegment),
    );

    let response = route(
        &router,
        page_view("/publisher.example/article", BROWSER, None),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let cookie = edge_cookie(&response).expect("a first page view should get the Edge Cookie");
    assert!(
        cookie.contains("Path=/publisher.example"),
        "the Edge Cookie should be written under the publisher's path: {cookie}"
    );
    assert_eq!(
        origin.recorded_request_uris(),
        ["https://origin.test-publisher.example.com/article"],
        "the page should be asked for at the path the addressing continues with"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_returning_page_view_keeps_its_edge_cookie() {
    let origin = origin_answering(2);
    let router = router_for(settings_for_a_reader(), &[], services_for_a_reader(&origin));

    let first = route(&router, page_view("/article", BROWSER, None)).await;
    let written = edge_cookie(&first).expect("the first page view should get the Edge Cookie");
    assert!(
        written.contains("Path=/;") || written.ends_with("Path=/"),
        "a request addressed by host writes the Edge Cookie under the root: {written}"
    );
    let pair = written
        .split(';')
        .next()
        .expect("a Set-Cookie line starts with its pair")
        .to_owned();

    let returning = route(&router, page_view("/article", BROWSER, Some(&pair))).await;

    assert_eq!(returning.status(), StatusCode::OK);
    assert_eq!(
        edge_cookie(&returning),
        None,
        "a returning page view should keep the Edge Cookie it carries"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_s_page_view_gets_no_edge_cookie() {
    let origin = origin_answering(1);
    let router = router_for(settings_for_a_reader(), &[], services_for_a_reader(&origin));

    let response = route(&router, page_view("/article", CRAWLER, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        edge_cookie(&response),
        None,
        "the bot gate should keep a crawler from getting an Edge Cookie"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withdrawal_expires_the_edge_cookie_the_browser_carries() {
    // No `[geo]` module, so the reader resolves at the top of the sample
    // rules, the `gdpr-eu` group, where storage needs a signal, so a TC String
    // refusing storage withdraws it.
    let origin = origin_answering(1);
    let router = router_for(
        settings_with("[consent]\ncheck_expiration = false"),
        &[],
        services_for_a_reader(&origin),
    );
    let cookies = format!("ts-ec={CARRIED_EC}; euconsent-v2={STORAGE_REFUSING_TC_STRING}");

    let response = route(&router, page_view("/article", BROWSER, Some(&cookies))).await;

    assert_eq!(response.status(), StatusCode::OK);
    let expired =
        edge_cookie(&response).expect("a withdrawal should expire the Edge Cookie it carries");
    assert!(
        expired.starts_with("ts-ec=;") && expired.contains("Max-Age=0"),
        "the Edge Cookie should be expired: {expired}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_resolve_route_answers() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_for(settings_for_a_reader(), &[], services_for_a_reader(&origin));
    let resolve = |page_origin: &str| {
        request_builder()
            .method("POST")
            .uri("https://test-publisher.example.com/_ts/api/v1/ec/resolve")
            .header("host", "test-publisher.example.com")
            .header("origin", page_origin)
            .header("content-type", "text/plain")
            .header("user-agent", BROWSER)
            .body(Body::from("posted value"))
            .expect("should build the resolve request")
    };

    let foreign = route(&router, resolve("https://elsewhere.example")).await;
    assert_eq!(
        foreign.status(),
        StatusCode::FORBIDDEN,
        "a foreign Origin should be refused"
    );
    assert_eq!(header(&foreign, "cache-control"), Some("no-store"));

    let own = route(&router, resolve("https://test-publisher.example.com")).await;
    assert_eq!(
        own.status(),
        StatusCode::NO_CONTENT,
        "a server-side module creates nothing from a posted value"
    );
    assert_eq!(edge_cookie(&own), None);

    assert!(
        origin.recorded_request_uris().is_empty(),
        "the resolve route should be answered here, never by the origin"
    );
}

/// The id and module name of an Edge Cookie module that creates nothing at
/// the edge and takes its identifier from the value the page posts, as a
/// client-side module does.
const POSTED_VALUE_ID: &str = "posted_value";
const POSTED_VALUE_MODULE: &str = "testing.posted-value";

/// The value the page posts, which the module takes as its identifier.
const POSTED_VALUE: &str = "a-posted-value";

#[derive(Debug)]
struct PostedValueModule;

#[async_trait::async_trait(?Send)]
impl EdgeCookieModule for PostedValueModule {
    fn id(&self) -> &'static str {
        POSTED_VALUE_MODULE
    }

    fn code(&self) -> ModuleCode {
        trusted_server_core::module_code!("t0pv")
    }

    async fn generate(
        &self,
        _call: ModuleCall<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        Ok(GeneratedEdgeCookie::default())
    }

    async fn resolve_from_client(
        &self,
        _call: ModuleCall<'_>,
        payload: &[u8],
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        Ok(GeneratedEdgeCookie {
            id: (payload == POSTED_VALUE.as_bytes()).then(|| POSTED_VALUE.to_owned()),
            response_headers: Vec::new(),
        })
    }

    fn accepts_id(&self, value: &str) -> bool {
        value == POSTED_VALUE
    }

    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none().with(Permission::StoreOnDevice)
    }
}

fn register_posted_value(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(Some(
        IntegrationRegistration::builder(POSTED_VALUE_ID)
            .without_js()
            .with_ec_module(POSTED_VALUE_MODULE, Arc::new(PostedValueModule))
            .build(),
    ))
}

fn posted_value_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        POSTED_VALUE_ID,
        "adapter-tests",
        register_posted_value,
        always,
    )
    .with_module_name(POSTED_VALUE_MODULE)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_resolve_route_sets_a_client_side_module_s_edge_cookie() {
    let mut settings = settings_for_a_reader();
    settings.select_module("testing", POSTED_VALUE_MODULE);
    settings.ec.module = Some(POSTED_VALUE_MODULE.into());
    settings.ec.module_blocks.clear();
    let origin = Arc::new(StubHttpClient::new());
    let router = router_for(
        settings,
        &[posted_value_builder()],
        services_for_a_reader(&origin),
    );
    let resolve = request_builder()
        .method("POST")
        .uri("https://test-publisher.example.com/_ts/api/v1/ec/resolve")
        .header("host", "test-publisher.example.com")
        .header("origin", "https://test-publisher.example.com")
        .header("content-type", "text/plain")
        .header("user-agent", BROWSER)
        .body(Body::from(POSTED_VALUE))
        .expect("should build the resolve request");

    let response = route(&router, resolve).await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the module's identifier should be set with no identity graph"
    );
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    let cookie = edge_cookie(&response).expect("the resolve route should set the Edge Cookie");
    assert!(
        cookie.starts_with("ts-ec=t0pv~a-posted-value;"),
        "the Edge Cookie should carry the module's coded identifier: {cookie}"
    );
    assert!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|line| line.starts_with("ts-ecr=1;")),
        "the resolved marker should be set beside the Edge Cookie"
    );
    assert!(
        origin.recorded_request_uris().is_empty(),
        "the resolve route should be answered here, never by the origin"
    );
}

#[test]
fn startup_warns_that_this_adapter_opens_no_identity_graph() {
    // The flag is set as the warning is logged, so a deployment whose
    // `[ec] ec_store` names no store logs nothing.
    let logged = AtomicBool::new(false);
    super::warn_once_when_the_identity_graph_is_unavailable(&settings_for_a_reader(), &logged);
    assert!(
        !logged.load(Ordering::Relaxed),
        "with no ec_store there is no identity graph to warn about"
    );

    let mut settings = settings_for_a_reader();
    settings.ec.ec_store = Some("ec_identity_store".to_owned());
    let origin = Arc::new(StubHttpClient::new());

    let _router = router_for(settings, &[], services_for_a_reader(&origin));

    assert!(
        super::IDENTITY_GRAPH_UNAVAILABLE_LOGGED.load(Ordering::Relaxed),
        "startup should warn that the identity graph is unavailable"
    );
}

// ---------------------------------------------------------------------------
// Asset routes
// ---------------------------------------------------------------------------

/// Settings with an asset route under `/assets/` whose source signs its
/// requests with the stock `s3_sigv4` signer.
fn settings_with_a_signed_asset_route() -> Settings {
    settings_with(
        r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/assets/"

            [proxy.asset_routes.source]
            module = "origin"

            [proxy.asset_routes.source.origin]
            url = "https://assets.example.com"

            [proxy.asset_routes.source.origin.auth]
            type = "s3_sigv4"
            region = "us-east-1"
        "#,
    )
}

/// What the origin was asked, being each request's address and whether it
/// carried an `s3_sigv4` signature.
fn asked(origin: &StubHttpClient) -> Vec<(String, bool)> {
    origin
        .recorded_request_uris()
        .into_iter()
        .zip(origin.recorded_request_headers())
        .map(|(uri, headers)| {
            let signed = headers.iter().any(|(name, value)| {
                name == "authorization" && value.starts_with("AWS4-HMAC-SHA256 ")
            });
            (uri, signed)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_under_an_asset_route_reaches_the_route_s_origin_signed() {
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"image bytes".to_vec(),
        vec![("content-type", "image/png")],
    );
    let router = router_with(settings_with_a_signed_asset_route(), &[], &origin);

    let response = route(&router, get("/assets/logo.png", None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response), "image bytes");
    assert_eq!(
        asked(&origin),
        [(
            "https://assets.example.com/assets/logo.png".to_owned(),
            true
        )],
        "the route's origin should be asked, signed by the route's signer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_outside_the_asset_routes_reaches_the_publisher_s_origin() {
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"body { color: black; }".to_vec(),
        vec![("content-type", "text/css")],
    );
    let router = router_with(settings_with_a_signed_asset_route(), &[], &origin);

    let response = route(&router, get("/styles/site.css", None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        asked(&origin),
        [(
            "https://origin.test-publisher.example.com/styles/site.css".to_owned(),
            false
        )],
        "the publisher's origin should be asked, unsigned"
    );
}

/// Settings with an asset route under `/assets/` and a cache rule covering
/// it, which keeps an answer for a minute in a browser and an hour in a
/// shared cache.
fn settings_with_a_cached_asset_route() -> Settings {
    settings_with(
        r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/assets/"

            [proxy.asset_routes.source]
            module = "origin"

            [proxy.asset_routes.source.origin]
            url = "https://assets.example.com"

            [[cache.asset_rules]]
            id = "assets"
            enabled = true
            path_prefix = "/assets/"
            browser_ttl_seconds = 60
            edge_ttl_seconds = 3600
        "#,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_asset_route_s_cache_rule_is_written_after_the_finalize_headers() {
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"image bytes".to_vec(),
        vec![("content-type", "image/png")],
    );
    let router = router_with(settings_with_a_cached_asset_route(), &[], &origin);

    let response = route(&router, get("/assets/logo.png", None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "cache-control"),
        Some("public, max-age=60, s-maxage=3600"),
        "the rule should be written with this adapter's `s-maxage` for shared caches"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_asset_route_whose_origin_fails_answers_502_and_is_not_stored() {
    // No answer is queued, so the request to the route's origin fails.
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(settings_with_a_cached_asset_route(), &[], &origin);

    let response = route(&router, get("/assets/logo.png", None)).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        header(&response, "cache-control"),
        Some("no-store, private"),
        "an asset route's error should not be stored by any cache"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_view_under_an_asset_route_gets_no_edge_cookie() {
    let origin = origin_answering(1);
    let mut settings = settings_with_a_cached_asset_route();
    settings.geo.module = Some("platform".to_owned());
    let router = router_for(settings, &[], services_for_a_reader(&origin));

    let response = route(&router, page_view("/assets/guide.html", BROWSER, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        edge_cookie(&response),
        None,
        "an asset response skips the Edge Cookie steps, so none is written"
    );
    assert_eq!(
        origin.recorded_request_uris(),
        ["https://assets.example.com/assets/guide.html"],
        "the route's origin should answer the page view"
    );
}

#[test]
fn startup_warns_when_an_asset_route_enables_the_image_optimizer() {
    let settings = settings_with(
        r#"
            [image_optimizer.profile_sets.photos]
            default_profile = "default"

            [image_optimizer.profile_sets.photos.profiles]
            default = "width=640"

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/images/"

            [proxy.asset_routes.source]
            module = "origin"

            [proxy.asset_routes.source.origin]
            url = "https://images.example.com"

            [proxy.asset_routes.image_optimizer]
            region = "us_east"
            profile_set = "photos"
        "#,
    );
    let origin = Arc::new(StubHttpClient::new());

    let _router = router_with(settings, &[], &origin);

    assert!(
        super::IMAGE_OPTIMIZER_UNAVAILABLE_LOGGED.load(std::sync::atomic::Ordering::Relaxed),
        "startup should warn that this adapter cannot optimize the route's images"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn robots_txt_shows_the_device_module_none_of_the_crawler_s_cookies() {
    let origin = Arc::new(StubHttpClient::new());
    origin.push_response_with_headers(
        200,
        b"User-agent: *\nAllow: /\n".to_vec(),
        vec![("content-type", "text/plain")],
    );
    let router = router_with(
        settings_with(&format!(
            r#"
            [device]
            module = "{PROBE_MODULE}"

            [testing]
            modules = ["cycle-probe"]

            [robots-txt]
            modules = "refuse_all"
            always_allow = ["/ads.txt"]

            [[fetch]]
            media_type = "text/plain"
            path = "/robots.txt"
            middleware = ["robots_txt"]
            "#
        )),
        &[probe_builder()],
        &origin,
    );

    let robots = route(&router, get("/robots.txt", Some(CRAWLER_COOKIE))).await;
    let report = route(
        &router,
        get("/integrations/cycle_probe/report", Some(READER_COOKIE)),
    )
    .await;

    assert_eq!(robots.status(), StatusCode::OK);
    assert_eq!(
        header(&robots, EFFECT_HEADER),
        None,
        "the filters should not run for robots.txt"
    );
    assert_eq!(report.status(), StatusCode::OK);
    let shown = cookies_shown();
    assert!(
        shown.iter().any(|cookie| cookie == READER_COOKIE),
        "the device module should be shown the cookies of a request in the cycle"
    );
    assert!(
        !shown.iter().any(|cookie| cookie.contains(CRAWLER_COOKIE)),
        "the device module should not be shown the crawler's cookies"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_cookie_context_that_cannot_be_read_answers_before_the_filters() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"answering-filter\"]"),
        &[answering_builder(), spoiling_builder()],
        &origin,
    );

    let mut sent = get("/article", None);
    sent.headers_mut().insert("cookie", unreadable_cookie());
    let page = route(&router, sent).await;

    // A preparer the adapter links drops a `Cookie` header value it cannot
    // read as text, so a client's unreadable header never reaches the
    // context.
    assert_eq!(
        text(page),
        ANSWER,
        "a client's unreadable Cookie header should not stop the cycle"
    );
    // A page reaches the context through the fallback, and discovery through
    // the named route's handler, each once the preparers have run.
    for path in [SPOILED_PATH, DISCOVERY_PATH] {
        let spoiled = route(&router, get(path, None)).await;

        assert_eq!(
            spoiled.status(),
            StatusCode::BAD_REQUEST,
            "the context's error should answer {path}"
        );
        assert_eq!(
            header(&spoiled, EFFECT_HEADER),
            None,
            "the filters should not run for {path} once the context cannot be read"
        );
    }
    assert!(
        origin.recorded_request_uris().is_empty(),
        "the publisher's origin should not be asked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_classifies_the_request_with_the_built_in_module() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with(
            r#"
            [device]
            module = "platform"

            [testing]
            modules = ["cycle-probe"]
            "#,
        ),
        &[probe_builder()],
        &origin,
    );

    let response = route(
        &router,
        get_from(
            "/integrations/cycle_probe/report",
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)",
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let report: serde_json::Value =
        serde_json::from_str(&text(response)).expect("the report should be JSON");
    assert_eq!(
        report["device"], "ios",
        "the built-in module should classify the request from its User-Agent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_inspection_endpoints_skip_the_request_filters() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with(
            r#"
            [testing]
            modules = ["answering-filter"]
            "#,
        ),
        &[answering_builder()],
        &origin,
    );

    let page = route(&router, get("/article", None)).await;
    assert_eq!(
        text(page),
        ANSWER,
        "the filter should answer a request in the cycle"
    );
    for path in [
        "/_ts/permissions",
        "/_ts/permissions.json",
        "/_ts/config",
        "/_ts/config.json",
        "/_ts/data",
    ] {
        let response = route(&router, get(path, None)).await;
        assert_eq!(
            header(&response, EFFECT_HEADER),
            None,
            "{path} should be answered without the request filters"
        );
        assert_ne!(
            text(response),
            ANSWER,
            "{path} should be answered by its own handler"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_cannot_make_a_response_core_made_private_public() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"privacy-probe\"]"),
        &[privacy_builder()],
        &origin,
    );

    let response = route(&router, get(PRIVATE_PATH, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "cache-control"),
        Some("no-store, private"),
        "a response core marked private should stay private after a filter's header"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_s_cookie_keeps_the_response_from_shared_caches() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"privacy-probe\"]"),
        &[privacy_builder()],
        &origin,
    );

    let response = route(&router, get(PUBLIC_PATH, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "set-cookie"), Some(FILTER_COOKIE));
    assert_eq!(
        header(&response, "cache-control"),
        Some("private, max-age=0"),
        "a response a filter sets a cookie on should not be stored by a shared cache"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filter_cannot_add_a_shared_cache_header_to_a_private_response() {
    let origin = Arc::new(StubHttpClient::new());
    let router = router_with(
        settings_with("[testing]\nmodules = [\"privacy-probe\"]"),
        &[privacy_builder()],
        &origin,
    );

    let response = route(&router, get(UNCACHEABLE_PATH, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "cache-control"),
        Some("private, max-age=600")
    );
    assert_eq!(
        header(&response, "surrogate-control"),
        None,
        "a private response should carry no shared cache header a filter names"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_response_core_made_private_loses_the_validators_an_operator_writes() {
    let origin = Arc::new(StubHttpClient::new());
    let mut settings = settings_with("[testing]\nmodules = [\"privacy-probe\"]");
    for (name, value) in [
        ("etag", "\"operator\""),
        ("last-modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
    ] {
        settings
            .response_headers
            .insert(name.to_owned(), value.to_owned());
    }
    let router = router_with(settings, &[privacy_builder()], &origin);

    let response = route(&router, get(UNFILTERED_PATH, None)).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "cache-control"),
        Some("no-store, private")
    );
    for name in ["etag", "last-modified"] {
        assert_eq!(
            header(&response, name),
            None,
            "a response core marked private should lose the {name} an operator writes, \
             with no filter's header on it"
        );
    }
}
