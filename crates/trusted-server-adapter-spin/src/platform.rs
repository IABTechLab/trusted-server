use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::error::EdgeError;
use edgezero_core::key_value_store::{KvHandle, KvPage, KvStore};
use error_stack::Report;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, KvError, PlatformConfigStore, PlatformError, PlatformGeo, PlatformKvStore,
    PlatformSecretStore, RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
const SPIN_VARIABLE_HEX: &[u8; 16] = b"0123456789abcdef";

// ---------------------------------------------------------------------------
// Noop stubs — used when a handle is absent (native CI, missing binding)
// ---------------------------------------------------------------------------

struct NoopConfigStore;

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for NoopConfigStore {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }

    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }
}

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
struct NoopSecretStore;

#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for NoopSecretStore {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }

    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore).attach("secret store not available"))
    }
}

// ---------------------------------------------------------------------------
// edgezero handle adapters — injected by edgezero_adapter_spin::run_app
// ---------------------------------------------------------------------------

/// Bridges edgezero's [`ConfigStoreHandle`] to [`PlatformConfigStore`].
///
/// Spin config stores are KV-backed, so reads preserve the requested key
/// verbatim. Writes are unsupported on current Spin runtime config and return
/// typed errors.
pub(crate) struct ConfigStoreHandleAdapter(pub(crate) ConfigStoreHandle);

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for ConfigStoreHandleAdapter {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        futures::executor::block_on(self.0.get(key))
            .map_err(|error| {
                Report::new(PlatformError::ConfigStore).attach(format!(
                    "config store lookup failed for key `{key}`: {error}"
                ))
            })?
            .map(|value| String::from(value.as_str()))
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!("key `{key}` not found in Spin config store"))
            })
    }

    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on Spin"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore)
            .attach("config store writes are not supported on Spin"))
    }
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn spin_variable_name(
    key: &str,
    error_context: PlatformError,
) -> Result<String, Report<PlatformError>> {
    if key.is_empty() {
        return Err(Report::new(error_context).attach("Spin variable key must not be empty"));
    }

    // Spin requires each _-separated word to start with an ASCII letter.
    // Reject at the encoder boundary so no caller can accidentally produce aliasing
    // (e.g. "1foo" and "n1foo" both encoding to the same variable name).
    if !key.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(Report::new(error_context).attach(format!(
            "Spin variable key `{key}` must start with a lowercase ASCII letter"
        )));
    }

    // `v_` prefix + worst-case 4 bytes per char for escape sequences.
    let mut out = String::with_capacity(key.len() * 4 + 3);
    out.push_str("v_");

    for ch in key.chars() {
        match ch {
            'a'..='z' | '0'..='9' => out.push(ch),
            'A'..='Z' | '-' | '_' | '.' | ':' => {
                push_spin_variable_escape(&mut out, ch as u8);
            }
            _ => {
                return Err(Report::new(error_context).attach(format!(
                    "Spin variable key `{key}` contains unsupported character `{ch}`"
                )));
            }
        }
    }

    Ok(out)
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn push_spin_variable_escape(out: &mut String, byte: u8) {
    out.push('_');
    out.push('x');
    out.push(SPIN_VARIABLE_HEX[(byte >> 4) as usize] as char);
    out.push(SPIN_VARIABLE_HEX[(byte & 0x0f) as usize] as char);
}

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn spin_secret_variable_name(
    store_name: &StoreName,
    key: &str,
) -> Result<String, Report<PlatformError>> {
    let store_variable = spin_variable_name(store_name.as_ref(), PlatformError::SecretStore)?;
    let key_variable = spin_variable_name(key, PlatformError::SecretStore)?;

    Ok(format!("{store_variable}_{key_variable}"))
}

/// Bridges edgezero's [`KvHandle`] to [`PlatformKvStore`].
///
/// Delegates all operations through `KvHandle`'s raw-bytes API. Spin KV has no
/// native TTL support, so [`put_bytes_with_ttl`](KvStore::put_bytes_with_ttl)
/// returns `KvError::Unsupported` rather than silently writing a non-expiring
/// record. `KvHandle` returns `KvError::Validation` for invalid keys, TTLs, or
/// values before delegating to the backend. Callers of the generic platform KV
/// interface must handle that capability difference explicitly.
struct KvHandleAdapter(KvHandle);

