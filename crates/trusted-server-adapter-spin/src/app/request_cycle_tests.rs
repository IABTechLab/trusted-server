//! The request cycle this adapter runs for a routed request, checked through
//! its router: the request filters run before the route and their response
//! effects after the finalize headers, the route's module call carries the
//! device classification, and robots.txt is answered ahead of both.

use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::http::{Request, Response, StatusCode, request_builder};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::ec::device::{DeviceModule, DeviceSignals};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::integrations::{
    HeaderMutation, IntegrationBuilder, IntegrationEndpoint, IntegrationProxy,
    IntegrationRegistration, IntegrationRequestFilter, RequestFilterDecision, RequestFilterEffects,
    RequestFilterInput,
};
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::platform::PlatformHttpClient;
use trusted_server_core::platform::test_support::{
    StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::settings::Settings;

use super::{build_router, build_state_with_registrations_and_services};

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
/// request sent to `origin`.
fn router_with(
    settings: Settings,
    builders: &[IntegrationBuilder],
    origin: &Arc<StubHttpClient>,
) -> RouterService {
    let state = build_state_with_registrations_and_services(
        settings,
        builders,
        Some(build_services_with_http_client(
            Arc::clone(origin) as Arc<dyn PlatformHttpClient>
        )),
    )
    .expect("should build the state");
    build_router(&state)
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
