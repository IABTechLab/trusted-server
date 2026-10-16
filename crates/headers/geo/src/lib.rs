//! The reader's location as the `x-geo-*` response headers.
//!
//! The location is the one the request already resolved through its geo
//! module, which the module call hands this middleware's preparation, so no
//! geo module is asked again. The headers go on the responses a `[[serve]]`
//! entry naming the middleware covers, and on no other response.

use std::any::Any;
use std::sync::Arc;

use error_stack::Report;
use http::{HeaderName, HeaderValue};
use serde::Deserialize;
use validator::{Validate, ValidationErrors};

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::middleware::{
    Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase, Preparation, ResponseHeader,
    is_dispatched_media_type,
};
use trusted_server_core::module_context::{FromModuleContext as _, ModuleCall};
use trusted_server_core::settings::{IntegrationConfig, Settings};

/// The id the module's registration carries.
const INTEGRATION_ID: &str = "headers_geo";

/// The name `[headers] modules` selects the module by and a `[[serve]]`
/// entry names its middleware by, from this crate's folder.
pub const MODULE: &str = "headers.geo";

const HEADER_X_GEO_CITY: HeaderName = HeaderName::from_static("x-geo-city");
const HEADER_X_GEO_CONTINENT: HeaderName = HeaderName::from_static("x-geo-continent");
const HEADER_X_GEO_COORDINATES: HeaderName = HeaderName::from_static("x-geo-coordinates");
const HEADER_X_GEO_COUNTRY: HeaderName = HeaderName::from_static("x-geo-country");
const HEADER_X_GEO_INFO_AVAILABLE: HeaderName = HeaderName::from_static("x-geo-info-available");
const HEADER_X_GEO_METRO_CODE: HeaderName = HeaderName::from_static("x-geo-metro-code");
const HEADER_X_GEO_REGION: HeaderName = HeaderName::from_static("x-geo-region");

/// The module's settings, of which there are none, so a key in its table is
/// refused rather than ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoHeadersConfig {}

impl Validate for GeoHeadersConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        Ok(())
    }
}

impl IntegrationConfig for GeoHeadersConfig {}

/// Refuses a document that selects the module and names its middleware in no
/// `[[serve]]` entry, where the headers would never be written.
fn require_an_entry(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let placed = settings
        .serve
        .entries()
        .iter()
        .any(|entry| entry.middleware.iter().any(|name| name == MODULE));
    if placed {
        return Ok(());
    }
    Err(Report::new(TrustedServerError::Settings {
        message: format!(
            "[headers] selects `geo`, but no [[serve]] entry names `{MODULE}`, so the headers \
             would never be written. Add `{MODULE}` to the [[serve]] entry that covers the \
             responses that are to carry them"
        ),
    }))
}

/// Validates the module's table and reports whether a section selects the
/// module.
///
/// # Errors
///
/// Returns an error when the table holds a key, or when no `[[serve]]` entry
/// names the middleware.
pub fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    if settings
        .module_config::<GeoHeadersConfig>(MODULE)?
        .is_none()
    {
        return Ok(false);
    }
    require_an_entry(settings)?;
    Ok(true)
}

/// Registers the middleware when a section selects the module.
///
/// # Errors
///
/// Returns the errors [`validate`] returns.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    if !validate(settings)? {
        return Ok(None);
    }
    Ok(Some(
        IntegrationRegistration::builder(INTEGRATION_ID)
            .without_js()
            .with_middleware(Arc::new(GeoHeaders))
            .build(),
    ))
}

/// The builder a deployment installs to offer this module.
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(INTEGRATION_ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_module_name(MODULE)
}

/// The middleware, which writes the location the request resolved.
#[derive(Debug)]
pub struct GeoHeaders;

