use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use super::{
    PlatformBackend, PlatformConfigStore, PlatformGeo, PlatformHttpClient, PlatformKvStore,
    PlatformSecretStore,
};

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
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Client IP address, if available.
    pub client_ip: Option<IpAddr>,
    /// TLS protocol version string, if the connection used TLS.
    pub tls_protocol: Option<String>,
    /// OpenSSL cipher name, if the connection used TLS.
    pub tls_cipher: Option<String>,
}

/// Edge-visible name used to open a config or secret store at runtime.
///
/// Passed to read methods on [`super::PlatformConfigStore`] and
/// [`super::PlatformSecretStore`]. Distinct from [`StoreId`] to prevent
/// accidentally passing a management API identifier where a runtime name is
/// expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display)]
pub struct StoreName(pub String);

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
pub struct StoreId(pub String);

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
    /// KV store for the primary (opid) store.
    ///
    /// Additional stores (`counter_store`, `creative_store`) are opened on
    /// demand in individual handlers until multi-store support lands here.
    pub(crate) kv_store: Arc<dyn PlatformKvStore>,
    /// Dynamic backend registration and name prediction.
    pub(crate) backend: Arc<dyn PlatformBackend>,
    /// Outbound HTTP client abstraction.
    pub(crate) http_client: Arc<dyn PlatformHttpClient>,
    /// Geographic information lookup.
    pub(crate) geo: Arc<dyn PlatformGeo>,
    /// Per-request client metadata extracted at the entry point.
    pub client_info: ClientInfo,
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

    /// Wrap the KV store in a [`super::KvHandle`] for ergonomic access to
    /// JSON helpers, pagination, and validation.
    #[must_use]
    pub fn kv_handle(&self) -> super::KvHandle {
        super::KvHandle::new(self.kv_store.clone())
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
        }
    }
}
