//! An integration that lives outside `trusted-server-core` and exercises every
//! part of the integration seam from a vendor crate's position.
//!
//! One registration carries a browser module, a proxy route, a geo module, an
//! Edge Cookie identity module, a device module, a prefix module, a robots.txt
//! contributor and a middleware for each phase of a page, alongside its own
//! configuration block, and the builder adds a request preparer, a demand
//! implementation `[demand]` can name and an ad server implementation
//! `[ad-server]` can name. The round-trip tests in
//! `crates/trusted-server-adapter-axum/tests/seam_probe.rs` and
//! `crates/trusted-server-adapter-fastly/src/app/seam_probe_tests.rs` drive
//! each of those through a real adapter, so the seam is proven by a caller that
//! core does not know about.
//!
//! This crate is a test fixture and must never ship in a deployment. It
//! reports internal request state over an unauthenticated route, serves a
//! browser module that does nothing useful, and resolves location from static
//! configuration rather than from the request. It is a development dependency
//! of the four adapters only, so no adapter's production build reaches it.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::header::{self, HeaderValue};
use http::{Method, Request, Response};
use serde::Deserialize;
use serde_json::json;
use trusted_server_core::auction::demand::{
    AdServerImplementation, CompiledDemand, DemandFieldPolicy, DemandImplementation,
    DemandResponse, DemandTimeoutDefault, ProviderAuctionInput, RequestExtensions, accept_endpoint,
};
use trusted_server_core::auction::provider::{AuctionProvider, ProviderRequestOutcome};
use trusted_server_core::auction::types::{AuctionContext, AuctionRequest, AuctionResponse};
use trusted_server_core::ec::device::{
    DeviceAttributes, DeviceModule, DeviceSignals, HumanConfidence,
};
use trusted_server_core::ec::module::{EdgeCookieModule, GeneratedEdgeCookie, ModuleCode};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::integrations::{
    AttributeRewriteAction, CarriedJsModule, IntegrationBuilder, IntegrationEndpoint,
    IntegrationProxy, IntegrationRegistration, ScriptRewriteAction,
};
use trusted_server_core::middleware::{
    AttributeRewrite, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, TextHandler,
};
use trusted_server_core::module_context::{ModuleCall, ModuleRequest, Withheld};
use trusted_server_core::platform::{
    GeoInfo, PlatformError, PlatformGeo, PlatformGeoModule, PlatformResponse, RuntimeServices,
    ServerKeyPrefix,
};
use trusted_server_core::prefix::{PrefixModule, PublicPrefix};
use trusted_server_core::robots_txt::{Contribution, RobotsTxtContributor};
use trusted_server_core::settings::{IntegrationConfig, Settings};
use validator::Validate;

/// Integration id, which names the probe's routes and its browser module.
pub const SEAM_PROBE_ID: &str = "seam_probe";

/// The name this module is selected by, its crate folder below `crates/`, so
/// `[testing] modules = ["seam-probe"]` selects it. Its geo, Edge Cookie and
/// device modules carry the same name, so `[geo] module`, `[ec] module` and
/// `[device] module` select each of them as `testing.seam-probe`, and so does
/// `[robots-txt] modules` its robots.txt contributor.
#[must_use]
pub fn module_name() -> &'static str {
    trusted_server_core::module_name!()
}

/// The crawler the probe's robots.txt rules are addressed to.
pub const SEAM_PROBE_ROBOTS_AGENT: &str = "SeamProbeBot";

/// Source label the registry uses in duplicate-id errors.
pub const SEAM_PROBE_SOURCE: &str = "trusted-server-integration-seam-probe";

/// Path of the proxy route that reports what the seam delivered.
///
/// The route itself is built through [`IntegrationProxy::get`], so
/// `report_route_is_the_documented_path` pins this constant to the path the
/// registry actually routes.
pub const SEAM_PROBE_REPORT_PATH: &str = "/integrations/seam_probe/report";

/// Path suffix of the report route, relative to the proxy prefix.
const REPORT_ROUTE_SUFFIX: &str = "/report";

/// Path of the probe's route outside `/integrations/`, which the proxy
/// declares as written rather than beneath its prefix.
pub const SEAM_PROBE_OWN_PATH: &str = "/seam-probe/own-path";

/// Body the probe answers a `GET` of [`SEAM_PROBE_OWN_PATH`] with.
pub const SEAM_PROBE_OWN_BODY: &str = "answered by the seam probe";

/// The browser module this registration carries, built outside
/// `trusted-server-js`.
pub const PROBE_JS: &str = include_str!("../js/probe.js");

/// SHA-256 of [`PROBE_JS`], hex encoded, lower case.
///
/// The registry rejects a registration whose declared hash does not match its
/// source, so a stale literal is a startup error rather than a stale script.
/// `carried_module_hash_literal_matches_its_source` keeps this honest.
pub const PROBE_JS_SHA256: &str =
    "4711826cac0ce3df585c4a2b30f6ad5d565900a81e89f0ee00148efff2342079";

/// Message the probe's own deploy rule rejects a bad country with, so a test
/// can prove deploy validation reached a vendor's rules rather than stopping
/// at the core ones.
pub const SEAM_PROBE_COUNTRY_MESSAGE: &str =
    "`[testing.seam-probe] country` must be exactly two letters";

/// Request header a caller sets to have the preparer, and the handler of
/// [`SEAM_PROBE_OWN_PATH`], count that request.
///
/// The header's value is a token the caller chooses, so tests running in
/// parallel count into separate entries.
pub const SEAM_PROBE_COUNT_HEADER: &str = "x-seam-probe-count";

