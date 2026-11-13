#[cfg(any(target_arch = "wasm32", test))]
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use edgezero_core::config_store::ConfigStoreHandle;
use edgezero_core::error::EdgeError;
use edgezero_core::key_value_store::{KvHandle, KvPage, KvStore};
use error_stack::Report;
use trusted_server_core::outbound::request_outbound_client;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, KvError, PlatformConfigStore, PlatformError, PlatformGeo, PlatformKvStore,
    PlatformSecretStore, RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};

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
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store not available"))
    }
}

struct NoopSecretStore;

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
// edgezero handle adapters — no #[cfg] needed; platform-specific store
// construction is handled by edgezero's run_app before we receive the ctx.
// ---------------------------------------------------------------------------

/// Bridges edgezero's [`ConfigStoreHandle`] (injected by `run_app` from the
/// `TRUSTED_SERVER_CONFIG` env-var binding) to [`PlatformConfigStore`].
///
/// Reads delegate through the handle. Writes are unsupported on all current
/// adapter targets and return errors.
///
/// Note: Cloudflare config is a single flat JSON env-var binding — all keys
/// live in one namespace. The `store_name` argument is intentionally ignored;
/// callers cannot route to a different store by passing a different name.
struct ConfigStoreHandleAdapter(ConfigStoreHandle);

#[async_trait::async_trait(?Send)]
impl PlatformConfigStore for ConfigStoreHandleAdapter {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        futures::executor::block_on(self.0.get(key))
            .map_err(|e| {
                Report::new(PlatformError::ConfigStore)
                    .attach(format!("config store lookup failed: {e}"))
            })?
            .map(|value| String::from(value.as_str()))
            .ok_or_else(|| {
                Report::new(PlatformError::ConfigStore).attach(format!("key not found: {key}"))
            })
    }

    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore).attach("config store writes are not supported"))
    }
}

/// Bridges edgezero's [`KvHandle`] (injected by `run_app` from the
/// `TRUSTED_SERVER_KV` KV namespace binding) to [`PlatformKvStore`].
///
/// Delegates all operations through `KvHandle`'s raw-bytes API, which includes
/// key/value validation before forwarding to the underlying store.
///
/// Note: key/value validation runs twice — once inside this `KvHandle` and once
/// inside the `KvHandle` that `RuntimeServices::kv_handle()` constructs from
/// this adapter. The overhead is negligible (string length checks only) and
/// avoided by the fact that we reuse the already-opened `env.kv()` handle from
/// `run_app` rather than opening a new one.
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
// CloudflareSecretStoreAdapter — WASM target only
//
// Secrets are the one platform surface that cannot be bridged through an
// edgezero handle: `SecretHandle::get_bytes` is async, but
// `PlatformSecretStore::get_bytes` is sync. The Cloudflare `env.secret()`
// call IS synchronous at the JS level, so we call it directly here.
// ---------------------------------------------------------------------------

/// Bridges [`worker::Env`] secrets to [`PlatformSecretStore`] by calling
/// `env.secret(key)` synchronously. Writes and deletes return errors.
#[cfg(target_arch = "wasm32")]
pub(crate) struct CloudflareSecretStoreAdapter {
    pub(crate) env: worker::Env,
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for CloudflareSecretStoreAdapter {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        match self.env.secret(key) {
            // worker 0.7: Secret implements Display via JsValue::as_string() which
            // returns the raw JS string value with no wrapping or debug formatting.
            // Verified in worker-rs src/env.rs: `impl Display for Secret { fn fmt ->
            // write!(f, "{}", self.inner.as_string().unwrap_or_default()) }`.
            Ok(secret) => Ok(secret.to_string().into_bytes()),
            Err(err) => Err(Report::new(PlatformError::SecretStore)
                .attach(format!("secret lookup failed for key `{key}`: {err}"))),
        }
    }

    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Cloudflare Workers"))
    }

    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Cloudflare Workers"))
    }
}

// ---------------------------------------------------------------------------
// build_runtime_services
// ---------------------------------------------------------------------------