#[async_trait::async_trait(?Send)]
impl KvStore for KvHandleAdapter {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        self.0.get_bytes(key).await
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.0.put_bytes(key, value).await
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        ttl: Duration,
    ) -> Result<(), KvError> {
        self.0.put_bytes_with_ttl(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.delete(key).await
    }

    async fn list_keys_page(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<KvPage, KvError> {
        self.0.list_keys_page(prefix, cursor, limit).await
    }
}

// ---------------------------------------------------------------------------
// SpinSecretStoreAdapter — WASM target + spin feature only
// ---------------------------------------------------------------------------

/// Bridges Spin component variables to [`PlatformSecretStore`].
///
/// # Limitations
///
/// - **UTF-8 only.** `spin_sdk::variables::get` returns a `String`, so all
///   secret values must be valid UTF-8. JSON-encoded signing keys work today;
///   raw binary secrets (e.g. bare Ed25519 bytes) would silently fail at the
///   Spin runtime layer.
///
/// - **Plaintext at rest.** Spin component variables are unencrypted in the
///   application manifest by default. Production deployments must back variables
///   with a real secret-provider source (e.g. Vault, Azure Key Vault) to avoid
///   storing signing keys in plaintext on disk.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
pub(crate) struct SpinSecretStoreAdapter;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for SpinSecretStoreAdapter {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let variable_name = spin_secret_variable_name(store_name, key)?;
        match futures::executor::block_on(spin_sdk::variables::get(&variable_name)) {
            Ok(value) => Ok(value.into_bytes()),
            Err(error) => Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret lookup failed for key `{key}` as Spin variable `{variable_name}`: {error}"
            ))),
        }
    }

    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Spin"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store deletes are not supported on Spin"))
    }
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Spin request.
///
/// Config and KV are sourced from the `EdgeZero` handles that `run_app` injects
/// before routing. Secrets are read synchronously from Spin component
/// variables because Trusted Server's platform secret trait is sync.
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
    let client_ip = extract_client_ip(ctx);
    let outbound_client = ctx
        .http_client()
        .ok_or_else(|| {
            EdgeError::internal(std::io::Error::other("request has no outbound client"))
        })?
        .with_proxy_marker(false);

    let config_store: Arc<dyn PlatformConfigStore> = ctx
        .config_store_default()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigStore>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));

    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_store_default()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(SpinSecretStoreAdapter);
    #[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(NoopSecretStore);

    let builder = RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .outbound_client(outbound_client)
        .clock(ctx.monotonic_clock())
        .geo(Arc::new(NullGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
            // Spin's HTTP trigger does not expose TLS fingerprint or edge-server
            // metadata to the guest, so these stay unset.
            tls_ja4: None,
            h2_fingerprint: None,
            server_hostname: None,
            server_region: None,
        });
    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();
    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let builder = builder.timer(Arc::clone(TIMER.get_or_init(|| Arc::new(SpinTimer))));
    Ok(builder.build())
}

// ---------------------------------------------------------------------------
// Geo and client info
// ---------------------------------------------------------------------------

struct NullGeo;

impl PlatformGeo for NullGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