/// How many times the preparer ran for each counting token.
///
/// Request extensions die with the request, so a route that reports nothing
/// about them cannot say how many times its request was prepared. This
/// counter lets a caller pin the once-per-request invariant on any route,
/// including the named routes that return a fixed response.
static PREPARE_COUNTS: LazyLock<Mutex<BTreeMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// How many times the handler of [`SEAM_PROBE_OWN_PATH`] ran for each
/// counting token, so a caller can tell that a request never reached it.
static OWN_PATH_COUNTS: LazyLock<Mutex<BTreeMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// How many times the preparer ran for requests carrying `token` in
/// [`SEAM_PROBE_COUNT_HEADER`].
///
/// # Panics
///
/// Panics when the counter's lock is poisoned.
#[must_use]
pub fn prepare_runs_for(token: &str) -> usize {
    runs_for(&PREPARE_COUNTS, token)
}

/// How many times the handler of [`SEAM_PROBE_OWN_PATH`] ran for requests
/// carrying `token` in [`SEAM_PROBE_COUNT_HEADER`].
///
/// # Panics
///
/// Panics when the counter's lock is poisoned.
#[must_use]
pub fn own_path_runs_for(token: &str) -> usize {
    runs_for(&OWN_PATH_COUNTS, token)
}

/// The runs `counts` holds for `token`.
fn runs_for(counts: &Mutex<BTreeMap<String, usize>>, token: &str) -> usize {
    counts
        .lock()
        .expect("should lock a seam probe counter")
        .get(token)
        .copied()
        .unwrap_or(0)
}

/// Records one preparer run against the token the request carries, if any.
fn count_prepare_run(request: &Request<EdgeBody>) {
    count_run(&PREPARE_COUNTS, request);
}

/// Records one run in `counts` against the token the request carries, if
/// any.
fn count_run(counts: &Mutex<BTreeMap<String, usize>>, request: &Request<EdgeBody>) {
    let Some(token) = request
        .headers()
        .get(SEAM_PROBE_COUNT_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let mut counts = counts.lock().expect("should lock a seam probe counter");
    *counts.entry(token.to_string()).or_insert(0) += 1;
}

/// What the probe's middleware writes at the start of `<head>` when the probe
/// is set to `country`.
#[must_use]
pub fn seam_probe_head_marker(country: &str) -> String {
    format!("<meta name=\"seam-probe-country\" content=\"{country}\">")
}

/// The markup the probe's middleware writes immediately after the opening
/// body tag, so a test can see that place is reached.
#[must_use]
pub fn seam_probe_body_marker() -> String {
    "<!-- seam-probe body -->".to_owned()
}

/// The word the probe's middleware swaps for the country, in the text of a
/// script that carries `data-seam-probe`.
pub const SEAM_PROBE_SCRIPT_WORD: &str = "SEAM_PROBE_COUNTRY";

/// The probe's page change, which an entry names as the module is named.
///
/// It marks the head with the country the probe is set to, points every link
/// that carries `data-seam-probe` at the probe's report route, and writes the
/// country in place of [`SEAM_PROBE_SCRIPT_WORD`] in the text of a script
/// that carries `data-seam-probe`.
pub struct SeamProbeMiddleware {
    country: String,
    /// The head marker as written, then [`SEAM_PROBE_REPORT_PATH`].
    template_key: String,
}

impl SeamProbeMiddleware {
    fn new(country: String) -> Self {
        let template_key = seam_probe_head_marker(&country) + SEAM_PROBE_REPORT_PATH;
        Self {
            country,
            template_key,
        }
    }
}

impl Middleware for SeamProbeMiddleware {
    fn middleware_id(&self) -> &'static str {
        module_name()
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            head_inserts: vec![seam_probe_head_marker(&self.country)],
            body_open_inserts: vec![seam_probe_body_marker()],
            element_handlers: vec![Box::new(AttributeRewrite::matching(
                "a[data-seam-probe]",
                "href",
                Rc::new(|_matched| AttributeRewriteAction::replace(SEAM_PROBE_REPORT_PATH)),
            ))],
            text_handlers: vec![Box::new(CountryInScript {
                country: self.country.clone(),
                held: String::new(),
            })],
            ..MiddlewareAction::pass()
        }
    }

    /// The head marker and the report path, because their text is compiled
    /// into this crate and no setting changes when it does.
    fn template_key(&self) -> &str {
        &self.template_key
    }
}

/// The name an entry gives the probe's serve middleware, which is the
/// module's name with a part of its own after it.
#[must_use]
pub fn reader_middleware_name() -> &'static str {
    trusted_server_core::module_name!("reader")
}

/// What the probe's serve middleware writes ahead of the script bundle for a
/// reader who asked for `request_host`.
#[must_use]
pub fn seam_probe_reader_marker(request_host: &str) -> String {
    format!("<meta name=\"seam-probe-reader\" content=\"{request_host}\">")
}

/// The probe's change to one reader's copy of a page, which marks the head
/// with the host that reader asked for.
pub struct SeamProbeReaderMiddleware;

impl Middleware for SeamProbeReaderMiddleware {
    fn middleware_id(&self) -> &'static str {
        reader_middleware_name()
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            head_inserts: vec![seam_probe_reader_marker(context.request_host)],
            ..MiddlewareAction::pass()
        }
    }
}

/// Holds a script's text until its last chunk arrives, then writes the whole
/// of it back with the word swapped, because the word may lie across chunks.
struct CountryInScript {
    country: String,
    held: String,
}

impl TextHandler for CountryInScript {
    fn selector(&self) -> &str {
        "script[data-seam-probe]"
    }

    fn decide(&mut self, text: &str, is_last: bool) -> ScriptRewriteAction {
        self.held.push_str(text);
        if !is_last {
            return ScriptRewriteAction::remove_node();
        }
        ScriptRewriteAction::replace(
            std::mem::take(&mut self.held).replace(SEAM_PROBE_SCRIPT_WORD, &self.country),
        )
    }
}