impl Middleware for GeoHeaders {
    fn middleware_id(&self) -> &'static str {
        MODULE
    }

    /// Every media type an entry may cover, because the middleware writes
    /// headers and leaves the body alone.
    fn handles_media_type(&self, media_type: &str) -> bool {
        is_dispatched_media_type(media_type)
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Serve]
    }

    /// Takes the location from the module call, which is none when the
    /// request resolved none or the geo module's permissions withhold it.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        _context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        let location = <&GeoInfo>::from_module_context(&call).ok().cloned();
        Some(Box::pin(async move {
            Ok(Box::new(location) as Box<dyn Any + Send>)
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let location = context
            .take_prepared::<Option<GeoInfo>>(MODULE)
            .unwrap_or_else(|| {
                // Nothing prepared means the preparation did not run, which no
                // request can cause.
                log::error!("the location was not prepared, so the response says none is known");
                None
            });
        MiddlewareAction {
            response_headers: geo_headers(location.as_ref()),
            ..MiddlewareAction::pass()
        }
    }
}

/// The headers that carry `location`, or `x-geo-info-available: false` alone
/// when there is none.
fn geo_headers(location: Option<&GeoInfo>) -> Vec<ResponseHeader> {
    let Some(geo) = location else {
        return vec![(
            HEADER_X_GEO_INFO_AVAILABLE,
            HeaderValue::from_static("false"),
        )];
    };
    let mut headers = Vec::new();
    push_header(&mut headers, HEADER_X_GEO_CITY, &geo.city);
    push_header(&mut headers, HEADER_X_GEO_COUNTRY, &geo.country);
    push_header(&mut headers, HEADER_X_GEO_CONTINENT, &geo.continent);
    push_header(
        &mut headers,
        HEADER_X_GEO_COORDINATES,
        &geo.coordinates_string(),
    );
    if geo.has_metro_code() {
        push_header(
            &mut headers,
            HEADER_X_GEO_METRO_CODE,
            &geo.metro_code.to_string(),
        );
    }
    if let Some(region) = &geo.region {
        push_header(&mut headers, HEADER_X_GEO_REGION, region);
    }
    headers.push((
        HEADER_X_GEO_INFO_AVAILABLE,
        HeaderValue::from_static("true"),
    ));
    headers
}

/// Adds `value` under `name`, or skips it with a warning when a header cannot
/// carry it.
fn push_header(headers: &mut Vec<ResponseHeader>, name: HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => headers.push((name, value)),
        Err(_) => log::warn!("Skipping invalid geo header value for {}", name.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use edgezero_core::body::Body as EdgeBody;
    use futures::executor::block_on;
    use http::{Method, Request, header};
    use trusted_server_core::auction::compile_auction_plan;
    use trusted_server_core::auction::orchestrator::AuctionOrchestrator;
    use trusted_server_core::cache_policy::EdgeCacheHeader;
    use trusted_server_core::ec::EcContext;
    use trusted_server_core::html_processor::test_support::page_settings;
    use trusted_server_core::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use trusted_server_core::middleware::test_support::context;
    use trusted_server_core::middleware::{HTML_MEDIA_TYPE, MiddlewareChain, Prepared};
    use trusted_server_core::module_context::{ModuleContext, ModuleRequest};
    use trusted_server_core::permissions::{Permission, PermissionSet};
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };
    use trusted_server_core::platform::{
        GeoModule, PlatformError, PlatformHttpClient, RuntimeServices,
    };
    use trusted_server_core::publisher::{
        AppContext, AuctionDispatch, buffer_publisher_response_async, handle_publisher_request,
    };

    /// The settings of a deployment that selects the module and places it on
    /// every page.
    const PLACED: &str = include_str!("fixtures/geo-headers.settings.toml");

    fn located() -> GeoInfo {
        GeoInfo {
            city: "San Francisco".to_owned(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 37.7749,
            longitude: -122.4194,
            metro_code: 807,
            region: Some("CA".to_owned()),
            asn: None,
        }
    }

    /// The headers as text, in the order they are written.
    fn as_text(headers: &[ResponseHeader]) -> Vec<(&str, &str)> {
        headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str(),
                    value.to_str().expect("should be a text header value"),
                )
            })
            .collect()
    }

    /// What the middleware writes for a request whose module context is
    /// `module_context`, prepared and planned as the serve phase runs it.
    fn written_for(module_context: &ModuleContext<'_>) -> Vec<ResponseHeader> {
        let chain = MiddlewareChain::new(
            MiddlewarePhase::Serve,
            HTML_MEDIA_TYPE,
            vec![Arc::new(GeoHeaders)],
        );
        let document_state = IntegrationDocumentState::default();
        let prepared = Prepared::default();
        let context = MiddlewareContext {
            prepared: &prepared,
            ..context(MiddlewarePhase::Serve, &document_state)
        };
        block_on(chain.prepare(module_context, &context)).expect("should prepare");
        chain.plan(&context).expect("should plan").response_headers
    }

    fn request_view(method: &Method) -> ModuleRequest<'_> {
        ModuleRequest::new(method, "publisher.example.com", "https", "/article")
    }

    /// A geo module that answers one location and counts the calls it is
    /// asked.
    struct CountingGeo {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait(?Send)]
    impl GeoModule for CountingGeo {
        fn id(&self) -> &'static str {
            "testing.counting-geo"
        }

        async fn locate(
            &self,
            _call: ModuleCall<'_>,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(located()))
        }
    }

    fn page_request() -> Request<EdgeBody> {
        Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article")
            .header(header::HOST, "publisher.example.com")
            .body(EdgeBody::empty())
            .expect("should build the page request")
    }

    /// The page a reader of `settings` receives from an origin that answers
    /// with HTML, and how many times the geo module was asked for it.
    fn page_for(settings: &Settings) -> (http::Response<EdgeBody>, usize) {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"<html><head></head><body>origin</body></html>".to_vec(),
            vec![("content-type", "text/html; charset=utf-8")],
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let services: RuntimeServices =
            build_services_with_http_client(Arc::clone(&stub) as Arc<dyn PlatformHttpClient>)
                .with_geo(Arc::new(CountingGeo {
                    calls: Arc::clone(&calls),
                }));
        let registry = IntegrationRegistry::with_registrations(settings, &[builder()])
            .expect("should build the registry");
        let plan = compile_auction_plan(settings).expect("should compile the auction plan");
        let orchestrator = AuctionOrchestrator::from_plan(Arc::new(plan), None);
        let request = page_request();

        let response = block_on(async {
            let mut ec_context =
                EcContext::read_from_request_resolving_geo(settings, &request, &services)
                    .await
                    .expect("should read the request's state");
            let publisher = handle_publisher_request(
                AppContext {
                    settings,
                    integration_registry: &registry,
                },
                &services,
                None,
                &mut ec_context,
                AuctionDispatch {
                    orchestrator: &orchestrator,
                    slots: &[],
                    registry: None,
                },
                request,
                EdgeCacheHeader::SurrogateControl,
            )
            .await
            .expect("should fetch the page");
            buffer_publisher_response_async(
                publisher,
                &Method::GET,
                settings,
                &registry,
                &orchestrator,
                &services,
            )
            .await
            .expect("should finish the page")
        });
        (response, calls.load(Ordering::SeqCst))
    }

    /// The `x-geo-*` headers a response carries, sorted by name.
    fn geo_headers_on(response: &http::Response<EdgeBody>) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter(|(name, _)| name.as_str().starts_with("x-geo-"))
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().expect("should be text").to_owned(),
                )
            })
            .collect();
        headers.sort();
        headers
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }

    #[test]
    fn an_unselected_module_registers_nothing() {
        let settings = page_settings("");

        assert!(register(&settings).expect("should read").is_none());
        assert!(!validate(&settings).expect("should read"));
    }

    #[test]
    fn a_document_selecting_the_module_and_naming_it_in_no_serve_entry_is_refused() {
        let settings = page_settings("[headers]\nmodules = [\"geo\"]\n");

        let refused = validate(&settings).expect_err("should refuse a module no entry places");
        assert!(
            format!("{refused:?}").contains("no [[serve]] entry names `headers.geo`"),
            "{refused:?}"
        );
        assert!(register(&settings).is_err(), "should refuse to register");
    }

    #[test]
    fn the_serve_entry_naming_the_module_runs_its_middleware() {
        let settings = page_settings(PLACED);
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should build the registry");

        assert_eq!(
            registry
                .middleware_chain(
                    &settings.serve,
                    MiddlewarePhase::Serve,
                    HTML_MEDIA_TYPE,
                    "/article"
                )
                .ids(),
            [MODULE],
            "should run on a page the entry covers"
        );
    }

    #[test]
    fn the_middleware_runs_in_the_serve_phase_on_every_media_type_an_entry_may_cover() {
        assert_eq!(GeoHeaders.phases(), [MiddlewarePhase::Serve]);
        for media_type in [
            "text/html",
            "text/plain",
            "application/json",
            "application/javascript",
        ] {
            assert!(
                GeoHeaders.handles_media_type(media_type),
                "should work on {media_type}"
            );
        }
        assert!(!GeoHeaders.handles_media_type("image/png"));
    }

    #[test]
    fn the_location_in_the_module_call_is_written_as_the_headers() {
        let method = Method::GET;
        let geo = located();
        let module_context =
            ModuleContext::new(request_view(&method)).with_geo(&geo, PermissionSet::none());

        assert_eq!(
            as_text(&written_for(&module_context)),
            [
                ("x-geo-city", "San Francisco"),
                ("x-geo-country", "US"),
                ("x-geo-continent", "NorthAmerica"),
                ("x-geo-coordinates", "37.7749,-122.4194"),
                ("x-geo-metro-code", "807"),
                ("x-geo-region", "CA"),
                ("x-geo-info-available", "true"),
            ],
            "should write every part of the location the request resolved"
        );
    }

    #[test]
    fn a_metro_code_of_zero_and_no_region_are_not_written() {
        let method = Method::GET;
        let geo = GeoInfo {
            metro_code: 0,
            region: None,
            ..located()
        };
        let module_context =
            ModuleContext::new(request_view(&method)).with_geo(&geo, PermissionSet::none());

        let headers = written_for(&module_context);
        let written = as_text(&headers);

        assert!(
            written
                .iter()
                .all(|(name, _)| *name != "x-geo-metro-code" && *name != "x-geo-region"),
            "should leave out what the answer does not have: {written:?}"
        );
        assert!(
            written.contains(&("x-geo-info-available", "true")),
            "should still say a location is known: {written:?}"
        );
    }

    #[test]
    fn a_value_a_header_cannot_carry_is_skipped() {
        let method = Method::GET;
        let geo = GeoInfo {
            city: "Line\nbreak".to_owned(),
            ..located()
        };
        let module_context =
            ModuleContext::new(request_view(&method)).with_geo(&geo, PermissionSet::none());

        let headers = written_for(&module_context);
        let written = as_text(&headers);

        assert!(
            written.iter().all(|(name, _)| *name != "x-geo-city"),
            "should skip the city a header cannot carry: {written:?}"
        );
        assert!(
            written.contains(&("x-geo-country", "US")),
            "should write the rest: {written:?}"
        );
    }

    #[test]
    fn no_location_says_none_is_available() {
        let method = Method::GET;
        let module_context = ModuleContext::new(request_view(&method));

        assert_eq!(
            as_text(&written_for(&module_context)),
            [("x-geo-info-available", "false")],
            "should say no location is known and write nothing else"
        );
    }

    #[test]
    fn a_location_whose_use_needs_a_permission_the_middleware_does_not_declare_is_not_written() {
        let method = Method::GET;
        let geo = located();
        let module_context = ModuleContext::new(request_view(&method))
            .with_geo(&geo, PermissionSet::none().with(Permission::SelectBasicAds));

        assert_eq!(
            as_text(&written_for(&module_context)),
            [("x-geo-info-available", "false")],
            "should write no location the geo module's permissions withhold"
        );
    }

    #[test]
    fn nothing_prepared_says_no_location_is_known() {
        let document_state = IntegrationDocumentState::default();

        let action = GeoHeaders.create(&context(MiddlewarePhase::Serve, &document_state));

        assert_eq!(
            as_text(&action.response_headers),
            [("x-geo-info-available", "false")],
            "should say no location is known when nothing was prepared"
        );
    }

    #[test]
    fn a_page_carries_the_location_the_request_resolved_and_the_geo_module_is_asked_once() {
        let settings = page_settings(PLACED);

        let (response, calls) = page_for(&settings);

        assert_eq!(
            geo_headers_on(&response),
            [
                ("x-geo-city", "San Francisco"),
                ("x-geo-continent", "NorthAmerica"),
                ("x-geo-coordinates", "37.7749,-122.4194"),
                ("x-geo-country", "US"),
                ("x-geo-info-available", "true"),
                ("x-geo-metro-code", "807"),
                ("x-geo-region", "CA"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
            "should write the location the request's state resolved on the page"
        );
        assert_eq!(
            calls, 1,
            "should ask the geo module once, for the request's state, and not again for the headers"
        );
    }

    #[test]
    fn a_page_no_serve_entry_names_the_middleware_on_carries_no_geo_header() {
        let settings = page_settings("");

        let (response, calls) = page_for(&settings);

        assert!(
            geo_headers_on(&response).is_empty(),
            "should write no x-geo-* header: {:?}",
            geo_headers_on(&response)
        );
        assert_eq!(calls, 1, "should still resolve the request's location once");
    }
}