/// Construct [`RuntimeServices`] for an incoming Cloudflare Workers request.
///
/// Config and KV are sourced from the edgezero handles that `run_app` injects
/// before routing — via the `TRUSTED_SERVER_CONFIG` env-var binding and the
/// `TRUSTED_SERVER_KV` KV namespace declared in `cloudflare.toml`. No
/// platform-specific `#[cfg]` is required for these two stores.
///
/// Secrets still require direct `worker::Env` access because
/// `SecretHandle::get_bytes` is async while `PlatformSecretStore::get_bytes`
/// is sync; the underlying `env.secret()` call is synchronous at the JS level.
///
/// Geo information is read from Cloudflare's injected request headers
/// (`cf-ipcountry`, etc.) which are present on all plans; headers absent on
/// the native host target simply produce empty/zero defaults.
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
    let outbound_client = request_outbound_client(ctx)?;

    // Config: use the ConfigStoreHandle injected by run_app — no #[cfg] needed.
    let config_store: Arc<dyn PlatformConfigStore> = ctx
        .config_store_default()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigStore>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));

    // KV: use the KvHandle injected by run_app — no #[cfg] needed.
    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_store_default()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));

    // Secrets: still requires wasm32-specific env.secret() (async/sync mismatch).
    #[cfg(target_arch = "wasm32")]
    let secret_store: Arc<dyn PlatformSecretStore> = ctx
        .extensions()
        .get::<edgezero_adapter_cloudflare::context::CloudflareRequestContext>()
        .map(|cf_ctx| {
            Arc::new(CloudflareSecretStoreAdapter {
                env: cf_ctx.env().clone(),
            }) as Arc<dyn PlatformSecretStore>
        })
        .unwrap_or_else(|| Arc::new(NoopSecretStore));
    #[cfg(not(target_arch = "wasm32"))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(NoopSecretStore);

    // Geo: read Cloudflare-injected headers — no #[cfg] needed; headers are
    // simply absent on the native host target, producing Ok(None) from lookup().
    let geo = build_geo(ctx);

    let builder = RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .outbound_client(outbound_client)
        .clock(ctx.monotonic_clock())
        .geo(Arc::new(geo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
            ..ClientInfo::default()
        });
    #[cfg(target_arch = "wasm32")]
    static TIMER: std::sync::OnceLock<Arc<dyn trusted_server_core::platform::PlatformTimer>> =
        std::sync::OnceLock::new();
    #[cfg(target_arch = "wasm32")]
    let builder = builder.timer(Arc::clone(TIMER.get_or_init(|| Arc::new(CloudflareTimer))));
    Ok(builder.build())
}

// ---------------------------------------------------------------------------
// Geo — reads Cloudflare-injected request headers (no #[cfg] needed)
// ---------------------------------------------------------------------------

/// Reads Cloudflare geo headers injected by the Workers runtime.
///
/// `cf-ipcountry` is available on all plans. `cf-ipcity`, `cf-ipcontinent`,
/// `cf-iplatitude`, and `cf-iplongitude` require an Enterprise plan. Absent or
/// unparseable values are absent. Coordinates are available only when both
/// values parse. Country code `XX` is treated as absent.
struct CloudflareGeo {
    country: String,
    city: Option<String>,
    continent: Option<String>,
    coordinates: Option<trusted_server_core::platform::GeoCoordinates>,
}

impl PlatformGeo for CloudflareGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        if self.country.is_empty() {
            return Ok(None);
        }
        Ok(Some(GeoInfo {
            city: self.city.clone(),
            country: self.country.clone(),
            continent: self.continent.clone(),
            coordinates: self.coordinates,
            metro_code: None,
            region: None,
            asn: None,
        }))
    }
}

fn build_geo(ctx: &edgezero_core::context::RequestContext) -> CloudflareGeo {
    let headers = ctx.headers();
    let country = headers
        .get("cf-ipcountry")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && *s != "XX")
        .unwrap_or("")
        .to_string();
    let city = headers
        .get("cf-ipcity")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let continent = headers
        .get("cf-ipcontinent")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let latitude = headers
        .get("cf-iplatitude")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok());
    let longitude = headers
        .get("cf-iplongitude")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok());
    let coordinates = latitude.zip(longitude).map(|(latitude, longitude)| {
        trusted_server_core::platform::GeoCoordinates {
            latitude,
            longitude,
        }
    });
    CloudflareGeo {
        country,
        city,
        continent,
        coordinates,
    }
}

fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    ctx.headers()
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

