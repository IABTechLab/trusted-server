use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::Method;
use http::header::{self, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError};

use crate::error::TrustedServerError;
use crate::integrations::{
    INTEGRATION_MAX_BODY_BYTES, IntegrationEndpoint, IntegrationHeadInjector,
    IntegrationHtmlContext, IntegrationProxy, IntegrationRegistration, collect_body_bounded,
    ensure_integration_backend,
};
use crate::platform::{GeoInfo, PlatformHttpRequest, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

const DIDOMI_INTEGRATION_ID: &str = "didomi";
const DIDOMI_DEFAULT_PREFIX: &str = "/integrations/didomi/consent";

/// Configuration for the Didomi consent notice reverse proxy.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct DidomiIntegrationConfig {
    /// Whether the integration is enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Add trusted country and region parameters to notice-loader URLs.
    #[serde(default)]
    pub geo_query_parameters: bool,
    /// Custom proxy path prefix to avoid ad-blocker detection.
    /// Defaults to "integrations/didomi/consent" if not set.
    #[serde(default)]
    #[validate(custom(function = "validate_proxy_path"))]
    pub proxy_path: Option<String>,
    /// Base URL for the Didomi SDK origin.
    #[serde(default = "default_sdk_origin")]
    #[validate(url)]
    pub sdk_origin: String,
    /// Base URL for the Didomi API origin.
    #[serde(default = "default_api_origin")]
    #[validate(url)]
    pub api_origin: String,
}

/// Validates the optional `proxy_path` value.
/// Rejects empty, root-only, trailing-slash, dot-segment, and values
/// containing characters that are unsafe for URL path routing.
fn validate_proxy_path(value: &str) -> Result<(), ValidationError> {
    let trimmed = value.trim_start_matches('/');

    if trimmed.is_empty() {
        return Err(ValidationError::new("proxy_path_empty"));
    }

    if trimmed.ends_with('/') {
        return Err(ValidationError::new("proxy_path_trailing_slash"));
    }

    if trimmed.contains("//") {
        return Err(ValidationError::new("proxy_path_double_slash"));
    }

    if trimmed
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(ValidationError::new("proxy_path_dot_segment"));
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '/'))
    {
        return Err(ValidationError::new("proxy_path_forbidden_chars"));
    }

    Ok(())
}

impl IntegrationConfig for DidomiIntegrationConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

fn default_enabled() -> bool {
    true
}

fn default_sdk_origin() -> String {
    "https://sdk.privacy-center.org".to_string()
}

fn default_api_origin() -> String {
    "https://api.privacy-center.org".to_string()
}

enum DidomiBackend {
    Sdk,
    Api,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DidomiGeo {
    country: String,
    region: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DidomiGeoError {
    MissingCountry,
    MissingRegion,
    InvalidCountry,
    InvalidRegion,
}

impl DidomiGeoError {
    const fn reason(self) -> &'static str {
        match self {
            Self::MissingCountry => "missing_country",
            Self::MissingRegion => "missing_region",
            Self::InvalidCountry => "invalid_country",
            Self::InvalidRegion => "invalid_region",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct CanonicalLoaderUrl {
    browser_target: String,
    query: String,
}

fn is_notice_loader(method: &Method, consent_path: &str) -> bool {
    if *method != Method::GET {
        return false;
    }

    let Some(path) = consent_path.strip_prefix('/') else {
        return false;
    };
    let mut segments = path.split('/');
    let public_key = segments.next();
    let file_name = segments.next();

    public_key.is_some_and(|segment| !segment.is_empty())
        && file_name == Some("loader.js")
        && segments.next().is_none()
}

fn trim_ascii(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_ascii_whitespace())
}

fn normalize_didomi_geo(geo: &GeoInfo) -> Result<DidomiGeo, DidomiGeoError> {
    let country = trim_ascii(&geo.country).to_ascii_uppercase();
    if country.is_empty() {
        return Err(DidomiGeoError::MissingCountry);
    }
    if country.len() != 2
        || !country.bytes().all(|byte| byte.is_ascii_alphabetic())
        || matches!(country.as_str(), "XX" | "ZZ")
    {
        return Err(DidomiGeoError::InvalidCountry);
    }

    let Some(region) = geo.region.as_deref() else {
        return Err(DidomiGeoError::MissingRegion);
    };
    let region = trim_ascii(region).to_ascii_uppercase();
    if region.is_empty() {
        return Err(DidomiGeoError::MissingRegion);
    }
    let region = match region.split_once('-') {
        Some((prefix, subdivision)) if prefix == country => subdivision,
        Some(_) => return Err(DidomiGeoError::InvalidRegion),
        None => region.as_str(),
    };
    if !(1..=3).contains(&region.len()) || !region.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(DidomiGeoError::InvalidRegion);
    }

    Ok(DidomiGeo {
        country,
        region: region.to_string(),
    })
}

fn canonical_loader_url(path: &str, query: Option<&str>, geo: &DidomiGeo) -> CanonicalLoaderUrl {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    if let Some(query) = query {
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if !name.eq_ignore_ascii_case("country") && !name.eq_ignore_ascii_case("region") {
                serializer.append_pair(&name, &value);
            }
        }
    }
    serializer.append_pair("country", &geo.country);
    serializer.append_pair("region", &geo.region);
    let query = serializer.finish();

    CanonicalLoaderUrl {
        browser_target: format!("{path}?{query}"),
        query,
    }
}

struct DidomiIntegration {
    config: Arc<DidomiIntegrationConfig>,
}

impl DidomiIntegration {
    fn new(config: Arc<DidomiIntegrationConfig>) -> Arc<Self> {
        Arc::new(Self { config })
    }

