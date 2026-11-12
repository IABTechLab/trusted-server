use std::net::IpAddr;
use std::sync::Arc;

use error_stack::{Report, ResultExt as _};
use trusted_server_core::platform::{
    AuctionTargetId, BackendNamingPolicy, ClientInfo, EdgeZeroHttpClient, GeoInfo, PlatformBackend,
    PlatformBackendSpec, PlatformConfigStore, PlatformError, PlatformGeo, PlatformKvStore,
    PlatformSecretStore, RuntimeServices, StoreId, StoreName,
};

// ---------------------------------------------------------------------------
// Env-var naming helpers
// ---------------------------------------------------------------------------

/// Normalize a store name or key for use as an environment-variable segment.
///
/// Uppercases and replaces hyphens, dots, and spaces with underscores.
fn normalize_env_segment(s: &str) -> String {
    s.to_uppercase().replace(['-', '.', ' '], "_")
}

/// Returns the environment-variable name for a config store entry.
#[must_use]
pub fn config_env_var(store_name: &str, key: &str) -> String {
    format!(
        "TRUSTED_SERVER_CONFIG_{}_{}",
        normalize_env_segment(store_name),
        normalize_env_segment(key),
    )
}

fn secret_env_var(store_name: &str, key: &str) -> String {
    format!(
        "TRUSTED_SERVER_SECRET_{}_{}",
        normalize_env_segment(store_name),
        normalize_env_segment(key),
    )
}

// ---------------------------------------------------------------------------
// PlatformConfigStore
// ---------------------------------------------------------------------------

/// Environment-variable–backed config store for the Axum dev server.
///
/// Reads from `TRUSTED_SERVER_CONFIG_{STORE}_{KEY}` (uppercased, hyphens→underscores).
/// Write operations are unsupported in local development.
pub struct AxumPlatformConfigStore;

impl PlatformConfigStore for AxumPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let var_name = config_env_var(store_name.as_ref(), key);
        std::env::var(&var_name).map_err(|_| {
            Report::new(PlatformError::ConfigStore).attach(format!(
                "env var '{var_name}' not set — export it to supply this config value"
            ))
        })
    }

    fn put(
        &self,
        store_id: &StoreId,
        key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformConfigStore: write to store '{}' key '{}' ignored \
             (config store writes are not supported on the Axum dev server)",
            store_id.as_ref(),
            key
        );
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on the Axum dev server"))
    }

    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformConfigStore: delete from store '{}' key '{}' ignored \
             (config store deletes are not supported on the Axum dev server)",
            store_id.as_ref(),
            key
        );
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store deletes are not supported on the Axum dev server"))
    }
}

// ---------------------------------------------------------------------------
// PlatformSecretStore
// ---------------------------------------------------------------------------

/// Environment-variable–backed secret store for the Axum dev server.
///
/// Reads from `TRUSTED_SERVER_SECRET_{STORE}_{KEY}` as raw UTF-8 bytes.
/// Write operations are unsupported in local development.
pub struct AxumPlatformSecretStore;

impl PlatformSecretStore for AxumPlatformSecretStore {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let var_name = secret_env_var(store_name.as_ref(), key);
        std::env::var(&var_name)
            .map(String::into_bytes)
            .map_err(|_| {
                Report::new(PlatformError::SecretStore).attach(format!(
                    "env var '{var_name}' not set — export it to supply this secret value"
                ))
            })
    }

    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformSecretStore: create '{}' in store '{}' ignored \
             (secret store writes are not supported on the Axum dev server)",
            name,
            store_id.as_ref()
        );
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on the Axum dev server"))
    }

    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
        log::warn!(
            "AxumPlatformSecretStore: delete '{}' from store '{}' ignored \
             (secret store deletes are not supported on the Axum dev server)",
            name,
            store_id.as_ref()
        );
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store deletes are not supported on the Axum dev server"))
    }
}

// ---------------------------------------------------------------------------
// PlatformBackend
// ---------------------------------------------------------------------------

/// No-op backend for the Axum dev server.
///
/// Returns a deterministic name; `ensure` is a no-op returning the same name.
/// The Axum HTTP client sends directly to URIs and ignores backend names.
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

