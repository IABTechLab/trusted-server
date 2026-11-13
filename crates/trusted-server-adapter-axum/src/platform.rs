use std::net::IpAddr;
use std::sync::Arc;

use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_core::error::EdgeError;
use error_stack::Report;
use trusted_server_core::outbound::request_outbound_client;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformError, PlatformGeo, PlatformKvStore, RuntimeServices,
};

// ---------------------------------------------------------------------------
// PlatformGeo
// ---------------------------------------------------------------------------

/// No-op geo implementation — geographic lookup is unavailable in local development.
pub struct AxumPlatformGeo;

impl PlatformGeo for AxumPlatformGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
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

    let client_ip = ctx
        .extensions()
        .get::<AxumRequestContext>()
        .and_then(|c| c.remote_addr)
        .map(|addr| addr.ip());

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();
    static GEO: std::sync::OnceLock<Arc<dyn PlatformGeo>> = std::sync::OnceLock::new();

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
        .geo(Arc::clone(GEO.get_or_init(|| {
            Arc::new(AxumPlatformGeo) as Arc<dyn PlatformGeo>
        })))
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
        let geo = AxumPlatformGeo;
        let no_ip = geo.lookup(None).expect("should not error");
        assert!(no_ip.is_none(), "should return None for no IP");
        let with_ip = geo
            .lookup(Some("127.0.0.1".parse().expect("should parse IP")))
            .expect("should not error");
        assert!(with_ip.is_none(), "should return None for any IP");
    }
}
