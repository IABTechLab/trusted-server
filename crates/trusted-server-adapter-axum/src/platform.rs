use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use edgezero_adapter_axum::diagnostics::NativeIngressMetadata;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
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

/// Reader geography is unavailable on native, regardless of deployment region.
pub struct AxumPlatformGeo;

impl PlatformGeo for AxumPlatformGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
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
/// Generic KV and reader geography are intentionally unavailable in this demo.
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
    static GEO: OnceLock<Arc<dyn PlatformGeo>> = OnceLock::new();

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
        .geo(Arc::clone(GEO.get_or_init(|| Arc::new(AxumPlatformGeo))))
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

    #[test]
    fn reader_geography_remains_unavailable() {
        assert!(
            AxumPlatformGeo
                .lookup(None)
                .expect("should look up geography")
                .is_none()
        );
        assert!(
            AxumPlatformGeo
                .lookup(Some("192.0.2.1".parse().expect("should parse client IP")))
                .expect("should look up geography")
                .is_none()
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
