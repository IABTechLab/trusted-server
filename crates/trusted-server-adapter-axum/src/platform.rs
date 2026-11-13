use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_adapter_axum::diagnostics::NativeIngressMetadata;
use edgezero_core::error::EdgeError;
use edgezero_core::geo::{GeoOutcome, GeoStatus};
use error_stack::Report;
use trusted_server_core::outbound::request_outbound_client;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformError, PlatformGeo, PlatformKvStore, RuntimeServices,
};

// ---------------------------------------------------------------------------
// PlatformGeo
// ---------------------------------------------------------------------------

/// An owned snapshot of the managed native request's geographic outcome.
///
/// Provider resolution belongs to `EdgeZero` ingress. Every downstream lookup
/// returns this same request's data, never an IP lookup or shared global state.
#[derive(Clone, Default)]
pub struct AxumPlatformGeo {
    info: Option<GeoInfo>,
}

impl AxumPlatformGeo {
    fn from_context(context: &edgezero_core::context::RequestContext) -> Self {
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

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct request-scoped services from the immutable prepared native snapshots.
///
/// # Degraded features in dev
///
/// The generic runtime KV slot uses
/// [`trusted_server_core::platform::UnavailableKvStore`]. A `warn` log is
/// emitted once per process.
///
/// The outbound client is the one the request context carries, with the proxy
/// marker turned off, and the clock is the context's monotonic clock.
/// Geography is a request-local snapshot of the private managed outcome.
///
/// # Errors
///
/// Returns an internal [`EdgeError`] when prepared snapshots or the request's
/// outbound client are absent. No legacy environment store is consulted.
pub fn build_runtime_services(
    ctx: &edgezero_core::context::RequestContext,
) -> Result<RuntimeServices, EdgeError> {
    let snapshot = ctx
        .extensions()
        .get::<Arc<crate::startup::NativeStoreSnapshot>>()
        .ok_or_else(|| {
            EdgeError::internal(std::io::Error::other(
                "request has no prepared native store snapshot",
            ))
        })?;
    static KV_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KV_WARNED.get_or_init(|| {
        log::warn!("Axum dev server: generic runtime KV is unavailable (UnavailableKvStore).");
    });

    let client_ip = match ctx.extensions().get::<NativeIngressMetadata>() {
        Some(metadata) => metadata.effective_client(),
        None => ctx
            .extensions()
            .get::<AxumRequestContext>()
            .and_then(|context| context.remote_addr)
            .map(|address| address.ip()),
    };

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();

    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();

    let outbound_client = request_outbound_client(ctx)?;

    Ok(RuntimeServices::builder()
        .timer(Arc::clone(TIMER.get_or_init(|| Arc::new(AxumTimer))))
        .config_store(Arc::clone(&snapshot.config_store))
        .secret_store(Arc::clone(&snapshot.secret_store))
        .kv_store(Arc::clone(KV_STORE.get_or_init(|| {
            Arc::new(trusted_server_core::platform::UnavailableKvStore) as Arc<dyn PlatformKvStore>
        })))
        .outbound_client(outbound_client)
        .clock(ctx.monotonic_clock())
        .geo(Arc::new(AxumPlatformGeo::from_context(ctx)))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
            ..ClientInfo::default()
        })
        .build())
}

struct AxumTimer;

impl trusted_server_core::platform::PlatformTimer for AxumTimer {
    fn delay(&self, duration: std::time::Duration) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async move {
            tokio::time::sleep(duration).await;
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body;
    use edgezero_core::context::RequestContext;
    use edgezero_core::geo::{
        CountryCode, GeoError, GeoLocation, GeoProviderId, GeoSource, GeoTrustBasis,
        GeoUnavailableReason, SubdivisionCode,
    };
    use edgezero_core::http::request_builder;
    use trusted_server_core::platform::HttpClient;
    use trusted_server_core::test_support::scripted_client::ScriptedHttpClient;

    /// Builds a context whose request carries `client`, as `EdgeZero`'s Axum
    /// request conversion installs its default client.
    fn context_with_client(client: Option<HttpClient>) -> edgezero_core::context::RequestContext {
        let mut request = edgezero_core::http::request_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build a request");
        request
            .extensions_mut()
            .insert(Arc::new(crate::startup::NativeStoreSnapshot::empty()));
        if let Some(client) = client {
            request.extensions_mut().insert(client);
        }
        edgezero_core::context::RequestContext::new(request, Default::default())
    }

    #[test]
    fn runtime_services_carry_the_context_client_with_the_proxy_marker_off() {
        let client = HttpClient::with_client(ScriptedHttpClient::forbidding());
        assert!(
            client.proxy_marker_enabled(),
            "should start from a client whose marker is on"
        );
        let context = context_with_client(Some(client));

        let services = build_runtime_services(&context).expect("should build runtime services");

        assert!(
            !services.outbound_client().proxy_marker_enabled(),
            "should turn the proxy marker off on the context client"
        );
    }

    #[test]
    fn runtime_services_reject_a_request_without_an_outbound_client() {
        let context = context_with_client(None);

        let error = build_runtime_services(&context)
            .expect_err("should reject a request with no outbound client");

        assert_eq!(
            error.status(),
            edgezero_core::http::StatusCode::INTERNAL_SERVER_ERROR,
            "should answer a missing client with an internal error"
        );
        assert_eq!(
            error.to_string(),
            "internal error: request has no outbound client",
            "should name the missing client"
        );
    }

    fn located(country: &str, region: Option<&str>) -> GeoOutcome {
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
    fn html_finalization_timer_is_shared_without_an_executor() {
        let context = context_with_client(Some(ScriptedHttpClient::forbidding().http_client()));
        let services = build_runtime_services(&context).expect("should build runtime services");
        let next_services =
            build_runtime_services(&context).expect("should build runtime services again");
        assert!(
            std::ptr::eq(
                services
                    .timer()
                    .expect("should install a cooperative timer"),
                next_services
                    .timer()
                    .expect("should reuse the cooperative timer"),
            ),
            "should share the stateless timer without requiring an executor"
        );
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
    fn runtime_services_reject_a_request_without_prepared_snapshots() {
        let request = edgezero_core::http::request_builder()
            .body(edgezero_core::Body::empty())
            .expect("should build a request");
        let context = edgezero_core::context::RequestContext::new(request, Default::default());
        let error = build_runtime_services(&context)
            .expect_err("should not fall back to legacy environment stores");
        assert_eq!(
            error.to_string(),
            "internal error: request has no prepared native store snapshot"
        );
    }

    #[test]
    fn geo_always_returns_none() {
        let geo = AxumPlatformGeo::default();
        let no_ip = geo.lookup(None).expect("should not error");
        assert!(no_ip.is_none(), "should return None for no IP");
        let with_ip = geo
            .lookup(Some("127.0.0.1".parse().expect("should parse IP")))
            .expect("should not error");
        assert!(with_ip.is_none(), "should return None for any IP");
    }
}