#[cfg(target_arch = "wasm32")]
struct CloudflareTimer;

#[cfg(target_arch = "wasm32")]
impl trusted_server_core::platform::PlatformTimer for CloudflareTimer {
    fn delay(&self, duration: std::time::Duration) -> futures::future::LocalBoxFuture<'static, ()> {
        relay_timer_waker(worker::Delay::from(duration))
    }
}

// `worker::Delay` retains its first waker. Keep that waker stable while relaying
// wakeups to the latest task polling the shared HTML completion deadline.
#[cfg(any(target_arch = "wasm32", test))]
fn relay_timer_waker(
    delay: impl Future<Output = ()> + 'static,
) -> futures::future::LocalBoxFuture<'static, ()> {
    let relay = Arc::new(TimerWakerRelay(futures::task::AtomicWaker::new()));
    let relay_waker = futures::task::waker(Arc::clone(&relay));
    let mut delay = Box::pin(delay);
    Box::pin(futures::future::poll_fn(move |context| {
        relay.0.register(context.waker());
        delay
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&relay_waker))
    }))
}

#[cfg(any(target_arch = "wasm32", test))]
struct TimerWakerRelay(futures::task::AtomicWaker);

#[cfg(any(target_arch = "wasm32", test))]
impl futures::task::ArcWake for TimerWakerRelay {
    fn wake_by_ref(relay: &Arc<Self>) {
        relay.0.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::context::RequestContext;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl futures::task::ArcWake for WakeCounter {
        fn wake_by_ref(counter: &Arc<Self>) {
            counter.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn timer_relay_wakes_the_latest_task() {
        let first_waker = Arc::new(Mutex::new(None::<Waker>));
        let ready = Arc::new(AtomicBool::new(false));
        let captured_waker = Arc::clone(&first_waker);
        let delay_ready = Arc::clone(&ready);
        // Model worker::Delay by saving only the first poll's waker.
        let delay = futures::future::poll_fn(move |context| {
            captured_waker
                .lock()
                .expect("should lock the captured waker")
                .get_or_insert_with(|| context.waker().clone());
            if delay_ready.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut timer = relay_timer_waker(delay);
        let first = Arc::new(WakeCounter::default());
        let latest = Arc::new(WakeCounter::default());
        let first_task_waker = futures::task::waker(Arc::clone(&first));
        let latest_task_waker = futures::task::waker(Arc::clone(&latest));

        assert!(
            timer
                .as_mut()
                .poll(&mut Context::from_waker(&first_task_waker))
                .is_pending(),
            "should register the first polling task"
        );
        assert!(
            timer
                .as_mut()
                .poll(&mut Context::from_waker(&latest_task_waker))
                .is_pending(),
            "should register the latest polling task"
        );
        ready.store(true, Ordering::SeqCst);
        first_waker
            .lock()
            .expect("should lock the captured waker")
            .as_ref()
            .expect("should capture the delay's first waker")
            .wake_by_ref();

        assert_eq!(
            first.0.load(Ordering::SeqCst),
            0,
            "should leave the old task asleep"
        );
        assert_eq!(
            latest.0.load(Ordering::SeqCst),
            1,
            "should wake the latest polling task"
        );
        assert!(
            timer
                .as_mut()
                .poll(&mut Context::from_waker(&latest_task_waker))
                .is_ready(),
            "should complete the timer in the latest task"
        );
    }

    use edgezero_core::http::{HeaderValue, request_builder};
    use edgezero_core::params::PathParams;
    use trusted_server_core::platform::HttpClient;
    use trusted_server_core::test_support::scripted_client::ScriptedHttpClient;

    fn make_ctx_with_header(name: &str, value: &str) -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .header(
                name,
                HeaderValue::from_str(value).expect("should parse test header value"),
            )
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::default())
    }

    fn make_ctx_without_header() -> RequestContext {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        RequestContext::new(req, PathParams::default())
    }

    /// Builds a context whose request carries `client`, as `EdgeZero`'s
    /// Cloudflare request conversion installs its default client.
    fn make_ctx_with_outbound_client(client: HttpClient) -> RequestContext {
        let mut req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
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
        let ctx = make_ctx_without_header();

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

    #[test]
    fn extract_client_ip_parses_cf_connecting_ip() {
        let ctx = make_ctx_with_header("cf-connecting-ip", "203.0.113.42");
        let ip = extract_client_ip(&ctx);
        assert_eq!(
            ip,
            Some("203.0.113.42".parse().expect("should parse test IP")),
            "should parse cf-connecting-ip header"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_when_header_absent() {
        let ctx = make_ctx_without_header();
        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None when cf-connecting-ip is not set"
        );
    }

    #[test]
    fn extract_client_ip_returns_none_for_invalid_ip() {
        let ctx = make_ctx_with_header("cf-connecting-ip", "not-an-ip");
        assert!(
            extract_client_ip(&ctx).is_none(),
            "should return None for an unparseable IP string"
        );
    }

    #[test]
    fn build_geo_returns_country_from_header() {
        let ctx = make_ctx_with_header("cf-ipcountry", "US");
        let geo = build_geo(&ctx);
        assert_eq!(geo.country, "US", "should extract cf-ipcountry");
    }

    #[test]
    fn build_geo_treats_xx_as_absent() {
        let ctx = make_ctx_with_header("cf-ipcountry", "XX");
        let geo = build_geo(&ctx);
        assert!(geo.country.is_empty(), "XX should be treated as absent");
    }

    #[test]
    fn build_geo_lookup_returns_none_when_country_absent() {
        let ctx = make_ctx_without_header();
        let geo = build_geo(&ctx);
        assert!(
            geo.lookup(None)
                .expect("should perform geo lookup")
                .is_none(),
            "should return None when no country header"
        );
    }

    #[test]
    fn build_geo_lookup_returns_some_with_populated_country() {
        let req = request_builder()
            .method("GET")
            .uri("https://example.com/")
            .header("cf-ipcountry", HeaderValue::from_static("US"))
            .header("cf-ipcity", HeaderValue::from_static("New York"))
            .header("cf-ipcontinent", HeaderValue::from_static("NA"))
            .header("cf-iplatitude", HeaderValue::from_static("40.71"))
            .header("cf-iplongitude", HeaderValue::from_static("-74.01"))
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");
        let ctx = RequestContext::new(req, PathParams::default());
        let geo = build_geo(&ctx);
        let info = geo
            .lookup(None)
            .expect("should perform geo lookup")
            .expect("should return GeoInfo when country is set");
        assert_eq!(info.country, "US", "should populate country");
        assert_eq!(
            info.city.as_deref(),
            Some("New York"),
            "should populate city"
        );
        assert_eq!(
            info.continent.as_deref(),
            Some("NA"),
            "should populate continent"
        );
        let coordinates = info.coordinates.expect("should populate coordinate pair");
        assert_eq!(coordinates.latitude, 40.71);
        assert_eq!(coordinates.longitude, -74.01);
    }

    #[test]
    fn build_geo_country_only_does_not_invent_detail() {
        let ctx = make_ctx_with_header("cf-ipcountry", "US");
        let info = build_geo(&ctx)
            .lookup(None)
            .expect("should look up geo")
            .expect("should have country");
        assert_eq!(info, GeoInfo::country_region("US", None));
    }

    #[test]
    fn build_geo_requires_both_parseable_coordinates_and_preserves_zero() {
        for (latitude, longitude, has_coordinates) in [
            (None, None, false),
            (Some("0"), None, false),
            (None, Some("0"), false),
            (Some("bad"), Some("0"), false),
            (Some("0"), Some("bad"), false),
            (Some("0"), Some("0"), true),
        ] {
            let mut builder = request_builder()
                .uri("https://example.com/")
                .header("cf-ipcountry", "US");
            if let Some(value) = latitude {
                builder = builder.header("cf-iplatitude", value);
            }
            if let Some(value) = longitude {
                builder = builder.header("cf-iplongitude", value);
            }
            let request = builder
                .body(edgezero_core::body::Body::empty())
                .expect("should build request");
            let ctx = RequestContext::new(request, PathParams::default());
            let info = build_geo(&ctx)
                .lookup(None)
                .expect("should look up geo")
                .expect("should have country");
            assert_eq!(info.coordinates.is_some(), has_coordinates);
            if has_coordinates {
                assert_eq!(info.coordinates_string().as_deref(), Some("0,0"));
            }
        }
    }
}
