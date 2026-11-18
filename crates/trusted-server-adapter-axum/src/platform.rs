use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edgezero_adapter_axum::context::AxumRequestContext;
use edgezero_adapter_axum::key_value_store::PersistentKvStore;
use edgezero_core::error::EdgeError;
use edgezero_core::key_value_store::{KvHandle, KvPage};
use error_stack::Report;
use trusted_server_core::outbound::request_outbound_client;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, KvError, KvHandleStore, PlatformConfigStore, PlatformError, PlatformGeo,
    PlatformKvStore, PlatformSecretStore, RuntimeServices, StoreId, StoreName, UnavailableKvStore,
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

#[async_trait(?Send)]
impl PlatformConfigStore for AxumPlatformConfigStore {
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        let var_name = config_env_var(store_name.as_ref(), key);
        std::env::var(&var_name).map_err(|_| {
            Report::new(PlatformError::ConfigStore).attach(format!(
                "env var '{var_name}' not set — export it to supply this config value"
            ))
        })
    }

    async fn put(
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

    async fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
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

#[async_trait(?Send)]
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

    async fn create(
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

    async fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>> {
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
// Runtime KV
// ---------------------------------------------------------------------------

/// Environment variable naming the file that backs the generic runtime KV store.
///
/// Unset, the store is unavailable. The adapter does not declare the store to
/// `EdgeZero`, whose Axum server would treat it as required and refuse to start
/// without a writable local database.
pub const KV_PATH_ENV_VAR: &str = "TRUSTED_SERVER_KV_PATH";

/// Runs a [`PersistentKvStore`]'s synchronous database I/O off the async task.
///
/// Inside a Tokio runtime each operation runs on the blocking pool, so a slow
/// write cannot stall other requests; outside one it runs inline. An operation
/// whose blocking task panics or is cancelled answers `KvError::Unavailable`.
struct BlockingKvStore(Arc<PersistentKvStore>);

impl BlockingKvStore {
    async fn run<T, F, Fut>(&self, operation: F) -> Result<T, KvError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<PersistentKvStore>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, KvError>>,
    {
        let store = Arc::clone(&self.0);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime
                .spawn_blocking(move || futures::executor::block_on(operation(store)))
                .await
                .unwrap_or_else(|error| {
                    log::error!("Axum: runtime KV operation did not complete: {error}");
                    Err(KvError::Unavailable)
                }),
            Err(_) => operation(store).await,
        }
    }
}

#[async_trait(?Send)]
impl PlatformKvStore for BlockingKvStore {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        let key = key.to_owned();
        self.run(move |store| async move { store.get_bytes(&key).await })
            .await
    }

    async fn exists(&self, key: &str) -> Result<bool, KvError> {
        let key = key.to_owned();
        self.run(move |store| async move { store.exists(&key).await })
            .await
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        let key = key.to_owned();
        self.run(move |store| async move { store.put_bytes(&key, value).await })
            .await
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: Duration,
    ) -> Result<(), KvError> {
        let key = key.to_owned();
        self.run(move |store| async move { store.put_bytes_with_ttl(&key, value, ttl).await })
            .await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        let key = key.to_owned();
        self.run(move |store| async move { store.delete(&key).await })
            .await
    }

    async fn list_keys_page(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<KvPage, KvError> {
        let prefix = prefix.to_owned();
        let cursor = cursor.map(str::to_owned);
        self.run(move |store| async move {
            store
                .list_keys_page(&prefix, cursor.as_deref(), limit)
                .await
        })
        .await
    }
}

/// Opens the runtime KV store backed by the database file at `path`.
///
/// The store sits behind a `KvHandle`, so key, value, TTL and list-limit
/// validation matches the other adapters before any database I/O.
///
/// Without a path, or when the file cannot be opened (a missing parent
/// directory, a lock held by another process or a corrupt file), the store is
/// unavailable: each KV operation answers `KvError::Unavailable`, a warning is
/// logged, and requests are still served.
fn kv_store_from_path(path: Option<&Path>) -> Arc<dyn PlatformKvStore> {
    let Some(path) = path else {
        log::warn!("Axum: generic runtime KV is unavailable; set {KV_PATH_ENV_VAR} to enable it");
        return Arc::new(UnavailableKvStore);
    };
    match PersistentKvStore::new(path) {
        Ok(store) => {
            log::info!("Axum: generic runtime KV store at {}", path.display());
            Arc::new(KvHandleStore::new(KvHandle::new(Arc::new(
                BlockingKvStore(Arc::new(store)),
            ))))
        }
        Err(error) => {
            log::warn!(
                "Axum: generic runtime KV is unavailable; cannot open {}: {error}",
                path.display()
            );
            Arc::new(UnavailableKvStore)
        }
    }
}

/// Opens the runtime KV store named by [`KV_PATH_ENV_VAR`].
fn kv_store_from_env() -> Arc<dyn PlatformKvStore> {
    let path = std::env::var_os(KV_PATH_ENV_VAR).map(PathBuf::from);
    kv_store_from_path(path.as_deref())
}

/// Returns the process-wide runtime KV store, opening it on the first call.
///
/// Startup calls this while assembling the app, before the listener serves a
/// request, so a slow database open or repair never stalls a request.
pub(crate) fn runtime_kv_store() -> Arc<dyn PlatformKvStore> {
    static KV_STORE: std::sync::OnceLock<Arc<dyn PlatformKvStore>> = std::sync::OnceLock::new();
    Arc::clone(KV_STORE.get_or_init(kv_store_from_env))
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Axum request.
///
/// # Runtime KV
///
/// The generic runtime KV store is the file [`KV_PATH_ENV_VAR`] names, opened
/// once per process during startup and shared by every request. Unset or
/// unopenable, it is [`UnavailableKvStore`], with a `warn` log once per
/// process.
///
/// The outbound client is the one the request context carries, with the proxy
/// marker turned off, and the clock is the context's monotonic clock.
///
/// # Errors
///
/// Returns an internal [`EdgeError`] when the request carries no outbound
/// client.
pub fn build_runtime_services(
    ctx: &edgezero_core::context::RequestContext,
) -> Result<RuntimeServices, EdgeError> {
    let client_ip = ctx
        .extensions()
        .get::<AxumRequestContext>()
        .and_then(|c| c.remote_addr)
        .map(|addr| addr.ip());

    // Stateless shims are promoted to process-wide statics so callers clone
    // an existing Arc instead of allocating a new one per request.
    static CONFIG_STORE: std::sync::OnceLock<Arc<dyn PlatformConfigStore>> =
        std::sync::OnceLock::new();
    static SECRET_STORE: std::sync::OnceLock<Arc<dyn PlatformSecretStore>> =
        std::sync::OnceLock::new();
    static GEO: std::sync::OnceLock<Arc<dyn PlatformGeo>> = std::sync::OnceLock::new();

    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();

    let outbound_client = request_outbound_client(ctx)?;

    Ok(RuntimeServices::builder()
        .timer(Arc::clone(TIMER.get_or_init(|| Arc::new(AxumTimer))))
        .config_store(Arc::clone(CONFIG_STORE.get_or_init(|| {
            Arc::new(AxumPlatformConfigStore) as Arc<dyn PlatformConfigStore>
        })))
        .secret_store(Arc::clone(SECRET_STORE.get_or_init(|| {
            Arc::new(AxumPlatformSecretStore) as Arc<dyn PlatformSecretStore>
        })))
        .kv_store(runtime_kv_store())
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

    fn put_then_get(store: &dyn PlatformKvStore, key: &str) -> Option<Bytes> {
        futures::executor::block_on(store.put_bytes(key, Bytes::from_static(b"example-value")))
            .expect("should write the runtime KV store");
        futures::executor::block_on(store.get_bytes(key)).expect("should read the runtime KV store")
    }

    #[test]
    fn runtime_kv_round_trips_outside_a_tokio_runtime() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let store = kv_store_from_path(Some(&directory.path().join("runtime-kv.redb")));

        assert_eq!(
            put_then_get(store.as_ref(), "example-key"),
            Some(Bytes::from_static(b"example-value")),
            "should run store operations inline without a runtime"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_kv_round_trips_on_the_blocking_pool() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let store = kv_store_from_path(Some(&directory.path().join("runtime-kv.redb")));

        store
            .put_bytes("example-key", Bytes::from_static(b"example-value"))
            .await
            .expect("should write the runtime KV store");

        assert_eq!(
            store
                .get_bytes("example-key")
                .await
                .expect("should read the runtime KV store"),
            Some(Bytes::from_static(b"example-value")),
            "should read back the value written inside a runtime"
        );
    }

    #[test]
    fn runtime_kv_persists_across_reopen() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let path = directory.path().join("runtime-kv.redb");
        let first = kv_store_from_path(Some(&path));
        put_then_get(first.as_ref(), "example-key");
        drop(first);

        let reopened = kv_store_from_path(Some(&path));

        assert_eq!(
            futures::executor::block_on(reopened.get_bytes("example-key"))
                .expect("should read the reopened store"),
            Some(Bytes::from_static(b"example-value")),
            "should keep values in the database file across a restart"
        );
    }

    #[test]
    fn runtime_kv_validates_keys_before_database_io() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let store = kv_store_from_path(Some(&directory.path().join("runtime-kv.redb")));

        let result = futures::executor::block_on(store.get_bytes(""));

        assert!(
            matches!(result, Err(KvError::Validation(_))),
            "should reject an empty key as the other adapters do, got {result:?}"
        );
    }

    #[test]
    fn runtime_kv_without_a_path_is_unavailable() {
        let store = kv_store_from_path(None);

        assert!(
            matches!(
                futures::executor::block_on(store.get_bytes("example-key")),
                Err(KvError::Unavailable)
            ),
            "should report KV as unavailable without a path"
        );
    }

    #[test]
    fn runtime_kv_with_an_unopenable_path_is_unavailable() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let file = directory.path().join("regular-file");
        std::fs::write(&file, b"not a directory").expect("should create a regular file");

        let store = kv_store_from_path(Some(&file.join("runtime-kv.redb")));

        assert!(
            matches!(
                futures::executor::block_on(store.get_bytes("example-key")),
                Err(KvError::Unavailable)
            ),
            "should report KV as unavailable when the file cannot be opened"
        );
    }

    #[test]
    fn runtime_kv_reads_its_path_from_the_environment() {
        let directory = tempfile::tempdir().expect("should create a temp directory");
        let path = directory.path().join("runtime-kv.redb");

        let enabled = temp_env::with_var("TRUSTED_SERVER_KV_PATH", Some(&path), kv_store_from_env);
        let disabled = temp_env::with_var_unset("TRUSTED_SERVER_KV_PATH", kv_store_from_env);

        assert_eq!(
            put_then_get(enabled.as_ref(), "example-key"),
            Some(Bytes::from_static(b"example-value")),
            "should open the store TRUSTED_SERVER_KV_PATH names"
        );
        assert!(
            path.is_file(),
            "should create the database file at the configured path"
        );
        assert!(
            matches!(
                futures::executor::block_on(disabled.get_bytes("example-key")),
                Err(KvError::Unavailable)
            ),
            "should leave KV unavailable when TRUSTED_SERVER_KV_PATH is unset"
        );
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