/// Marker the request preparer inserts into the request extensions.
///
/// The proxy route reports [`Self::runs`], so a test observes both that the
/// preparer ran and how many times the request path ran it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeamProbePrepared {
    /// How many times the preparer ran for this request.
    pub runs: usize,
}

/// The probe's own configuration block, `[testing.seam-probe]`.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SeamProbeConfig {
    /// Country the probe's geo module resolves for every request.
    ///
    /// Rejected by [`validate`] when it is not two letters, which is the
    /// vendor-owned rule the deploy validation round trip exercises.
    #[serde(default = "default_country")]
    pub country: String,
    /// Whether the registration declares the probe's geo module.
    ///
    /// Set this to `false` to build a selected module that supplies no geo
    /// module, which is what `[geo] module = "testing.seam-probe"` must
    /// reject at startup.
    #[serde(default = "default_declares_geo")]
    pub declares_geo: bool,
}

impl IntegrationConfig for SeamProbeConfig {}

/// `ZZ` is reserved in ISO 3166-1 for private use, so it names no real place.
fn default_country() -> String {
    "ZZ".to_string()
}

const fn default_declares_geo() -> bool {
    true
}

/// Geo module that resolves the country the probe's configuration names,
/// whatever the client address is.
pub struct SeamProbeGeo {
    country: String,
}

impl SeamProbeGeo {
    /// Creates a module that resolves `country` for every request.
    #[must_use]
    pub const fn new(country: String) -> Self {
        Self { country }
    }
}

/// The probe's Edge Cookie module, declared on its registration so identity
/// reaches core the same way location does.
///
/// It derives a value from the request evidence rather than a constant, so a
/// test can tell the difference between the module's module running and
/// core's built-in one running.
#[derive(Debug)]
pub struct SeamProbeEc;

/// The four-character code core stamps on identifiers this module owns.
const SEAM_PROBE_EC_CODE: ModuleCode = trusted_server_core::module_code!("sprb");

impl SeamProbeEc {
    /// An identifier derived from the client IP in the request's evidence.
    fn derive(&self, request_info: &dyn RequestInfo) -> GeneratedEdgeCookie {
        // Derived from evidence so the test can prove this ran rather than the
        // built-in module, and prove the evidence actually arrived.
        let ip = request_info.client_ip();
        let id = format!("seam-probe-{}", if ip.is_empty() { "no-ip" } else { ip });
        GeneratedEdgeCookie {
            id: Some(id),
            response_headers: Vec::new(),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl EdgeCookieModule for SeamProbeEc {
    fn id(&self) -> &'static str {
        module_name()
    }

    fn code(&self) -> ModuleCode {
        SEAM_PROBE_EC_CODE
    }

    async fn generate(
        &self,
        call: ModuleCall<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
        Ok(call.inject(self, Self::derive).unwrap_or_default())
    }

    fn accepts_id(&self, value: &str) -> bool {
        value.starts_with("seam-probe-")
    }
}

/// The probe's device module, declared on the same registration.
#[derive(Debug)]
pub struct SeamProbeDevice;

#[async_trait::async_trait(?Send)]
impl DeviceModule for SeamProbeDevice {
    fn id(&self) -> &'static str {
        module_name()
    }

    async fn detect(&self, _call: ModuleCall<'_>) -> DeviceSignals {
        DeviceSignals {
            is_mobile: 1,
            platform_class: Some("seam-probe".to_owned()),
            known_browser: Some(true),
            looks_like_browser: true,
            ja4_class: None,
            h2_fp_hash: None,
            // A scoring module's answer, which core's own modules never give.
            human_confidence: HumanConfidence::Medium,
        }
    }

    async fn advertising_attributes(&self, _call: ModuleCall<'_>) -> Option<DeviceAttributes> {
        Some(SeamProbeDevice::attributes())
    }
}

impl SeamProbeDevice {
    /// What the probe tells a bidder about the device, fixed so a test can
    /// read it back.
    #[must_use]
    pub fn attributes() -> DeviceAttributes {
        DeviceAttributes {
            // A tablet, as the module gives `OpenRTB`'s own code.
            device_type: Some(5),
            make: Some("Probe".to_owned()),
            model: Some("P-1".to_owned()),
            os: Some("ProbeOS".to_owned()),
            os_version: Some("1.0".to_owned()),
            screen_width: Some(800),
            screen_height: Some(1280),
        }
    }
}

/// The head the probe's prefix module gives, so a test can tell its names
/// from core's own `ts`.
pub const SEAM_PROBE_PREFIX_HEAD: &str = "_probe";

/// The probe's prefix module, declared on the same registration, which gives
/// `_probe` for the names the browser holds and `probe.<publisher.domain>`
/// for the keys in a shared store.
#[derive(Debug)]
pub struct SeamProbePrefix;

impl SeamProbePrefix {
    fn server_key(&self, settings: &Settings) -> ServerKeyPrefix {
        ServerKeyPrefix::new("probe", &settings.publisher.domain)
    }
}

impl PrefixModule for SeamProbePrefix {
    fn public_prefix(&self, _call: ModuleCall<'_>) -> Result<PublicPrefix, Withheld> {
        Ok(PublicPrefix::new([SEAM_PROBE_PREFIX_HEAD]).expect("the head is a valid prefix"))
    }