    fn error(message: impl Into<String>) -> TrustedServerError {
        TrustedServerError::Integration {
            integration: DIDOMI_INTEGRATION_ID.to_string(),
            message: message.into(),
        }
    }

    /// Returns the canonicalized proxy prefix: always starts with `/`, no trailing slash.
    fn resolved_prefix(&self) -> String {
        match &self.config.proxy_path {
            Some(custom) => format!("/{}", custom.trim_start_matches('/')),
            None => DIDOMI_DEFAULT_PREFIX.to_string(),
        }
    }

    fn backend_for_path(&self, consent_path: &str) -> DidomiBackend {
        if consent_path.starts_with("/api/") {
            DidomiBackend::Api
        } else {
            DidomiBackend::Sdk
        }
    }

    fn build_target_url(
        &self,
        base: &str,
        consent_path: &str,
        query: Option<&str>,
    ) -> Result<String, Report<TrustedServerError>> {
        let mut target =
            Url::parse(base).change_context(Self::error("Invalid Didomi origin URL"))?;
        let path = if consent_path.is_empty() {
            "/"
        } else {
            consent_path
        };
        target.set_path(path);
        target.set_query(query);
        Ok(target.to_string())
    }

    fn copy_headers(
        &self,
        backend: &DidomiBackend,
        client_ip: Option<std::net::IpAddr>,
        original_headers: &HeaderMap<HeaderValue>,
        proxy_headers: &mut HeaderMap<HeaderValue>,
        authoritative_geo: Option<&DidomiGeo>,
    ) {
        if let Some(ip) = client_ip {
            proxy_headers.insert(
                "X-Forwarded-For",
                HeaderValue::from_str(&ip.to_string())
                    .expect("should format X-Forwarded-For header"),
            );
        }

        // `Authorization` is intentionally NOT forwarded: it carries the
        // publisher site's own credential (e.g. staging basic-auth), which would
        // leak to the third-party upstream and can break APIs that reject an
        // unexpected `Authorization` header.
        for header_name in [
            header::ACCEPT,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_ENCODING,
            header::CONTENT_TYPE,
            header::USER_AGENT,
            header::REFERER,
            header::ORIGIN,
        ] {
            if let Some(value) = original_headers.get(&header_name) {
                proxy_headers.insert(header_name, value.clone());
            }
        }

        if matches!(backend, DidomiBackend::Sdk) {
            if let Some(geo) = authoritative_geo {
                Self::set_geo_headers(geo, proxy_headers);
            } else {
                Self::copy_geo_headers(original_headers, proxy_headers);
            }
        }
    }

    fn set_geo_headers(geo: &DidomiGeo, proxy_headers: &mut HeaderMap<HeaderValue>) {
        for (name, value) in [
            ("X-Geo-Country", geo.country.as_str()),
            ("X-Geo-Region", geo.region.as_str()),
            ("CloudFront-Viewer-Country", geo.country.as_str()),
        ] {
            proxy_headers.insert(
                name,
                HeaderValue::from_str(value).expect("should format validated Didomi geo header"),
            );
        }
    }

