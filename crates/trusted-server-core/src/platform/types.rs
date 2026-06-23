use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use super::{
    PlatformBackend, PlatformConfigStore, PlatformGeo, PlatformHttpClient, PlatformKvStore,
    PlatformSecretStore,
};
use crate::auction::telemetry::{AuctionEventSink, NoopSink};

/// Geographic information extracted from a request.
///
/// Serde derives are required because `GeoInfo` is embedded in
/// `AuctionRequest`, which is serialised for bid-request payloads.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeoInfo {
    /// City name.
    pub city: String,
    /// Two-letter country code.
    pub country: String,
    /// Continent name.
    pub continent: String,
    /// Latitude coordinate.
    pub latitude: f64,
    /// Longitude coordinate.
    pub longitude: f64,
    /// DMA (Designated Market Area) / metro code.
    pub metro_code: i64,
    /// Region code.
    pub region: Option<String>,
    /// Autonomous System Number (e.g. `7922` = Comcast).
    /// Used to distinguish home ISP vs. corporate VPN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
}

impl GeoInfo {
    /// Returns coordinates as a formatted string `"latitude,longitude"`.
    #[must_use]
    pub fn coordinates_string(&self) -> String {
        format!("{},{}", self.latitude, self.longitude)
    }

    /// Checks if a valid metro code is available.
    #[must_use]
    pub fn has_metro_code(&self) -> bool {
        self.metro_code > 0
    }
}

/// Per-request client metadata extracted once at the adapter entry point.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// Client IP address, if available.
    pub client_ip: Option<IpAddr>,
    /// TLS protocol version string, if the connection used TLS.
    pub tls_protocol: Option<String>,
    /// OpenSSL cipher name, if the connection used TLS.
    pub tls_cipher: Option<String>,
    /// TLS JA4 fingerprint, if the platform exposes it.
    pub tls_ja4: Option<String>,
    /// HTTP/2 client fingerprint, if the platform exposes it.
    pub h2_fingerprint: Option<String>,
    /// Edge server hostname, if available.
    pub server_hostname: Option<String>,
    /// Edge server region, if available.
    pub server_region: Option<String>,
}

/// Edge-visible name used to open a config or secret store at runtime.
///
/// Passed to read methods on [`super::PlatformConfigStore`] and
/// [`super::PlatformSecretStore`]. Distinct from [`StoreId`] to prevent
/// accidentally passing a management API identifier where a runtime name is
/// expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub struct StoreName(String);

impl From<String> for StoreName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for StoreName {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for StoreName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Management API identifier used to write to a config or secret store.
///
/// Passed to write methods on [`super::PlatformConfigStore`] and
/// [`super::PlatformSecretStore`]. Distinct from [`StoreName`] to prevent
/// accidentally passing a runtime store name where a management API
/// identifier is expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub struct StoreId(String);

impl From<String> for StoreId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for StoreId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for StoreId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Input specification for a dynamic backend.
///
/// Passed to [`PlatformBackend::predict_name`] and [`PlatformBackend::ensure`]
/// to deterministically name and register upstream origins.
#[derive(Debug, Clone)]
pub struct PlatformBackendSpec {
    /// URL scheme.
    pub scheme: String,
    /// Hostname of the backend origin.
    pub host: String,
    /// Explicit port, or `None` to use the scheme default.
    pub port: Option<u16>,
    /// Whether to verify the TLS certificate.
    pub certificate_check: bool,
    /// Maximum time to wait for the first response byte.
    pub first_byte_timeout: Duration,
}

/// Cloneable container of platform services for a single request.
#[derive(Clone)]
pub struct RuntimeServices {
    /// Access to key-value config stores.
    pub(crate) config_store: Arc<dyn PlatformConfigStore>,
    /// Access to encrypted secret stores.
    pub(crate) secret_store: Arc<dyn PlatformSecretStore>,
    /// KV store service selected for the current request path.
    ///
    /// Adapters may replace this with a different concrete store on a
    /// per-request basis by cloning [`RuntimeServices`] with
    /// [`RuntimeServices::with_kv_store`].
    pub(crate) kv_store: Arc<dyn PlatformKvStore>,
    /// Dynamic backend registration and name prediction.
    pub(crate) backend: Arc<dyn PlatformBackend>,
    /// Outbound HTTP client abstraction.
    pub(crate) http_client: Arc<dyn PlatformHttpClient>,
    /// Geographic information lookup.
    pub(crate) geo: Arc<dyn PlatformGeo>,
    /// Per-request client metadata extracted at the entry point.
    pub(crate) client_info: ClientInfo,
    /// Sink for auction telemetry rows. Defaults to a no-op; the Fastly adapter
    /// installs a real implementation.
    pub(crate) auction_event_sink: Arc<dyn AuctionEventSink>,
}

impl RuntimeServices {
    /// Create a builder for [`RuntimeServices`].
    ///
    /// Adapter crates should use this builder rather than constructing
    /// [`RuntimeServices`] directly, so that any future invariants on the
    /// struct are enforced in one place.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let services = RuntimeServices::builder()
    ///     .config_store(Arc::new(MyConfigStore))
    ///     .secret_store(Arc::new(MySecretStore))
    ///     .kv_store(kv_store)
    ///     .backend(Arc::new(MyBackend))
    ///     .http_client(Arc::new(MyHttpClient))
    ///     .geo(Arc::new(MyGeo))
    ///     .client_info(client_info)
    ///     .build();
    /// ```
    #[must_use]
    pub fn builder() -> RuntimeServicesBuilder {
        RuntimeServicesBuilder::new()
    }