    fn server_key_prefix(&self, call: ModuleCall<'_>) -> Result<ServerKeyPrefix, Withheld> {
        call.inject(self, Self::server_key)
    }
}

/// The probe's robots.txt contributor, declared on the same registration.
///
/// Its one rule names the request it was asked for, so a test can read back
/// that the contributor was handed the host and the path and not the query.
#[derive(Debug)]
pub struct SeamProbeRobotsTxt;

impl SeamProbeRobotsTxt {
    fn rules(&self, request: ModuleRequest<'_>) -> Contribution {
        Contribution::parse(&format!(
            "User-Agent: {SEAM_PROBE_ROBOTS_AGENT}\nDisallow: /{}{}?{}",
            request.host(),
            request.path(),
            request.query()
        ))
    }
}

#[async_trait::async_trait(?Send)]
impl RobotsTxtContributor for SeamProbeRobotsTxt {
    fn refresh(&self) -> core::time::Duration {
        core::time::Duration::from_secs(60)
    }

    fn fingerprint(&self) -> String {
        "seam-probe".to_owned()
    }

    async fn contribute(
        &self,
        call: ModuleCall<'_>,
    ) -> Result<Contribution, Report<TrustedServerError>> {
        Ok(call.inject(self, Self::rules)?)
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformGeo for SeamProbeGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &trusted_server_core::platform::RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo {
            city: "Example City".to_string(),
            country: self.country.clone(),
            continent: "Example Continent".to_string(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        }))
    }
}

/// The name an `[ad-server.<name>]` or `[demand.<name>]` `implementation`
/// line selects the probe's ad server or demand source by, which is the
/// probe's module name.
///
/// A static cannot call [`module_name`], so the literal is written out and
/// `auction_implementations_are_named_as_the_module` keeps it honest.
pub const SEAM_PROBE_ADSERVER_NAME: &str = "testing.seam-probe";

/// The probe's demand implementation, registered on its builder.
pub static SEAM_PROBE_DEMAND: DemandImplementation = DemandImplementation {
    id: SEAM_PROBE_ADSERVER_NAME,
    default_timeout: DemandTimeoutDefault::Auction,
    allows_all_eligible: false,
    serves_stored_requests: false,
    canonicalize_endpoint: accept_endpoint,
    compile: compile_demand,
};

/// A demand source that sends the standard request with nothing of its own
/// and reads every answer as no bid, so the auction's demand seam is reached
/// from a crate core does not know.
struct SeamProbeDemand;

/// Compiles the probe's demand source, which takes no settings of its own.
///
/// # Errors
///
/// Returns an error naming the first setting the table carries, because the
/// source reads none.
fn compile_demand(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
    if let Some(key) = settings.keys().next() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("the seam probe's demand source takes no setting, and `{key}` is set"),
        }));
    }
    Ok(Arc::new(SeamProbeDemand))
}

#[async_trait(?Send)]
impl CompiledDemand for SeamProbeDemand {
    fn field_policy(&self) -> DemandFieldPolicy {
        DemandFieldPolicy::default()
    }

    fn augment_request(
        &self,
        _extensions: &mut RequestExtensions<'_>,
        _input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>> {
        Ok(())
    }

    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        _response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        Ok(AuctionResponse::no_bid(
            context.provider_id,
            context.response_time_ms,
        ))
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// The probe's ad server implementation, registered on its builder.
pub static SEAM_PROBE_ADSERVER: AdServerImplementation = AdServerImplementation {
    id: SEAM_PROBE_ADSERVER_NAME,
    build: build_adserver,
};

/// How long the orchestrator waits for the probe's ad server, in milliseconds.
const SEAM_PROBE_ADSERVER_TIMEOUT_MS: u32 = 100;

/// An ad server that answers at once and picks nothing, so the auction's ad
/// server seam is reached from a crate core does not know.
struct SeamProbeAdServer {
    name: String,
}

/// Builds the probe's ad server under the name its table gave it.
///
/// # Errors
///
/// Never returns an error; the result type matches `BuildAdServerFn`.
fn build_adserver(
    name: &str,
    _settings: &serde_json::Map<String, serde_json::Value>,
) -> Result<Arc<dyn AuctionProvider>, Report<TrustedServerError>> {
    Ok(Arc::new(SeamProbeAdServer {
        name: name.to_owned(),
    }))
}

#[async_trait(?Send)]
impl AuctionProvider for SeamProbeAdServer {
    fn provider_name(&self) -> &str {
        &self.name
    }

    async fn request_bids(
        &self,
        _request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
            self.name.clone(),
            0,
        )))
    }

    async fn parse_response(
        &self,
        _response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        Ok(AuctionResponse::no_bid(self.name.clone(), response_time_ms))
    }

    fn timeout_ms(&self) -> u32 {
        SEAM_PROBE_ADSERVER_TIMEOUT_MS
    }
}

/// Proxy that reports what the seam delivered to this request.
pub struct SeamProbeProxy;

impl SeamProbeProxy {
    async fn route(
        &self,
        req: Request<EdgeBody>,
        services: &RuntimeServices,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        // The geo module is called as every geo call is, with the request
        // as evidence, its services and its client.
        let country = match services
            .geo()
            .locate_request(
                ModuleRequest::of(&req, services.client_info()),
                Some(req.headers()),
                None,
                services,
            )
            .await
        {
            Ok(geo) => geo.map(|info| info.country),
            Err(error) => {
                log::warn!("seam probe geo lookup failed: {error}");
                None
            }
        };
        let prepared = req.extensions().get::<SeamProbePrepared>().copied();

        let body = json!({
            "module": module_name(),
            "geo_country": country,
            "request_preparer_runs": prepared.map_or(0, |marker| marker.runs),
            "public_prefix": services.public_prefix().as_str(),
            "server_key_prefix": services.server_key_prefix().as_str(),
            "answers_cache_keeps_values": services
                .loading_cache_for(SEAM_PROBE_ANSWERS_CACHE)
                .keeps_values(),
        })
        .to_string();

        let mut response = Response::new(EdgeBody::from(body));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(response)
    }