/// Construct [`RuntimeServices`] for an incoming Axum request.
///
/// # Degraded features in dev
///
/// The generic runtime KV slot uses
/// [`trusted_server_core::platform::UnavailableKvStore`]. A `warn` log is
/// emitted once per process.
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    static KV_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    KV_WARNED.get_or_init(|| {
        log::warn!("Axum dev server: generic runtime KV is unavailable (UnavailableKvStore).");
    });

    let client_ip = ctx
        .extensions()
        .get::<edgezero_adapter_axum::context::AxumRequestContext>()
        .and_then(|c| c.remote_addr)
        .map(|addr| addr.ip());

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static CONFIG_STORE: std::sync::OnceLock<Arc<dyn PlatformConfigStore>> =
        std::sync::OnceLock::new();
    static SECRET_STORE: std::sync::OnceLock<Arc<dyn PlatformSecretStore>> =
        std::sync::OnceLock::new();
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();
    static BACKEND: std::sync::OnceLock<Arc<dyn PlatformBackend>> = std::sync::OnceLock::new();
    static GEO: std::sync::OnceLock<Arc<dyn PlatformGeo>> = std::sync::OnceLock::new();

    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();

    RuntimeServices::builder()
        .timer(Arc::clone(TIMER.get_or_init(|| Arc::new(AxumTimer))))
        .config_store(Arc::clone(CONFIG_STORE.get_or_init(|| {
            Arc::new(AxumPlatformConfigStore) as Arc<dyn PlatformConfigStore>
        })))
        .secret_store(Arc::clone(SECRET_STORE.get_or_init(|| {
            Arc::new(AxumPlatformSecretStore) as Arc<dyn PlatformSecretStore>
        })))
        .kv_store(Arc::clone(KV_STORE.get_or_init(|| {
            Arc::new(trusted_server_core::platform::UnavailableKvStore) as Arc<dyn PlatformKvStore>
        })))
        .backend(Arc::clone(BACKEND.get_or_init(|| {
            Arc::new(AxumPlatformBackend) as Arc<dyn PlatformBackend>
        })))
        .http_client(EdgeZeroHttpClient::for_request(ctx, AuctionTargetId::Axum))
        .geo(Arc::clone(GEO.get_or_init(|| {
            Arc::new(AxumPlatformGeo) as Arc<dyn PlatformGeo>
        })))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
            ..ClientInfo::default()
        })
        .build()
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
    use std::time::Duration;

    #[test]
    fn html_finalization_timer_is_shared_without_an_executor() {
        let request = edgezero_core::http::request_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build a request");
        let context = edgezero_core::context::RequestContext::new(request, Default::default());
        let services = build_runtime_services(&context);
        let next_services = build_runtime_services(&context);
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
    fn auction_http_capabilities_are_explicit() {
        let capabilities = trusted_server_core::platform::AuctionTargetId::Axum
            .descriptor()
            .capabilities();
        assert!(capabilities.supports_concurrent_provider_fanout());
    }

    #[test]
    fn config_env_var_normalizes_store_and_key() {
        assert_eq!(
            config_env_var("my-store.name", "my key"),
            "TRUSTED_SERVER_CONFIG_MY_STORE_NAME_MY_KEY",
            "should normalize environment-variable segments"
        );
    }

    #[test]
    fn config_store_reads_from_env_var() {
        temp_env::with_var(
            "TRUSTED_SERVER_CONFIG_MY_STORE_MY_KEY",
            Some("test-value"),
            || {
                let store = AxumPlatformConfigStore;
                let result = store
                    .get(&StoreName::from("my-store"), "my-key")
                    .expect("should read env var");
                assert_eq!(result, "test-value", "should return env var value");
            },
        );
    }

    #[test]
    fn config_store_returns_error_for_missing_env_var() {
        let store = AxumPlatformConfigStore;
        let result = store.get(
            &StoreName::from("nonexistent-store-zzz"),
            "nonexistent-key-zzz",
        );
        assert!(result.is_err(), "should error for missing env var");
    }

    #[test]
    fn secret_store_reads_bytes_from_env_var() {
        temp_env::with_var(
            "TRUSTED_SERVER_SECRET_MY_SECRETS_MY_SECRET",
            Some("hello"),
            || {
                let store = AxumPlatformSecretStore;
                let result = store
                    .get_bytes(&StoreName::from("my-secrets"), "my-secret")
                    .expect("should read env var as bytes");
                assert_eq!(result, b"hello", "should return raw bytes");
            },
        );
    }

    #[test]
    fn backend_predict_name_returns_deterministic_string() {
        let backend = AxumPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            host_header_override: None,
            discriminator: None,
        };
        let name1 = backend.predict_name(&spec).expect("should return a name");
        let name2 = backend
            .predict_name(&spec)
            .expect("should return same name");
        assert!(!name1.is_empty(), "should return a non-empty name");
        assert_eq!(name1, name2, "should be deterministic");
    }

    #[test]
    fn backend_ensure_returns_same_name_as_predict() {
        let backend = AxumPlatformBackend;
        let spec = PlatformBackendSpec {
            scheme: "https".to_string(),
            host: "example.com".to_string(),
            port: None,
            certificate_check: true,
            first_byte_timeout: Duration::from_secs(15),
            between_bytes_timeout: Duration::from_secs(15),
            host_header_override: None,
            discriminator: None,
        };
        assert_eq!(
            backend.predict_name(&spec).expect("should return name"),
            backend.ensure(&spec).expect("should return name"),
            "ensure should equal predict_name"
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