    fn copy_geo_headers(
        original_headers: &HeaderMap<HeaderValue>,
        proxy_headers: &mut HeaderMap<HeaderValue>,
    ) {
        let geo_headers = [
            ("X-Geo-Country", "FastlyGeo-CountryCode"),
            ("X-Geo-Region", "FastlyGeo-Region"),
            ("CloudFront-Viewer-Country", "FastlyGeo-CountryCode"),
        ];

        for (target, source) in geo_headers {
            if let Some(value) = original_headers.get(source) {
                proxy_headers.insert(target, value.clone());
            }
        }
    }

    fn add_cors_headers(response: &mut http::Response<EdgeBody>) {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, Authorization, X-Requested-With"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
    }

    fn backend_name_for_origin(
        services: &RuntimeServices,
        origin: &str,
    ) -> Result<String, Report<TrustedServerError>> {
        ensure_integration_backend(services, origin, DIDOMI_INTEGRATION_ID, None)
    }

    fn geo_failure_response(reason: &str) -> http::Response<EdgeBody> {
        log::warn!("Didomi loader geo unavailable: reason={reason}");
        let mut response = http::Response::builder()
            .status(http::StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(EdgeBody::from("Didomi loader unavailable"))
            .expect("should build static Didomi geo failure response");
        crate::response_privacy::enforce_terminal_private_cache_privacy(&mut response);
        response
    }

    fn redirect_response(
        location: &str,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let mut response = http::Response::builder()
            .status(http::StatusCode::TEMPORARY_REDIRECT)
            .header(header::LOCATION, location)
            .body(EdgeBody::empty())
            .change_context(Self::error("Failed to build Didomi geo redirect"))?;
        crate::response_privacy::enforce_terminal_private_cache_privacy(&mut response);
        Ok(response)
    }
}

fn build(
    settings: &Settings,
) -> Result<Option<Arc<DidomiIntegration>>, Report<TrustedServerError>> {
    let Some(config) =
        settings.integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    Ok(Some(DidomiIntegration::new(Arc::new(config))))
}

/// Register the Didomi consent notice integration when enabled.
///
/// # Errors
///
/// Returns an error when the Didomi integration is enabled with invalid
/// configuration.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(DIDOMI_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_head_injector(integration)
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for DidomiIntegration {
    fn integration_name(&self) -> &'static str {
        DIDOMI_INTEGRATION_ID
    }

