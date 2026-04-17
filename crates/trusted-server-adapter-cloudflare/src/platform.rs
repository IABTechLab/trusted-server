use std::net::IpAddr;
use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore, PlatformError,
    PlatformGeo, PlatformSecretStore, RuntimeServices, StoreId, StoreName, UnavailableHttpClient,
    UnavailableKvStore,
};

// ---------------------------------------------------------------------------
// Noop stubs for host-target builds (native CI, unit tests)
// ---------------------------------------------------------------------------

struct NoopConfigStore;

impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }

    fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("unavailable on host target"))
    }
}

struct NoopSecretStore;

impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("unavailable on host target"))
    }
}

struct NoopBackend;

impl PlatformBackend for NoopBackend {
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        Ok(format!("{}_{}", spec.scheme, spec.host))
    }

    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

struct NoopGeo;

impl PlatformGeo for NoopGeo {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Cloudflare Workers request.
///
/// On native (host target, CI), all platform services degrade gracefully via
/// noop stubs. The Cloudflare Workers runtime uses the same path — actual KV,
/// config, and secret access is mediated by the edgezero dispatch layer via
/// handles, not through these platform trait impls.
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    let client_ip = extract_client_ip(ctx);

    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(NoopBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(NoopGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}

fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    ctx.request()
        .headers()
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}
