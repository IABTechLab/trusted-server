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
    pub config_store: Arc<dyn PlatformConfigStore>,
    /// Access to encrypted secret stores.
    pub secret_store: Arc<dyn PlatformSecretStore>,
    /// KV store for the primary (opid) store.
    ///
    /// Additional stores (`counter_store`, `creative_store`) are opened on
    /// demand in individual handlers until multi-store support lands here.
    pub kv_store: Arc<dyn PlatformKvStore>,
    /// Dynamic backend registration and name prediction.
    pub backend: Arc<dyn PlatformBackend>,
    /// Outbound HTTP client abstraction.
    pub http_client: Arc<dyn PlatformHttpClient>,
    /// Geographic information lookup.
    pub geo: Arc<dyn PlatformGeo>,
    /// Per-request client metadata extracted at the entry point.
    pub client_info: ClientInfo,
}

impl RuntimeServices {
    /// Wrap the KV store in a [`edgezero_core::key_value_store::KvHandle`] for
    /// ergonomic access to JSON helpers, pagination, and validation.
    #[must_use]
    pub fn kv_handle(&self) -> edgezero_core::key_value_store::KvHandle {
        edgezero_core::key_value_store::KvHandle::new(self.kv_store.clone())
    }
}