    fn proxy_prefix(&self) -> String {
        self.resolved_prefix()
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/*"), self.post("/*")]
    }

    async fn handle(
        &self,
        _settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let (parts, body) = req.into_parts();
        let path = parts.uri.path().to_string();
        let prefix = self.resolved_prefix();
        let consent_path = path.strip_prefix(&prefix).unwrap_or(&path);
        let backend = self.backend_for_path(consent_path);
        let canonical_loader = if self.config.geo_query_parameters
            && matches!(backend, DidomiBackend::Sdk)
            && is_notice_loader(&parts.method, consent_path)
        {
            let geo = match services.geo().lookup(services.client_info().client_ip) {
                Ok(Some(geo)) => match normalize_didomi_geo(&geo) {
                    Ok(geo) => geo,
                    Err(error) => return Ok(Self::geo_failure_response(error.reason())),
                },
                Ok(None) => return Ok(Self::geo_failure_response("geo_unavailable")),
                Err(_) => return Ok(Self::geo_failure_response("lookup_failed")),
            };
            let canonical = canonical_loader_url(&path, parts.uri.query(), &geo);
            let incoming_path_and_query = parts
                .uri
                .path_and_query()
                .map_or(path.as_str(), http::uri::PathAndQuery::as_str);
            if incoming_path_and_query != canonical.browser_target {
                return Self::redirect_response(&canonical.browser_target);
            }
            Some((geo, canonical))
        } else {
            None
        };
        let base_origin = match backend {
            DidomiBackend::Sdk => self.config.sdk_origin.as_str(),
            DidomiBackend::Api => self.config.api_origin.as_str(),
        };
        let query = canonical_loader.as_ref().map_or_else(
            || parts.uri.query(),
            |(_, canonical)| Some(&*canonical.query),
        );

        let target_url = self
            .build_target_url(base_origin, consent_path, query)
            .change_context(Self::error("Failed to build Didomi target URL"))?;
        let backend_name = Self::backend_name_for_origin(services, base_origin)
            .change_context(Self::error("Failed to configure Didomi backend"))?;

        let request_body = if parts.method == Method::POST {
            let bytes =
                collect_body_bounded(body, INTEGRATION_MAX_BODY_BYTES, DIDOMI_INTEGRATION_ID)
                    .await?;
            EdgeBody::from(bytes)
        } else {
            EdgeBody::empty()
        };

        let mut proxy_req = http::Request::builder()
            .method(parts.method.clone())
            .uri(&target_url)
            .body(request_body)
            .change_context(Self::error("Failed to build Didomi proxy request"))?;
        self.copy_headers(
            &backend,
            services.client_info().client_ip,
            &parts.headers,
            proxy_req.headers_mut(),
            canonical_loader.as_ref().map(|(geo, _)| geo),
        );

        let platform_request = PlatformHttpRequest::new(proxy_req, backend_name);
        let platform_request = if matches!(backend, DidomiBackend::Api) {
            platform_request.with_cache_bypass()
        } else {
            platform_request
        };
        let mut response = services
            .http_client()
            .send(platform_request)
            .await
            .change_context(Self::error("Didomi upstream request failed"))?;

        if matches!(backend, DidomiBackend::Sdk) {
            Self::add_cors_headers(&mut response.response);
        } else {
            crate::response_privacy::enforce_terminal_private_cache_privacy(&mut response.response);
        }

        Ok(response.response)
    }
}

impl IntegrationHeadInjector for DidomiIntegration {
    fn integration_id(&self) -> &'static str {
        DIDOMI_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct InjectedDidomiClientConfig {
            proxy_path: String,
        }

        let payload = InjectedDidomiClientConfig {
            proxy_path: format!("{}/", self.resolved_prefix()),
        };

        // Escape `</` to prevent breaking out of the script tag.
        let config_json = serde_json::to_string(&payload)
            .unwrap_or_else(|e| {
                log::warn!("Didomi: failed to serialize client config: {e}");
                "{}".to_string()
            })
            .replace("</", "<\\/");

        vec![format!(
            r#"<script>window.__tsjs_didomi={config_json};</script>"#
        )]
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::platform::test_support::{
        NoopConfigStore, NoopSecretStore, StubBackend, StubHttpClient,
        build_services_with_http_client,
    };
    use crate::platform::{ClientInfo, GeoInfo, PlatformError, PlatformGeo};
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};
    use http::Method;

    enum GeoResult {
        Value(Option<GeoInfo>),
        Failure,
    }

    struct StubGeo(GeoResult);

    impl PlatformGeo for StubGeo {
        fn lookup(
            &self,
            _client_ip: Option<IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            match &self.0 {
                GeoResult::Value(geo) => Ok(geo.clone()),
                GeoResult::Failure => Err(Report::new(PlatformError::Geo)),
            }
        }
    }

    fn config(enabled: bool) -> DidomiIntegrationConfig {
        DidomiIntegrationConfig {
            enabled,
            geo_query_parameters: false,
            proxy_path: None,
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        }
    }

    fn geo_info(country: &str, region: Option<&str>) -> GeoInfo {
        GeoInfo {
            city: String::new(),
            country: country.to_string(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: region.map(str::to_string),
            asn: None,
        }
    }

    fn config_with_geo_query_parameters() -> DidomiIntegrationConfig {
        DidomiIntegrationConfig {
            geo_query_parameters: true,
            ..config(true)
        }
    }

    fn services_with_geo(
        http_client: Arc<StubHttpClient>,
        geo_result: GeoResult,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(StubBackend))
            .http_client(http_client)
            .geo(Arc::new(StubGeo(geo_result)))
            .client_info(ClientInfo {
                client_ip: Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))),
                ..ClientInfo::default()
            })
            .build()
    }

    #[test]
    fn geo_query_parameters_defaults_to_disabled() {
        let settings = Settings::from_toml(&format!(
            "{}\n[integrations.didomi]\nenabled = true\n",
            crate_test_settings_str()
        ))
        .expect("should parse Didomi configuration");

        let config = settings
            .integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)
            .expect("should read Didomi configuration")
            .expect("should enable Didomi");

        assert!(
            !config.geo_query_parameters,
            "should disable geo query parameters when omitted"
        );
    }

    #[test]
    fn geo_query_parameters_parses_explicit_opt_in() {
        let settings = Settings::from_toml(&format!(
            "{}\n[integrations.didomi]\nenabled = true\ngeo_query_parameters = true\n",
            crate_test_settings_str()
        ))
        .expect("should parse Didomi geo configuration");

        let config = settings
            .integration_config::<DidomiIntegrationConfig>(DIDOMI_INTEGRATION_ID)
            .expect("should read Didomi configuration")
            .expect("should enable Didomi");

        assert!(
            config.geo_query_parameters,
            "should retain explicit geo query parameter opt-in"
        );
    }

    #[test]
    fn matches_only_exact_get_notice_loader_paths() {
        assert!(is_notice_loader(&Method::GET, "/public-key/loader.js"));

        for (method, path) in [
            (Method::POST, "/public-key/loader.js"),
            (Method::GET, "/loader.js"),
            (Method::GET, "/api/public-key/loader.js"),
            (Method::GET, "/public-key/loader.js/"),
            (Method::GET, "/public-key/loader.js.map"),
            (Method::GET, "/nested/public-key/loader.js"),
            (Method::GET, "/public-key/other.js"),
        ] {
            assert!(
                !is_notice_loader(&method, path),
                "should reject {method} {path}"
            );
        }
    }

    #[test]
    fn normalizes_country_and_region() {
        let geo = geo_info(" us \n", Some(" ca\t"));

        assert_eq!(
            normalize_didomi_geo(&geo).expect("should normalize geo"),
            DidomiGeo {
                country: "US".to_string(),
                region: "CA".to_string(),
            },
            "should trim ASCII whitespace and uppercase both values"
        );
    }

    #[test]
    fn normalizes_matching_country_prefixed_region() {
        let geo = geo_info("us", Some("us-ca"));

        assert_eq!(
            normalize_didomi_geo(&geo).expect("should normalize prefixed region"),
            DidomiGeo {
                country: "US".to_string(),
                region: "CA".to_string(),
            },
            "should remove a matching country prefix"
        );
    }

    #[test]
    fn rejects_incomplete_or_invalid_geo() {
        for (country, region) in [
            ("", None),
            ("US", None),
            ("XX", Some("CA")),
            ("ZZ", Some("CA")),
            ("U1", Some("CA")),
            ("USA", Some("CA")),
            ("ÜS", Some("CA")),
            ("US", Some("")),
            ("US", Some("C-A")),
            ("US", Some("CAL1")),
            ("US", Some("CA!")),
            ("US", Some("GB-LND")),
        ] {
            let geo = geo_info(country, region);

            assert!(
                normalize_didomi_geo(&geo).is_err(),
                "should reject country {country:?} and region {region:?}"
            );
        }
    }

    #[test]
    fn canonicalizes_loader_query_with_authoritative_geo() {
        let geo = DidomiGeo {
            country: "US".to_string(),
            region: "CA".to_string(),
        };
        let canonical = canonical_loader_url(
            "/integrations/didomi/consent/key/loader.js",
            Some("target_type=notice&x=1&Country=gb&%72egion=lnd&x=2&empty=&space=a+b&plus=%2B"),
            &geo,
        );

        assert_eq!(
            canonical.query,
            "target_type=notice&x=1&x=2&empty=&space=a+b&plus=%2B&country=US&region=CA",
            "should preserve unrelated decoded pairs and replace all geo pairs"
        );
        assert_eq!(
            canonical.browser_target,
            "/integrations/didomi/consent/key/loader.js?target_type=notice&x=1&x=2&empty=&space=a+b&plus=%2B&country=US&region=CA",
            "should build a relative same-origin target"
        );
    }

    #[test]
    fn canonical_loader_query_is_idempotent() {
        let geo = DidomiGeo {
            country: "US".to_string(),
            region: "CA".to_string(),
        };
        let first = canonical_loader_url(
            "/integrations/didomi/consent/key/loader.js",
            Some("quote=%27&country=US&region=CA"),
            &geo,
        );
        let second = canonical_loader_url(
            "/integrations/didomi/consent/key/loader.js",
            Some(&first.query),
            &geo,
        );

        assert_eq!(second, first, "should remain stable after canonicalization");
    }

    #[test]
    fn enabled_geo_redirects_noncanonical_loader_without_upstream_call() {
        let stub = Arc::new(StubHttpClient::new());
        let services = services_with_geo(
            Arc::clone(&stub),
            GeoResult::Value(Some(geo_info("us", Some("ca")))),
        );
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config_with_geo_query_parameters()));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/key/loader.js?target_type=notice&Country=GB&region=LND")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should return redirect");

        assert_eq!(
            response.status(),
            http::StatusCode::TEMPORARY_REDIRECT,
            "should redirect to the canonical loader URL"
        );
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some(
                "/integrations/didomi/consent/key/loader.js?target_type=notice&country=US&region=CA"
            ),
            "should use a relative target with authoritative geo"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store, private"),
            "should make the redirect private and non-storable"
        );
        assert!(
            stub.recorded_backend_names().is_empty(),
            "should not contact Didomi before canonical redirect"
        );
    }

    #[test]
    fn enabled_geo_proxies_canonical_loader_with_authoritative_headers() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"loader".to_vec());
        let services = services_with_geo(
            Arc::clone(&stub),
            GeoResult::Value(Some(geo_info("us", Some("us-ca")))),
        );
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config_with_geo_query_parameters()));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/key/loader.js?target_type=notice&country=US&region=CA")
            .header("FastlyGeo-CountryCode", "GB")
            .header("FastlyGeo-Region", "LND")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should proxy canonical loader");

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            stub.recorded_request_uris(),
            vec![
                "https://sdk.privacy-center.org/key/loader.js?target_type=notice&country=US&region=CA"
            ],
            "should send the canonical query to Didomi"
        );
        let headers = stub.recorded_request_headers();
        for (name, expected) in [
            ("x-geo-country", "US"),
            ("x-geo-region", "CA"),
            ("cloudfront-viewer-country", "US"),
        ] {
            assert!(
                headers[0]
                    .iter()
                    .any(|(actual_name, value)| actual_name == name && value == expected),
                "should set {name} from authoritative geo"
            );
        }
    }

    #[test]
    fn disabled_geo_preserves_existing_loader_behavior() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"loader".to_vec());
        let services = services_with_geo(
            Arc::clone(&stub),
            GeoResult::Value(Some(geo_info("US", Some("CA")))),
        );
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/key/loader.js?target_type=notice")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should proxy loader");

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            stub.recorded_request_uris(),
            vec!["https://sdk.privacy-center.org/key/loader.js?target_type=notice"],
            "should not add geo when the option is disabled"
        );
    }

    #[test]
    fn enabled_geo_leaves_unrelated_sdk_assets_unchanged() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"sdk".to_vec());
        let services = services_with_geo(Arc::clone(&stub), GeoResult::Failure);
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config_with_geo_query_parameters()));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/sdk/v1/core.js?v=1")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should proxy unrelated SDK asset");

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            stub.recorded_request_uris(),
            vec!["https://sdk.privacy-center.org/sdk/v1/core.js?v=1"],
            "should not apply loader geo behavior to other SDK assets"
        );
    }

    #[test]
    fn enabled_geo_fails_closed_without_complete_geo() {
        for geo_result in [
            GeoResult::Value(None),
            GeoResult::Value(Some(geo_info("US", None))),
            GeoResult::Value(Some(geo_info("XX", Some("CA")))),
            GeoResult::Failure,
        ] {
            let stub = Arc::new(StubHttpClient::new());
            let services = services_with_geo(Arc::clone(&stub), geo_result);
            let settings = create_test_settings();
            let integration = DidomiIntegration::new(Arc::new(config_with_geo_query_parameters()));
            let request = http::Request::builder()
                .method(Method::GET)
                .uri("https://publisher.example/integrations/didomi/consent/key/loader.js")
                .body(EdgeBody::empty())
                .expect("should build request");

            let response =
                futures::executor::block_on(integration.handle(&settings, &services, request))
                    .expect("should return controlled geo failure");

            assert_eq!(
                response.status(),
                http::StatusCode::SERVICE_UNAVAILABLE,
                "should fail closed without complete trusted geo"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok()),
                Some("no-store, private"),
                "should make geo failures private and non-storable"
            );
            assert!(
                stub.recorded_backend_names().is_empty(),
                "should not contact Didomi on geo failure"
            );
        }
    }

    #[test]
    fn api_requests_bypass_cache_and_strip_response_cache_metadata() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"event".to_vec(),
            vec![
                ("Cache-Control", "public, max-age=3600"),
                ("Expires", "Wed, 21 Oct 2037 07:28:00 GMT"),
                ("ETag", "\"api-response\""),
                ("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
                ("Age", "120"),
                ("Surrogate-Control", "max-age=3600"),
                ("CDN-Cache-Control", "max-age=3600"),
            ],
        );
        let services = services_with_geo(Arc::clone(&stub), GeoResult::Value(None));
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/api/events?x=1")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should proxy API request");

        assert_eq!(
            stub.recorded_cache_bypass_flags(),
            vec![true],
            "should bypass the platform cache for API requests"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private"),
            "should make API responses private and non-storable"
        );
        for name in [
            header::EXPIRES.as_str(),
            header::ETAG.as_str(),
            header::LAST_MODIFIED.as_str(),
            header::AGE.as_str(),
            "surrogate-control",
            "cdn-cache-control",
        ] {
            assert!(
                response.headers().get(name).is_none(),
                "should remove API cache metadata {name}"
            );
        }
    }

    #[test]
    fn sdk_responses_preserve_origin_cache_metadata() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            b"sdk".to_vec(),
            vec![
                ("Cache-Control", "public, max-age=3600"),
                ("Expires", "Wed, 21 Oct 2037 07:28:00 GMT"),
                ("ETag", "\"sdk-response\""),
                ("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
                ("Age", "120"),
                ("Surrogate-Control", "max-age=3600"),
            ],
        );
        let services = services_with_geo(Arc::clone(&stub), GeoResult::Value(None));
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let request = http::Request::builder()
            .method(Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/sdk/v1/core.js")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response =
            futures::executor::block_on(integration.handle(&settings, &services, request))
                .expect("should proxy SDK request");

        assert_eq!(
            stub.recorded_cache_bypass_flags(),
            vec![false],
            "should retain normal platform caching for SDK requests"
        );
        for (name, expected) in [
            (header::CACHE_CONTROL.as_str(), "public, max-age=3600"),
            (header::EXPIRES.as_str(), "Wed, 21 Oct 2037 07:28:00 GMT"),
            (header::ETAG.as_str(), "\"sdk-response\""),
            (
                header::LAST_MODIFIED.as_str(),
                "Wed, 21 Oct 2015 07:28:00 GMT",
            ),
            (header::AGE.as_str(), "120"),
            ("surrogate-control", "max-age=3600"),
        ] {
            assert_eq!(
                response
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok()),
                Some(expected),
                "should preserve SDK cache metadata {name}"
            );
        }
    }

    #[test]
    fn selects_api_backend_for_api_paths() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        assert!(matches!(
            integration.backend_for_path("/api/events"),
            DidomiBackend::Api
        ));
        assert!(matches!(
            integration.backend_for_path("/24cd/loader.js"),
            DidomiBackend::Sdk
        ));
    }

    #[test]
    fn builds_target_url_with_query() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let url = integration
            .build_target_url("https://sdk.privacy-center.org", "/loader.js", Some("v=1"))
            .expect("should build target URL");
        assert_eq!(url, "https://sdk.privacy-center.org/loader.js?v=1");
    }

    #[test]
    fn registers_prefix_routes() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(DIDOMI_INTEGRATION_ID, &config(true))
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(registry.has_route(&Method::GET, "/integrations/didomi/consent/loader.js"));
        assert!(registry.has_route(&Method::POST, "/integrations/didomi/consent/api/events"));
        assert!(!registry.has_route(&Method::GET, "/other"));
    }

    #[test]
    fn copy_headers_sets_x_forwarded_for_from_client_ip() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Sdk;
        let original_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://sdk.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");
        let client_ip = Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)));

        integration.copy_headers(
            &backend,
            client_ip,
            original_req.headers(),
            proxy_req.headers_mut(),
            None,
        );

        assert_eq!(
            proxy_req
                .headers()
                .get("X-Forwarded-For")
                .and_then(|v| v.to_str().ok()),
            Some("1.2.3.4"),
            "should set X-Forwarded-For from client_ip"
        );
    }

    #[test]
    fn copy_headers_strips_authorization() {
        // Security regression guard: the publisher's Authorization header must
        // not be forwarded to the Didomi upstream (credential leak).
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Api;
        let original_req = http::Request::builder()
            .method(Method::POST)
            .uri("https://example.com/test")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .header(header::USER_AGENT, "test-agent")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::POST)
            .uri("https://api.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");

        integration.copy_headers(
            &backend,
            None,
            original_req.headers(),
            proxy_req.headers_mut(),
            None,
        );

        assert!(
            proxy_req.headers().get(header::AUTHORIZATION).is_none(),
            "should NOT forward the publisher's Authorization header to Didomi"
        );
        assert_eq!(
            proxy_req
                .headers()
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            Some("test-agent"),
            "should still forward required headers (user-agent)"
        );
    }

    #[test]
    fn registers_custom_proxy_path() {
        let mut settings = create_test_settings();
        let custom_config = DidomiIntegrationConfig {
            enabled: true,
            geo_query_parameters: false,
            proxy_path: Some("my-custom-consent".to_string()),
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        };
        settings
            .integrations
            .insert_config(DIDOMI_INTEGRATION_ID, &custom_config)
            .expect("should insert config");

        let registry = IntegrationRegistry::new(&settings).expect("should create registry");
        assert!(registry.has_route(&Method::GET, "/my-custom-consent/loader.js"));
        assert!(registry.has_route(&Method::POST, "/my-custom-consent/api/events"));
        assert!(!registry.has_route(&Method::GET, "/integrations/didomi/consent/loader.js"));
    }

    #[test]
    fn validates_proxy_path_rejects_empty() {
        assert!(validate_proxy_path("").is_err());
        assert!(validate_proxy_path("/").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_trailing_slash() {
        assert!(validate_proxy_path("my-path/").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_forbidden_chars() {
        assert!(validate_proxy_path("path?query").is_err());
        assert!(validate_proxy_path("path#frag").is_err());
        assert!(validate_proxy_path("{param}").is_err());
        assert!(validate_proxy_path("wild*card").is_err());
        assert!(validate_proxy_path("has space").is_err());
        assert!(validate_proxy_path("has\"quote").is_err());
        assert!(validate_proxy_path("has\\backslash").is_err());
        assert!(validate_proxy_path("has\nnewline").is_err());
        assert!(validate_proxy_path("encoded%2e%2e/path").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_double_slash() {
        assert!(validate_proxy_path("my//path").is_err());
    }

    #[test]
    fn validates_proxy_path_rejects_dot_segments() {
        assert!(validate_proxy_path("my/./path").is_err());
        assert!(validate_proxy_path("my/../path").is_err());
    }

    #[test]
    fn validates_proxy_path_accepts_valid() {
        assert!(validate_proxy_path("my-custom-path").is_ok());
        assert!(validate_proxy_path("nested/path/here").is_ok());
        assert!(validate_proxy_path("/leading-slash-ok").is_ok());
    }

    #[test]
    fn head_injector_emits_proxy_path() {
        let custom_config = DidomiIntegrationConfig {
            enabled: true,
            geo_query_parameters: false,
            proxy_path: Some("my-consent".to_string()),
            sdk_origin: default_sdk_origin(),
            api_origin: default_api_origin(),
        };
        let integration = DidomiIntegration::new(Arc::new(custom_config));
        let doc_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "example.com",
            document_state: &doc_state,
        };
        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 1);
        assert_eq!(
            inserts[0],
            r#"<script>window.__tsjs_didomi={"proxyPath":"/my-consent/"};</script>"#
        );
    }

    #[test]
    fn copy_headers_omits_x_forwarded_for_when_no_client_ip() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let backend = DidomiBackend::Sdk;
        let original_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .body(EdgeBody::empty())
            .expect("should build original request");
        let mut proxy_req = http::Request::builder()
            .method(Method::GET)
            .uri("https://sdk.privacy-center.org/test")
            .body(EdgeBody::empty())
            .expect("should build proxy request");

        integration.copy_headers(
            &backend,
            None,
            original_req.headers(),
            proxy_req.headers_mut(),
            None,
        );

        assert!(
            proxy_req.headers().get("X-Forwarded-For").is_none(),
            "should omit X-Forwarded-For when client_ip is None"
        );
    }

    #[test]
    fn didomi_proxy_uses_platform_http_client() {
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"ok".to_vec());
        let services = build_services_with_http_client(
            Arc::clone(&stub) as Arc<dyn crate::platform::PlatformHttpClient>
        );
        let settings = create_test_settings();
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/integrations/didomi/consent/api/events")
            .body(EdgeBody::empty())
            .expect("should build request");

        let response = futures::executor::block_on(integration.handle(&settings, &services, req))
            .expect("should proxy request");

        assert_eq!(
            response.status(),
            http::StatusCode::OK,
            "should return stubbed response"
        );
        assert_eq!(
            stub.recorded_backend_names(),
            vec!["stub-backend".to_string()],
            "should route outbound request through PlatformHttpClient"
        );
    }

    #[test]
    fn head_injector_default_path() {
        let integration = DidomiIntegration::new(Arc::new(config(true)));
        let doc_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "example.com",
            request_scheme: "https",
            origin_host: "example.com",
            document_state: &doc_state,
        };
        let inserts = integration.head_inserts(&ctx);
        assert_eq!(
            inserts[0],
            r#"<script>window.__tsjs_didomi={"proxyPath":"/integrations/didomi/consent/"};</script>"#
        );
    }
}