    /// Returns the config store service.
    #[must_use]
    pub fn config_store(&self) -> &dyn PlatformConfigStore {
        &*self.config_store
    }

    /// Returns the secret store service.
    #[must_use]
    pub fn secret_store(&self) -> &dyn PlatformSecretStore {
        &*self.secret_store
    }

    /// Returns the KV store service.
    #[must_use]
    pub fn kv_store(&self) -> &dyn PlatformKvStore {
        &*self.kv_store
    }

    /// Returns the dynamic backend service.
    #[must_use]
    pub fn backend(&self) -> &dyn PlatformBackend {
        &*self.backend
    }

    /// Returns the outbound HTTP client service.
    #[must_use]
    pub fn http_client(&self) -> &dyn PlatformHttpClient {
        &*self.http_client
    }

    /// Returns the platform geo lookup service.
    #[must_use]
    pub fn geo(&self) -> &dyn PlatformGeo {
        &*self.geo
    }

    /// Returns per-request client metadata (IP address, TLS details).
    #[must_use]
    pub fn client_info(&self) -> &ClientInfo {
        &self.client_info
    }

    /// Returns the auction telemetry sink.
    #[must_use]
    pub fn auction_event_sink(&self) -> &dyn AuctionEventSink {
        &*self.auction_event_sink
    }

    /// Wrap the KV store in a [`super::KvHandle`] for ergonomic access to
    /// JSON helpers, pagination, and validation.
    #[must_use]
    pub fn kv_handle(&self) -> super::KvHandle {
        super::KvHandle::new(self.kv_store.clone())
    }

    /// Returns a clone of this instance with the KV store replaced by `store`.
    ///
    /// Adapters use this to lazily inject the request-specific KV store for
    /// handlers that require one without rebuilding the rest of the runtime
    /// services graph.
    #[must_use]
    pub fn with_kv_store(self, store: Arc<dyn PlatformKvStore>) -> Self {
        Self {
            kv_store: store,
            ..self
        }
    }

    /// Return a clone of these services with a different auction event sink.
    #[must_use]
    pub fn with_auction_event_sink(self, sink: Arc<dyn AuctionEventSink>) -> Self {
        Self {
            auction_event_sink: sink,
            ..self
        }
    }
}

impl fmt::Debug for RuntimeServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeServices")
            .field("client_info", &self.client_info)
            .finish_non_exhaustive()
    }
}

/// Builder for [`RuntimeServices`].
///
/// Obtain a builder via [`RuntimeServices::builder`] and set each service
/// before calling [`RuntimeServicesBuilder::build`].
pub struct RuntimeServicesBuilder {
    config_store: Option<Arc<dyn PlatformConfigStore>>,
    secret_store: Option<Arc<dyn PlatformSecretStore>>,
    kv_store: Option<Arc<dyn PlatformKvStore>>,
    backend: Option<Arc<dyn PlatformBackend>>,
    http_client: Option<Arc<dyn PlatformHttpClient>>,
    geo: Option<Arc<dyn PlatformGeo>>,
    client_info: Option<ClientInfo>,
    auction_event_sink: Option<Arc<dyn AuctionEventSink>>,
}

