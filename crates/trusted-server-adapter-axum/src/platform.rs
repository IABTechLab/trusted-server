use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use edgezero_adapter_axum::diagnostics::NativeIngressMetadata;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::geo::{GeoOutcome, GeoStatus};
use edgezero_core::http::{HeaderValue, Request, Uri, header};
use error_stack::{Report, ResultExt as _};
use trusted_server_core::http_util::RequestInfo;
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformError,
    PlatformGeo, PlatformKvStore, RuntimeServices, UnavailableKvStore,
};

pub use crate::outbound::AxumPlatformHttpClient;

/// Backend naming for native requests, whose RC transport connects directly to the URI.
pub struct AxumPlatformBackend;

impl PlatformBackend for AxumPlatformBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.naming_policy()
            .predict(spec)
            .map(|prediction| prediction.name)
            .change_context(PlatformError::Backend)
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

/// An owned snapshot of the managed native request's geographic outcome.
///
/// Provider resolution belongs to `EdgeZero` ingress. Every downstream lookup
/// returns this same request's data, never an IP lookup or shared global state.
#[derive(Clone, Default)]
pub struct AxumPlatformGeo {
    info: Option<GeoInfo>,
}

impl AxumPlatformGeo {
    fn from_context(context: &RequestContext) -> Self {
        Self {
            info: geo_from_outcome(context.geo()),
        }
    }
}

impl PlatformGeo for AxumPlatformGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(self.info.clone())
    }
}

/// Project only the private managed outcome, without reading request headers.
/// Unavailable, untrusted, error and unselected outcomes supply no location.
pub(crate) fn geo_from_outcome(outcome: &GeoOutcome) -> Option<GeoInfo> {
    match outcome {
        GeoOutcome::Selected {
            status: GeoStatus::Located { location, .. },
            ..
        } => Some(GeoInfo::country_region(
            location.country().as_str(),
            location
                .subdivision()
                .map(|value| value.as_str().to_owned()),
        )),
        _ => None,
    }
}

/// Install checked public URL facts without consuming or replacing the request body.
///
/// Raw forwarding headers are never consulted. Managed ingress validates proxy
/// trust and syntax before creating this metadata. Requests outside the native
/// runner remain unchanged, for explicit-services router tests and other adapters.
///
/// # Errors
/// Returns a bad-request error when checked public URI/Host facts cannot be represented.
pub(crate) fn apply_native_ingress(request: &mut Request) -> Result<(), EdgeError> {
    let Some(metadata) = request.extensions().get::<NativeIngressMetadata>() else {
        return Ok(());
    };
    let info = RequestInfo {
        host: metadata
            .effective_host()
            .authority()
            .unwrap_or_default()
            .to_owned(),
        scheme: metadata.effective_scheme().as_str().to_owned(),
    };
    // Some core consumers read URI/Host directly rather than RequestInfo.
    // Normalize all three representations from the same checked facts so an
    // absolute request target cannot override the selected public scheme.
    let uri = Uri::builder()
        .scheme(info.scheme.as_str())
        .authority(info.host.as_str())
        .path_and_query(
            request
                .uri()
                .path_and_query()
                .map_or("/", |path| path.as_str()),
        )
        .build()
        .map_err(|_| {
            EdgeError::bad_request("native request has no representable checked public URI")
        })?;
    let host = HeaderValue::from_str(&info.host).map_err(|_| {
        EdgeError::bad_request("native request has no representable checked public host")
    })?;
    *request.uri_mut() = uri;
    request.headers_mut().insert(header::HOST, host);
    request.extensions_mut().insert(info);
    Ok(())
}