    /// Answers [`SEAM_PROBE_OWN_PATH`] and counts the run against the
    /// request's token. A `HEAD` answer carries the headers a `GET` answer
    /// carries and no body, which the handler leaves out itself because
    /// nothing in core, the adapters or `EdgeZero` strips one.
    fn own_path(&self, req: &Request<EdgeBody>) -> Response<EdgeBody> {
        count_run(&OWN_PATH_COUNTS, req);
        let body = if req.method() == Method::HEAD {
            EdgeBody::empty()
        } else {
            EdgeBody::from(SEAM_PROBE_OWN_BODY)
        };
        let mut response = Response::new(body);
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        response
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for SeamProbeProxy {
    fn integration_name(&self) -> &'static str {
        SEAM_PROBE_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![
            self.get(REPORT_ROUTE_SUFFIX),
            IntegrationEndpoint::get(SEAM_PROBE_OWN_PATH),
            IntegrationEndpoint::new(Method::HEAD, SEAM_PROBE_OWN_PATH),
        ]
    }

    async fn handle(
        &self,
        call: trusted_server_core::module_context::ModuleCall<'_>,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        if req.uri().path() == SEAM_PROBE_OWN_PATH {
            return Ok(call.inject_with(self, &req, Self::own_path)?);
        }
        call.inject_with(self, req, Self::route)?.await
    }
}

/// Rejects a country that is not exactly two ASCII letters.
///
/// # Errors
///
/// Returns [`TrustedServerError::Integration`] carrying
/// [`SEAM_PROBE_COUNTRY_MESSAGE`] and the offending value.
fn check_country(country: &str) -> Result<(), Report<TrustedServerError>> {
    if country.len() == 2 && country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Ok(());
    }

    Err(Report::new(TrustedServerError::Integration {
        integration: SEAM_PROBE_ID.to_string(),
        message: format!("{SEAM_PROBE_COUNTRY_MESSAGE}, not `{country}`"),
    }))
}

/// Reads the probe's settings table, or `None` when no section selects the
/// probe.
///
/// # Errors
///
/// Returns an error when the block is present but cannot be parsed or fails
/// validation.
fn read_config(settings: &Settings) -> Result<Option<SeamProbeConfig>, Report<TrustedServerError>> {
    settings.module_config::<SeamProbeConfig>(module_name())
}

/// Builds the probe's registration, or `None` when no section selects the
/// probe.
///
/// # Errors
///
/// Returns an error when the configuration block cannot be parsed or fails
/// validation, or when the probe is selected with a country that is not two
/// letters.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(config) = read_config(settings)? else {
        return Ok(None);
    };
    check_country(&config.country)?;

    let mut registration = IntegrationRegistration::builder(SEAM_PROBE_ID)
        .with_proxy(Arc::new(SeamProbeProxy))
        .with_js_module(CarriedJsModule {
            source: PROBE_JS,
            sha256: PROBE_JS_SHA256,
        })
        .with_middleware(Arc::new(SeamProbeMiddleware::new(config.country.clone())))
        .with_middleware(Arc::new(SeamProbeReaderMiddleware));
    if config.declares_geo {
        registration = registration.with_geo_module(
            module_name(),
            Arc::new(PlatformGeoModule::new(
                module_name(),
                Arc::new(SeamProbeGeo::new(config.country)),
            )),
        );
    }
    {
        // Identity, device, the prefixes and robots.txt rules are declared
        // the same way location is, so one module supplies all five and there
        // is no second mechanism.
        registration = registration
            .with_ec_module(module_name(), Arc::new(SeamProbeEc))
            .with_device_module(module_name(), Arc::new(SeamProbeDevice))
            .with_prefix_module(module_name(), Arc::new(SeamProbePrefix))
            .with_robots_txt_contributor(module_name(), Arc::new(SeamProbeRobotsTxt));
    }

    Ok(Some(registration.build()))
}

/// Validates the probe's configuration for deployment and reports whether the
/// probe is selected.
///
/// # Errors
///
/// Returns an error when the configuration block cannot be parsed or fails
/// validation, or when the probe is selected with a country that is not two
/// letters.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = read_config(settings)? else {
        return Ok(false);
    };
    check_country(&config.country)?;
    Ok(true)
}

/// Inserts [`SeamProbePrepared`] into the request extensions, counting how
/// many times the request path ran it.
///
/// Runs whether or not the probe is selected, which is the contract the
/// registry gives preparers.
///
/// # Errors
///
/// Never returns an error; the result type matches
/// `IntegrationPrepareRequestFn`.
pub fn prepare_request(
    _settings: &Settings,
    request: &mut Request<EdgeBody>,
) -> Result<(), Report<TrustedServerError>> {
    count_prepare_run(request);
    let runs = request
        .extensions()
        .get::<SeamProbePrepared>()
        .map_or(0, |marker| marker.runs);
    request
        .extensions_mut()
        .insert(SeamProbePrepared { runs: runs + 1 });
    Ok(())
}

/// The probe's integration builder, for an adapter's
/// `routes_with_registrations` or `build_state_with_registrations`.
///
/// # Examples
///
/// ```
/// use trusted_server_integration_seam_probe::{builder, SEAM_PROBE_ID};
///
/// assert_eq!(builder().id(), SEAM_PROBE_ID);
/// ```
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(SEAM_PROBE_ID, SEAM_PROBE_SOURCE, register, validate)
        .with_module_name(module_name())
        .with_request_preparer(prepare_request)
        .with_demand(&SEAM_PROBE_DEMAND)
        .with_adserver(&SEAM_PROBE_ADSERVER)
        .with_cache_features(&[SEAM_PROBE_ANSWERS_CACHE])
}