impl RuntimeServicesBuilder {
    fn new() -> Self {
        Self {
            config_store: None,
            secret_store: None,
            kv_store: None,
            backend: None,
            http_client: None,
            geo: None,
            client_info: None,
            auction_event_sink: None,
        }
    }

    /// Set the config store implementation.
    #[must_use]
    pub fn config_store(mut self, config_store: Arc<dyn PlatformConfigStore>) -> Self {
        self.config_store = Some(config_store);
        self
    }

    /// Set the secret store implementation.
    #[must_use]
    pub fn secret_store(mut self, secret_store: Arc<dyn PlatformSecretStore>) -> Self {
        self.secret_store = Some(secret_store);
        self
    }

    /// Set the KV store implementation.
    #[must_use]
    pub fn kv_store(mut self, kv_store: Arc<dyn PlatformKvStore>) -> Self {
        self.kv_store = Some(kv_store);
        self
    }

    /// Set the backend implementation.
    #[must_use]
    pub fn backend(mut self, backend: Arc<dyn PlatformBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Set the HTTP client implementation.
    #[must_use]
    pub fn http_client(mut self, http_client: Arc<dyn PlatformHttpClient>) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// Set the geo lookup implementation.
    #[must_use]
    pub fn geo(mut self, geo: Arc<dyn PlatformGeo>) -> Self {
        self.geo = Some(geo);
        self
    }

    /// Set the per-request client metadata.
    #[must_use]
    pub fn client_info(mut self, client_info: ClientInfo) -> Self {
        self.client_info = Some(client_info);
        self
    }

    /// Set the auction telemetry sink. Defaults to a no-op when unset.
    #[must_use]
    pub fn auction_event_sink(mut self, sink: Arc<dyn AuctionEventSink>) -> Self {
        self.auction_event_sink = Some(sink);
        self
    }

    /// Construct [`RuntimeServices`] from the accumulated configuration.
    ///
    /// # Panics
    ///
    /// Panics if any required service has not been set via the builder methods.
    #[must_use]
    pub fn build(self) -> RuntimeServices {
        RuntimeServices {
            config_store: self
                .config_store
                .expect("should set config_store before building RuntimeServices"),
            secret_store: self
                .secret_store
                .expect("should set secret_store before building RuntimeServices"),
            kv_store: self
                .kv_store
                .expect("should set kv_store before building RuntimeServices"),
            backend: self
                .backend
                .expect("should set backend before building RuntimeServices"),
            http_client: self
                .http_client
                .expect("should set http_client before building RuntimeServices"),
            geo: self
                .geo
                .expect("should set geo before building RuntimeServices"),
            client_info: self
                .client_info
                .expect("should set client_info before building RuntimeServices"),
            auction_event_sink: self
                .auction_event_sink
                .unwrap_or_else(|| Arc::new(NoopSink)),
        }
    }
}

#[cfg(test)]
mod auction_sink_tests {
    use crate::auction::telemetry::types::{AuctionObservationContext, AuctionSource, EventKind};
    use crate::auction::telemetry::{AuctionEventRow, InMemorySink};
    use crate::platform::test_support::noop_services;

    fn row() -> AuctionEventRow {
        let ctx = AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::AuctionApi,
            publisher_domain: "example.com".to_string(),
            page_path: "/p".to_string(),
            country: "US".to_string(),
            region: None,
            is_mobile: 2,
            is_known_browser: 2,
            gdpr_applies: false,
            consent_present: false,
        };
        AuctionEventRow::base(&ctx, EventKind::Summary)
    }

    #[test]
    fn default_sink_is_noop_and_does_not_panic() {
        let services = noop_services();
        services.auction_event_sink().emit(&[row()]);
    }

    #[test]
    fn injected_sink_captures_emitted_rows() {
        let sink = std::sync::Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());
        services.auction_event_sink().emit(&[row()]);
        assert_eq!(
            sink.rows().len(),
            1,
            "should route emitted rows to the injected sink"
        );
    }
}
