//! The request cycle this adapter runs for a routed request, checked through
//! its router: the request filters run before the route and their response
//! effects after the finalize headers, the route's module call carries the
//! device classification, robots.txt is answered ahead of both, and the
//! Edge Cookie is created and written for a real browser's page view.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::http::{Request, Response, StatusCode, request_builder};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::addressing::addressed_router;
use trusted_server_core::ec::device::{DeviceModule, DeviceSignals};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::integrations::{
    HeaderMutation, IntegrationBuilder, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration, IntegrationRequestFilter, RequestFilterDecision, RequestFilterEffects,
    RequestFilterInput,
};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::platform::test_support::{
    NoopConfigStore, NoopSecretStore, StubBackend, StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformError, PlatformGeo, PlatformGeoModule, PlatformHttpClient,
    RuntimeServices, UnavailableKvStore,
};
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
/// shown, so a test can tell that it ran and what it saw.
struct MarkingDevice;

impl MarkingDevice {
    fn mark(&self, request_info: &dyn RequestInfo) -> DeviceSignals {
        let mut signals = DeviceSignals::derive_ua_only(request_info.user_agent());
        signals.platform_class = Some(format!(
            "marked:{}",
            request_info.header("cookie").unwrap_or_default()
        ));
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

fn always(_settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(true)
}

fn probe_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(PROBE_ID, "adapter-tests", register_probe, always)
        .with_module_name(PROBE_MODULE)
}

fn answering_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(ANSWERING_ID, "adapter-tests", register_answering, always)
        .with_module_name(ANSWERING_MODULE)
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

/// The services of a reader in the US with an address, every request sent to
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
fn edge_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|line| line.starts_with("ts-ec="))
        .map(str::to_owned)
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
        "with no identity graph the route should create nothing"
    );
    assert_eq!(edge_cookie(&own), None);

    assert!(
        origin.recorded_request_uris().is_empty(),
        "the resolve route should be answered here, never by the origin"
    );
}

#[test]
fn startup_warns_that_this_adapter_opens_no_identity_graph() {
    let mut settings = settings_for_a_reader();
    settings.ec.ec_store = Some("ec_identity_store".to_owned());
    let origin = Arc::new(StubHttpClient::new());

    let _router = router_for(settings, &[], services_for_a_reader(&origin));

    assert!(
        super::IDENTITY_GRAPH_UNAVAILABLE_LOGGED.load(std::sync::atomic::Ordering::Relaxed),
        "startup should warn that the identity graph is unavailable"
    );
}