/// Reads the client IP from [`SpinRequestContext`].
///
/// The value is the immediate TCP peer reported by Spin's `spin-client-addr`,
/// re-derived from the *trusted last* header occurrence by
/// [`crate::app::normalize_spin_request`] (which overwrites the context before
/// this runs) so a client cannot forge it with a prepended duplicate.
///
/// Trust assumption: this is the real client IP only when Spin terminates the
/// client connection directly. Behind an untrusted reverse proxy or load
/// balancer it is the proxy's address, and the real client IP would live in a
/// spoofable forwarded header — unlike the CDN-trusted context the Fastly and
/// Cloudflare adapters receive. Deployments fronting Spin with such a hop must
/// account for this.
fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    ctx.extensions()
        .get::<edgezero_adapter_spin::context::SpinRequestContext>()
        .and_then(|c| c.client_addr)
}

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
struct SpinTimer;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
impl trusted_server_core::platform::PlatformTimer for SpinTimer {
    fn delay(&self, duration: std::time::Duration) -> futures::future::LocalBoxFuture<'static, ()> {
        Box::pin(async move {
            spin_sdk::time::sleep(duration).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::body::Body;
    use edgezero_core::config_store::{
        BoundedStoreRead, ConfigStore, ConfigStoreError, ConfigValue, finish_bounded_config_read,
    };
    use edgezero_core::context::RequestContext;
    use edgezero_core::http::request_builder;
    use edgezero_core::params::PathParams;
    use edgezero_core::time::{Deadline, MonotonicClock};
    use trusted_server_core::platform::HttpClient;
    use trusted_server_core::test_support::scripted_client::ScriptedHttpClient;

    struct InMemoryConfigStore(std::collections::BTreeMap<String, String>);

    #[async_trait::async_trait(?Send)]
    impl ConfigStore for InMemoryConfigStore {
        async fn get(&self, key: &str) -> Result<Option<ConfigValue>, ConfigStoreError> {
            Ok(self
                .0
                .get(key)
                .map(|value| ConfigValue::from(value.as_str())))
        }

        async fn get_bounded(
            &self,
            key: &str,
            clock: &MonotonicClock,
            deadline: Deadline,
            max_backend_bytes: u64,
            max_value_bytes: u64,
        ) -> Result<BoundedStoreRead<ConfigValue>, ConfigStoreError> {
            finish_bounded_config_read(
                self.get(key).await,
                clock,
                deadline,
                max_backend_bytes,
                max_value_bytes,
            )
        }
    }

    fn make_ctx_without_spin_context() -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build request");
        RequestContext::new(req, PathParams::default())
    }

    #[test]
    fn extract_client_ip_reads_spin_request_context() {
        let mut req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build request");
        edgezero_adapter_spin::context::SpinRequestContext::insert(
            &mut req,
            edgezero_adapter_spin::context::SpinRequestContext {
                client_addr: Some("203.0.113.42".parse().expect("should parse test IP")),
                full_url: None,
            },
        );
        let ctx = RequestContext::new(req, PathParams::default());

        assert_eq!(
            extract_client_ip(&ctx),
            Some("203.0.113.42".parse().expect("should parse test IP")),
            "should read Spin client IP from request context"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_without_spin_context() {
        let ctx = make_ctx_without_spin_context();

        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None when Spin context is absent"
        );
    }

    #[test]
    fn null_geo_always_returns_none() {
        let geo = NullGeo;

        assert!(
            geo.lookup(None).expect("should not fail").is_none(),
            "should return None without a client IP"
        );
        assert!(
            geo.lookup(Some("127.0.0.1".parse().expect("should parse IP")))
                .expect("should not fail")
                .is_none(),
            "should return None for any client IP"
        );
    }

    #[test]
    fn config_store_handle_adapter_reads_verbatim_kv_key() {
        let handle = ConfigStoreHandle::new(Arc::new(InMemoryConfigStore(
            std::collections::BTreeMap::from([(
                "trusted_server_config".to_owned(),
                "blob-envelope".to_owned(),
            )]),
        )));
        let adapter = ConfigStoreHandleAdapter(handle);

        let value = adapter
            .get(
                &StoreName::from("trusted_server_config"),
                "trusted_server_config",
            )
            .expect("should read the verbatim config-store key");

        assert_eq!(
            value, "blob-envelope",
            "should not translate a KV-backed config key into a Spin variable name"
        );
    }

    #[test]
    fn spin_variable_name_encodes_secret_key_components() {
        assert_eq!(
            spin_variable_name("ts-2026-05-25", PlatformError::SecretStore)
                .expect("should encode generated kid"),
            "v_ts_x2d2026_x2d05_x2d25"
        );
        // Digit-leading keys are rejected at the encoder boundary.
        assert!(
            spin_variable_name("2026-key", PlatformError::SecretStore).is_err(),
            "should reject digit-leading key"
        );
    }

    #[test]
    fn spin_encoder_accepts_every_creatable_kid() {
        // Portability contract: core's create/rotate validation (kid_is_creatable)
        // must never admit a kid the Spin secret-variable encoder rejects — otherwise
        // that kid could be created but its private signing key could not be stored.
        // This pins core >= encoder strictness so the duplicated lowercase-leading
        // rule in validate_kid and spin_variable_name cannot silently drift.
        use trusted_server_core::request_signing::kid_is_creatable;

        let samples = [
            "a",
            "kid",
            "ts-2026-05-25",
            "azAZ09-_.:",
            "k.id:with_all-chars",
            "KidA",
            "_kid",
            "-kid",
            ".kid",
            ":kid",
            "1foo",
            "0abc",
            "",
            "a,b",
            "a b",
            "kid/name",
        ];
        for kid in samples {
            if kid_is_creatable(kid) {
                assert!(
                    spin_variable_name(kid, PlatformError::SecretStore).is_ok(),
                    "core accepts kid `{kid}` but the Spin encoder rejects it \
                     (portability contract broken)"
                );
            }
        }
    }

    #[test]
    fn spin_secret_variable_name_prefixes_store_name() {
        assert_eq!(
            spin_secret_variable_name(&StoreName::from("signing_keys"), "ts-2026-05-25")
                .expect("should encode secret variable name"),
            "v_signing_x5fkeys_v_ts_x2d2026_x2d05_x2d25"
        );
    }

    #[test]
    fn spin_variable_name_rejects_unsupported_characters() {
        assert!(
            spin_variable_name("kid/name", PlatformError::SecretStore).is_err(),
            "should reject characters outside the supported Spin variable mapping"
        );
    }

    #[test]
    fn spin_variable_name_does_not_collapse_distinct_allowed_kids() {
        let mut names = std::collections::BTreeSet::new();
        // Only lowercase-leading keys are accepted; "KidA" is rejected at the boundary.
        for kid in ["kid-a", "kid.a", "kid:a", "kid_a", "kid_2da", "kida"] {
            let variable_name = spin_variable_name(kid, PlatformError::ConfigStore)
                .expect("should encode allowed kid characters");
            assert!(
                names.insert(variable_name.clone()),
                "Spin variable mapping must not collide for kid `{kid}` as `{variable_name}`"
            );
        }
        assert!(
            spin_variable_name("KidA", PlatformError::ConfigStore).is_err(),
            "should reject uppercase-leading kid at the encoder boundary"
        );
    }

    /// Builds a context without a Spin request context whose request carries
    /// `client`, as `EdgeZero`'s Spin request conversion installs its default
    /// client.
    fn make_ctx_with_outbound_client(client: HttpClient) -> RequestContext {
        let mut req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(Body::empty())
            .expect("should build request");
        req.extensions_mut().insert(client);
        RequestContext::new(req, PathParams::default())
    }

    #[test]
    fn build_runtime_services_carries_the_context_client_with_the_proxy_marker_off() {
        let client = HttpClient::with_client(ScriptedHttpClient::forbidding());
        assert!(
            client.proxy_marker_enabled(),
            "should start from a client whose marker is on"
        );
        let ctx = make_ctx_with_outbound_client(client);

        let services = build_runtime_services(&ctx).expect("should build runtime services");

        assert!(
            !services.outbound_client().proxy_marker_enabled(),
            "should turn the proxy marker off on the context client"
        );
    }

    #[test]
    fn build_runtime_services_rejects_a_request_without_an_outbound_client() {
        let ctx = make_ctx_without_spin_context();

        let error = build_runtime_services(&ctx)
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn build_runtime_services_uses_noop_native_stores_without_handles() {
        let ctx = make_ctx_with_outbound_client(ScriptedHttpClient::forbidding().http_client());
        let services = build_runtime_services(&ctx).expect("should build runtime services");

        assert!(
            services.client_info().client_ip.is_none(),
            "should not set client IP without Spin request context"
        );
        assert!(
            services
                .geo()
                .lookup(None)
                .expect("should not fail")
                .is_none(),
            "should use null geo"
        );
        assert!(
            services
                .config_store()
                .get(&StoreName::from("config"), "missing")
                .is_err(),
            "should return typed config error without injected handle"
        );
        assert!(
            services.kv_store().get_bytes("missing").await.is_err(),
            "should return typed KV error without injected handle"
        );
    }

    #[test]
    fn spin_variable_name_rejects_digit_leading_key() {
        // Encoder enforces the lowercase-letter start contract at the boundary so
        // no caller can produce aliasing (e.g. "1foo" and "n1foo" would collide).
        let result = spin_variable_name("1foo", PlatformError::ConfigStore);
        assert!(
            result.is_err(),
            "should reject digit-leading key at the encoder boundary"
        );
    }

    #[test]
    fn spin_variable_name_rejects_uppercase_leading_key() {
        let result = spin_variable_name("Foo", PlatformError::ConfigStore);
        assert!(
            result.is_err(),
            "should reject uppercase-leading key at the encoder boundary"
        );
    }
}