/// The cache the probe declares and loads its answers through, selected in
/// `[cache.seam-probe-answers]`. The report route says whether the cache
/// that table selected keeps values.
pub const SEAM_PROBE_ANSWERS_CACHE: &str = "seam-probe-answers";

/// The id of the loading cache module the probe offers, selected in a
/// `[cache.<name>]` table as `module = "seam-probe"`.
pub const SEAM_PROBE_LOADING_CACHE_ID: &str = "loading-cache.seam-probe";

/// A loading cache that keeps every value in this process's memory for as
/// long as the process runs, enough to prove from outside core that a table
/// selected it.
#[derive(Default)]
struct MemoryLoadingCache {
    held: Mutex<BTreeMap<String, bytes::Bytes>>,
}

#[async_trait(?Send)]
impl trusted_server_core::platform::PlatformLoadingCache for MemoryLoadingCache {
    async fn get_or_load(
        &self,
        _services: &RuntimeServices,
        _spec: &trusted_server_core::platform::CacheSpec,
        key: &trusted_server_core::platform::StoreKey,
        load: trusted_server_core::platform::Loader,
    ) -> Result<bytes::Bytes, trusted_server_core::platform::LoadError> {
        if let Some(held) = self.held.lock().expect("lock").get(&key.to_string()) {
            return Ok(held.clone());
        }
        let loaded = load().await?.bytes;
        self.held
            .lock()
            .expect("lock")
            .insert(key.to_string(), loaded.clone());
        Ok(loaded)
    }

    async fn remove(
        &self,
        _services: &RuntimeServices,
        _spec: &trusted_server_core::platform::CacheSpec,
        key: &trusted_server_core::platform::StoreKey,
    ) {
        self.held.lock().expect("lock").remove(&key.to_string());
    }
}

fn memory_loading_cache() -> Arc<dyn trusted_server_core::platform::PlatformLoadingCache> {
    Arc::new(MemoryLoadingCache::default())
}

/// The builder offering the probe's loading cache, for a test that proves a
/// declared cache's table selects a module from outside core.
#[must_use]
pub fn loading_cache_builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations(SEAM_PROBE_LOADING_CACHE_ID, SEAM_PROBE_SOURCE)
        .with_loading_cache(memory_loading_cache)
}

/// The vendor's name, which names the table `[shared.seam-probe]` that runs
/// [`shared_builder`].
pub const SEAM_PROBE_SHARED_VENDOR: &str = "seam-probe";

/// The id of the builder the shared table runs.
pub const SEAM_PROBE_SHARED_ID: &str = "seam_probe_shared";

/// The route the shared builder answers with the table's marker.
pub const SEAM_PROBE_SHARED_PATH: &str = "/integrations/seam_probe_shared/marker";

/// The shared route's suffix under the builder's proxy prefix.
const SHARED_ROUTE_SUFFIX: &str = "/marker";

/// What `[shared.seam-probe]` carries.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct SeamProbeShared {
    /// The text the shared route answers with.
    #[validate(length(min = 1))]
    pub marker: String,
}

/// A route answering the shared table's marker, which proves the table ran
/// the builder and reached what it registered.
#[derive(Debug)]
struct SharedMarkerRoute {
    marker: String,
}