/// Construct request-local runtime services from initialized immutable stores.
///
/// The RC client and application clock come from the incoming request. Only
/// dependency handles are shared; checked visitor metadata is read afresh.
/// Generic KV remains unavailable. Geography is captured before converting the
/// managed context back into an ordinary request and never resolved again.
///
/// # Errors
/// Returns an error when startup has not installed the store snapshot or the
/// native runner has not supplied its HTTP client. There is no environment fallback.
pub fn build_runtime_services(
    context: &RequestContext,
) -> Result<RuntimeServices, Report<PlatformError>> {
    let snapshot = context
        .extensions()
        .get::<Arc<crate::startup::NativeStoreSnapshot>>()
        .ok_or_else(|| {
            Report::new(PlatformError::ConfigStore)
                .attach("native prepared-store snapshot is missing; initialize the application before serving")
        })?;
    let client = context.http_client().ok_or_else(|| {
        Report::new(PlatformError::HttpClient)
            .attach("request-scoped native HTTP client is missing")
    })?;
    let client_ip = context
        .extensions()
        .get::<NativeIngressMetadata>()
        .and_then(NativeIngressMetadata::effective_client);

    static KV: OnceLock<Arc<dyn PlatformKvStore>> = OnceLock::new();
    static BACKEND: OnceLock<Arc<dyn PlatformBackend>> = OnceLock::new();

    Ok(RuntimeServices::builder()
        .config_store(Arc::clone(&snapshot.config_store))
        .secret_store(Arc::clone(&snapshot.secret_store))
        .kv_store(Arc::clone(KV.get_or_init(|| Arc::new(UnavailableKvStore))))
        .backend(Arc::clone(
            BACKEND.get_or_init(|| Arc::new(AxumPlatformBackend)),
        ))
        .http_client(Arc::new(AxumPlatformHttpClient::new(
            client,
            context.monotonic_clock(),
        )))
        .geo(Arc::new(AxumPlatformGeo::from_context(context)))
        .client_info(ClientInfo {
            client_ip,
            ..ClientInfo::default()
        })
        .build())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    use super::*;

    #[test]
    fn backend_ensure_matches_prediction() {
        let backend = AxumPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_owned(),
            host: "example.com".to_owned(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            host_header_override: None,
            discriminator: None,
        };
        assert_eq!(
            backend.predict_name(&spec).expect("should predict backend"),
            backend.ensure(&spec).expect("should ensure backend"),
            "should preserve deterministic native backend names"
        );
    }

    fn located(country: &str, region: Option<&str>) -> GeoOutcome {
        use edgezero_core::geo::{
            CountryCode, GeoLocation, GeoProviderId, GeoSource, GeoTrustBasis, SubdivisionCode,
        };
        GeoOutcome::Selected {
            provider: GeoProviderId::cloudfront(),
            status: GeoStatus::Located {
                location: GeoLocation::new(
                    CountryCode::new(country).expect("should parse fixture country"),
                    region.map(|value| {
                        SubdivisionCode::new(value).expect("should parse fixture subdivision")
                    }),
                ),
                source: GeoSource::CloudFront,
                trust_basis: GeoTrustBasis::DeclaredProtectedAlb,
            },
        }
    }

    #[test]
    fn geography_is_partial_and_request_local() {
        let first = AxumPlatformGeo {
            info: geo_from_outcome(&located("US", Some("CA"))),
        };
        let second = AxumPlatformGeo {
            info: geo_from_outcome(&located("GB", None)),
        };
        let us = first
            .lookup(None)
            .expect("should read first snapshot")
            .expect("should retain US");
        let gb = second
            .lookup(None)
            .expect("should read second snapshot")
            .expect("should retain GB");
        assert_eq!(us.country, "US");
        assert_eq!(us.region.as_deref(), Some("CA"));
        assert_eq!(gb.country, "GB");
        assert_eq!(gb.region, None);
        assert_eq!(us.coordinates, None);
        assert_eq!(us.city, None);
        assert_eq!(us.continent, None);
        assert_eq!(us.metro_code, None);
        assert_eq!(
            first
                .lookup(Some(
                    "192.0.2.1".parse().expect("should parse different IP")
                ))
                .expect("should reuse same snapshot"),
            Some(us),
            "should not resolve geography from another lookup argument"
        );
    }

    #[test]
    fn unavailable_outcomes_and_forged_extensions_cannot_supply_geography() {
        use edgezero_core::geo::{GeoError, GeoProviderId, GeoUnavailableReason};
        for status in [
            GeoStatus::Unavailable(GeoUnavailableReason::MissingMetadata),
            GeoStatus::Unavailable(GeoUnavailableReason::UntrustedIngress),
            GeoStatus::Error(GeoError::InvalidAssertion),
            GeoStatus::Error(GeoError::ProviderFailure),
        ] {
            assert!(
                geo_from_outcome(&GeoOutcome::Selected {
                    provider: GeoProviderId::cloudfront(),
                    status,
                })
                .is_none()
            );
        }
        let mut request = request_builder()
            .header("cloudfront-viewer-country", "US")
            .header("cloudfront-viewer-country-region", "CA")
            .body(Body::empty())
            .expect("should build unmanaged request");
        request.extensions_mut().insert(located("US", Some("CA")));
        let context = RequestContext::new(request, edgezero_core::params::PathParams::default());
        assert!(
            AxumPlatformGeo::from_context(&context)
                .lookup(None)
                .expect("should read unmanaged outcome")
                .is_none(),
            "should ignore raw headers and public extensions"
        );
    }

    #[test]
    fn missing_prepared_snapshot_fails_without_environment_fallback() {
        let request = request_builder()
            .body(Body::empty())
            .expect("should build request");
        let context = RequestContext::new(request, edgezero_core::params::PathParams::default());
        assert!(
            build_runtime_services(&context).is_err(),
            "should require prepared stores"
        );
    }

    #[test]
    fn unmanaged_test_request_is_not_rewritten_from_raw_forwarding_headers() {
        let mut request = request_builder()
            .header("forwarded", "host=spoof.example.com;proto=https")
            .body(Body::from("body"))
            .expect("should build request");
        apply_native_ingress(&mut request).expect("should retain unmanaged test request");
        assert!(request.extensions().get::<RequestInfo>().is_none());
        assert_eq!(request.body().as_bytes(), Some(b"body".as_slice()));
    }
}