impl SharedMarkerRoute {
    async fn route(
        &self,
        _req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        Response::builder()
            .status(200)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )
            .body(EdgeBody::from(self.marker.clone().into_bytes()))
            .map_err(|error| {
                Report::new(TrustedServerError::Integration {
                    integration: SEAM_PROBE_SHARED_ID.to_string(),
                    message: error.to_string(),
                })
            })
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for SharedMarkerRoute {
    fn integration_name(&self) -> &'static str {
        SEAM_PROBE_SHARED_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get(SHARED_ROUTE_SUFFIX)]
    }

    async fn handle(
        &self,
        call: ModuleCall<'_>,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}

/// Registers the shared route when `[shared.seam-probe]` is present.
///
/// # Errors
///
/// Returns an error when the table cannot be read or fails its validation.
pub fn register_shared(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(shared) = settings.shared_config::<SeamProbeShared>(SEAM_PROBE_SHARED_VENDOR)? else {
        return Ok(None);
    };
    Ok(Some(
        IntegrationRegistration::builder(SEAM_PROBE_SHARED_ID)
            .without_js()
            .with_proxy(Arc::new(SharedMarkerRoute {
                marker: shared.marker,
            }))
            .build(),
    ))
}

/// Reports whether the shared table is present, validating it.
///
/// # Errors
///
/// Returns an error when the table cannot be read or fails its validation.
pub fn validate_shared(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    Ok(settings
        .shared_config::<SeamProbeShared>(SEAM_PROBE_SHARED_VENDOR)?
        .is_some())
}

/// The builder `[shared.seam-probe]` runs, which proves a vendor's shared
/// table from outside core.
#[must_use]
pub fn shared_builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        SEAM_PROBE_SHARED_ID,
        SEAM_PROBE_SOURCE,
        register_shared,
        validate_shared,
    )
    .with_shared_settings(SEAM_PROBE_SHARED_VENDOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    use sha2::{Digest as _, Sha256};
    use trusted_server_core::platform::{
        BackendNamingPolicy, ClientInfo, DisabledGeo, PlatformBackend, PlatformBackendSpec,
        PlatformConfigStore, PlatformSecretStore, StoreId, StoreName, UnavailableHttpClient,
        UnavailableKvStore,
    };

    /// Config store that answers nothing, so a test can build
    /// [`RuntimeServices`] without a real platform.
    ///
    /// [`SeamProbeGeo::lookup`] resolves from the country it holds and reads
    /// none of the services, so the store is never queried.
    struct StubConfigStore;

    impl PlatformConfigStore for StubConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
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

        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    /// Builds a minimal [`RuntimeServices`] the geo lookup can be handed. Every
    /// service is a stub or a core-provided unavailable implementation because
    /// [`SeamProbeGeo::lookup`] reads none of them.
    fn build_probe_services() -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(StubConfigStore))
            .secret_store(Arc::new(StubSecretStore))
            .kv_store(Arc::new(UnavailableKvStore))
            .backend(Arc::new(StubBackend))
            .http_client(Arc::new(UnavailableHttpClient))
            .geo(Arc::new(PlatformGeoModule::new(
                "none",
                Arc::new(DisabledGeo),
            )))
            .client_info(ClientInfo::default())
            .permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
            .build()
    }

    /// Settings carrying a `[testing.seam-probe]` block with `body`
    /// appended to it.
    fn settings_with_probe(body: &str) -> Settings {
        Settings::from_toml(&format!(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.test-publisher.example.com"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                proxy_secret = "seam-probe-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true

                [testing]
                modules = ["seam-probe"]

                [testing.seam-probe]
                {body}
            "#
        ))
        .expect("should parse seam probe test settings")
    }

    /// A page with one link and one script the probe's middleware changes,
    /// and one of each that it leaves alone.
    const PROBE_PAGE: &str = concat!(
        "<html><head><title>Page</title></head><body>",
        "<a data-seam-probe href=\"https://origin.example.com/probe\">probe</a>",
        "<a href=\"https://origin.example.com/other\">other</a>",
        "<script data-seam-probe>var country = \"SEAM_PROBE_COUNTRY\";</script>",
        "<script>var word = \"SEAM_PROBE_COUNTRY\";</script>",
        "</body></html>",
    );

    /// [`PROBE_PAGE`] as a reader receives it from a deployment running
    /// `settings` with the probe registered, read `chunk_size` bytes at a
    /// time.
    fn probe_page(settings: &Settings, chunk_size: usize) -> String {
        use trusted_server_core::html_processor::test_support::processed_page;
        use trusted_server_core::integrations::IntegrationRegistry;

        let registry = IntegrationRegistry::with_registrations(settings, &[builder()])
            .expect("should build a registry with the probe registered");
        processed_page(settings, &registry, PROBE_PAGE, chunk_size)
    }

    #[test]
    fn the_middleware_changes_a_page_where_an_entry_names_it() {
        use trusted_server_core::html_processor::test_support::REQUEST_HOST;

        let settings = settings_with_probe(
            r#"country = "NZ"

                [[fetch]]
                media_type = "text/html"
                middleware = ["testing.seam-probe"]
            "#,
        );

        // Read whole, and then seven bytes at a time so the script's text
        // reaches the handler in pieces.
        for chunk_size in [8192, 7] {
            let page = probe_page(&settings, chunk_size);

            assert_eq!(
                page.matches(&seam_probe_head_marker("NZ")).count(),
                1,
                "should mark the head once, with the country the probe is set to: {page}"
            );
            assert_eq!(
                page.matches(&seam_probe_body_marker()).count(),
                1,
                "should mark the body once: {page}"
            );
            assert!(
                page.contains(&format!("<body>{}", seam_probe_body_marker())),
                "should mark immediately after the opening body tag: {page}"
            );
            assert!(
                page.contains(&format!(
                    "<a data-seam-probe href=\"{SEAM_PROBE_REPORT_PATH}\">probe</a>"
                )),
                "should point the probe's link at its report route: {page}"
            );
            assert!(
                page.contains(&format!(
                    "<a href=\"https://{REQUEST_HOST}/other\">other</a>"
                )),
                "should leave another link to core, which moves it to the reader's host: {page}"
            );
            assert!(
                page.contains("<script data-seam-probe>var country = \"NZ\";</script>"),
                "should write the country into the probe's script: {page}"
            );
            assert!(
                page.contains("<script>var word = \"SEAM_PROBE_COUNTRY\";</script>"),
                "should leave another script as it was: {page}"
            );
        }
    }

    #[test]
    fn the_serve_middleware_marks_a_reader_s_copy_where_an_entry_names_it() {
        use trusted_server_core::html_processor::test_support::REQUEST_HOST;

        let settings = settings_with_probe(
            r#"country = "NZ"

                [[fetch]]
                media_type = "text/html"
                middleware = ["testing.seam-probe"]

                [[serve]]
                media_type = "text/html"
                middleware = ["testing.seam-probe.reader"]
            "#,
        );

        let page = probe_page(&settings, 8192);

        let country = page
            .find(&seam_probe_head_marker("NZ"))
            .expect("should mark the head with the country");
        let reader = page
            .find(&seam_probe_reader_marker(REQUEST_HOST))
            .expect("should mark the head with the host the reader asked for");
        let bundle = page
            .find("id=\"trustedserver-js\"")
            .expect("should carry the script bundle");
        assert!(
            country < reader && reader < bundle,
            "should write the serve middleware's mark after the fetch one's and ahead of the \
             bundle: {page}"
        );
        assert_eq!(
            reader_middleware_name(),
            "testing.seam-probe.reader",
            "should name the serve middleware after the module"
        );
    }

    #[test]
    fn the_middleware_changes_nothing_where_no_entry_names_it() {
        use trusted_server_core::html_processor::test_support::REQUEST_HOST;

        let page = probe_page(&settings_with_probe(r#"country = "NZ""#), 8192);

        assert!(
            !page.contains("seam-probe-country") && !page.contains("seam-probe-reader"),
            "should not mark the head of a page no entry places it on: {page}"
        );
        assert!(
            page.contains(&format!(
                "<a data-seam-probe href=\"https://{REQUEST_HOST}/probe\">probe</a>"
            )),
            "should leave the probe's link to core: {page}"
        );
        assert!(
            page.contains("<script data-seam-probe>var country = \"SEAM_PROBE_COUNTRY\";</script>"),
            "should leave the probe's script as it was: {page}"
        );
    }

    /// The registry rejects a carried module whose declared hash is wrong, so
    /// this keeps the fixture's literal honest rather than letting every other
    /// test fail with a startup error.
    #[test]
    fn carried_module_hash_literal_matches_its_source() {
        assert_eq!(
            hex::encode(Sha256::digest(PROBE_JS.as_bytes())),
            PROBE_JS_SHA256,
            "PROBE_JS_SHA256 should be the SHA-256 of js/probe.js; if the source is unchanged, check that the file was checked out with LF line endings as .gitattributes requires"
        );
    }

    #[test]
    fn auction_implementations_are_named_as_the_module() {
        assert_eq!(
            SEAM_PROBE_ADSERVER.id,
            module_name(),
            "the ad server is named as the crate's folder below `crates/` names it"
        );
        assert_eq!(
            SEAM_PROBE_DEMAND.id,
            module_name(),
            "the demand source is named as the crate's folder below `crates/` names it"
        );
    }

    #[test]
    fn the_demand_source_refuses_a_setting_it_does_not_read() {
        let mut settings = serde_json::Map::new();
        settings.insert("unexpected".to_owned(), serde_json::Value::Bool(true));

        assert!(
            compile_demand(&serde_json::Map::new()).is_ok(),
            "an empty table should compile"
        );
        let error = compile_demand(&settings)
            .err()
            .expect("should refuse a setting the source does not read");
        assert!(
            error.to_string().contains("`unexpected`"),
            "should name the setting: {error}"
        );
    }

    #[test]
    fn report_route_is_the_documented_path() {
        let routes: Vec<(Method, String)> = SeamProbeProxy
            .routes()
            .into_iter()
            .map(|route| (route.method, route.path))
            .collect();

        assert_eq!(
            routes,
            [
                (Method::GET, SEAM_PROBE_REPORT_PATH.to_owned()),
                (Method::GET, SEAM_PROBE_OWN_PATH.to_owned()),
                (Method::HEAD, SEAM_PROBE_OWN_PATH.to_owned()),
            ],
            "should route the documented report path, and the probe's own path for GET and \
             HEAD"
        );
    }

    #[test]
    fn validate_accepts_a_two_letter_country() {
        let settings = settings_with_probe("country = \"ZZ\"");

        assert!(
            validate(&settings).expect("should accept a two letter country"),
            "should report the probe as selected"
        );
    }

    #[test]
    fn validate_rejects_a_country_that_is_not_two_letters() {
        let settings = settings_with_probe("country = \"ZZZ\"");

        let error = validate(&settings).expect_err("should reject a three letter country");

        assert!(
            error.to_string().contains(SEAM_PROBE_COUNTRY_MESSAGE),
            "should reject with the probe's own message: {error}"
        );
    }

    #[test]
    fn validate_reports_not_selected_without_a_module_entry() {
        let settings = Settings::from_toml(
            r#"
                [source]
                module = "origin"

                [source.origin]
                url = "https://origin.test-publisher.example.com"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                proxy_secret = "seam-probe-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"

                [geo]
                assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse settings that do not name the probe");

        assert!(
            !validate(&settings).expect("should validate settings that do not name the probe"),
            "should report the probe as not selected"
        );
        assert!(
            register(&settings)
                .expect("should build no registration")
                .is_none(),
            "should build no registration when the probe is not selected"
        );
    }

    #[test]
    fn registration_declares_no_geo_module_when_the_block_turns_it_off() {
        let settings = settings_with_probe("country = \"ZZ\"\ndeclares_geo = false");

        let registration = register(&settings)
            .expect("should build a registration")
            .expect("should be selected");

        assert!(
            registration.geo_module.is_none(),
            "should declare no geo module when declares_geo is false"
        );
    }

    /// The head marker's markup and the report path are compiled into this
    /// crate and no setting changes when they do, so they have to be in the
    /// text the registered fetch middleware keys a stored page by.
    #[test]
    fn the_marker_and_the_report_path_are_in_what_keys_a_stored_page() {
        use trusted_server_core::integrations::IntegrationRegistry;

        let settings = settings_with_probe("country = \"NZ\"");
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should build a registry with the probe registered");

        let keys = registry.middleware_template_keys();
        let (_, key) = keys
            .iter()
            .find(|(name, _)| *name == module_name())
            .expect("the probe's fetch middleware should be registered");
        assert!(key.contains(&seam_probe_head_marker("NZ")), "{key}");
        assert!(key.contains(SEAM_PROBE_REPORT_PATH), "{key}");
    }

    #[tokio::test]
    async fn geo_module_resolves_the_configured_country() {
        let module = SeamProbeGeo::new("ZZ".to_string());
        let services = build_probe_services();

        let geo = module
            .lookup(None, &services)
            .await
            .expect("should resolve location")
            .expect("should return geo information");

        assert_eq!(geo.country, "ZZ", "should resolve the configured country");
    }

    #[test]
    fn prepare_request_counts_each_run() {
        let settings = settings_with_probe("country = \"ZZ\"");
        let mut request = Request::new(EdgeBody::from(Vec::new()));

        prepare_request(&settings, &mut request).expect("should prepare the request");
        prepare_request(&settings, &mut request).expect("should prepare the request again");

        assert_eq!(
            request.extensions().get::<SeamProbePrepared>(),
            Some(&SeamProbePrepared { runs: 2 }),
            "should count both runs"
        );
    }
}
