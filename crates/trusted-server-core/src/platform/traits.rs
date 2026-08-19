use std::net::IpAddr;

use error_stack::Report;

use super::{GeoInfo, PlatformBackendSpec, PlatformError, StoreId, StoreName};

/// Synchronous, object-safe access to a key-value config store.
///
/// Reads use the edge-visible store name. Writes use the platform management
/// store identifier because Fastly separates the runtime store name from the
/// management API store ID.
pub trait PlatformConfigStore: Send + Sync {
    /// Retrieve a string value from `store_name` by `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ConfigStore`] when the key does not exist or
    /// the store cannot be opened.
    fn get(&self, store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>>;

    /// Store a string value in the management store identified by `store_id`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ConfigStore`] when the write fails or the
    /// platform management API is unreachable.
    fn put(&self, store_id: &StoreId, key: &str, value: &str) -> Result<(), Report<PlatformError>>;

    /// Delete a key from the management store identified by `store_id`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ConfigStore`] when the delete fails or the
    /// platform management API is unreachable.
    fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>>;
}

/// Synchronous, object-safe access to a secret store.
///
/// Reads use the edge-visible store name. Writes use the platform management
/// store identifier.
pub trait PlatformSecretStore: Send + Sync {
    /// Retrieve a secret value as raw bytes from `store_name` by `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] when the store cannot be opened,
    /// the key does not exist, or decryption fails.
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>>;

    /// Retrieve a secret value as a UTF-8 string from `store_name` by `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] when the secret cannot be
    /// retrieved or is not valid UTF-8.
    fn get_string(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<String, Report<PlatformError>> {
        let bytes = self.get_bytes(store_name, key)?;
        String::from_utf8(bytes).map_err(|error| {
            Report::new(PlatformError::SecretStore)
                .attach(format!("secret is not valid UTF-8: {error}"))
        })
    }

    /// Create or overwrite a secret in the management store identified by `store_id`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] when the create fails or the
    /// platform management API is unreachable.
    fn create(
        &self,
        store_id: &StoreId,
        name: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>>;

    /// Delete a secret from the management store identified by `store_id`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] when the delete fails or the
    /// platform management API is unreachable.
    fn delete(&self, store_id: &StoreId, name: &str) -> Result<(), Report<PlatformError>>;
}

/// Synchronous, object-safe dynamic backend management.
pub trait PlatformBackend: Send + Sync {
    /// Compute the deterministic backend name for the given spec without
    /// registering anything.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Backend`] when the spec is invalid or the
    /// name cannot be computed.
    fn predict_name(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>>;

    /// Ensure a dynamic backend exists for the given spec and return its name.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Backend`] when the backend cannot be
    /// registered on the platform.
    fn ensure(&self, spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>>;

    /// Canonicalize a per-provider transport timeout for backend-name stability.
    ///
    /// `remaining_ms` is the wall-clock budget left in the auction and
    /// `configured_ms` is the provider's own configured timeout. The returned
    /// value is used both to derive the dynamic backend name and as the
    /// provider's request deadline, so it must be identical for prediction and
    /// registration of the same launch.
    ///
    /// Adapters that embed the transport timeout in the dynamic backend name
    /// (Fastly) override this to round budget-derived values to a coarse
    /// ladder, so per-request wall-clock jitter neither defeats cross-request
    /// connection pooling nor accumulates registrations toward the per-service
    /// dynamic backend limit.
    ///
    /// The default returns the exact budget-bound value
    /// (`remaining_ms.min(configured_ms)`): adapters that neither register nor
    /// enforce a backend-name transport timeout gain nothing from rounding and
    /// must not shorten bidder deadlines for no benefit.
    fn canonicalize_transport_timeout_ms(&self, remaining_ms: u32, configured_ms: u32) -> u32 {
        remaining_ms.min(configured_ms)
    }
}

/// Synchronous, object-safe geo lookup.
pub trait PlatformGeo: Send + Sync {
    /// Look up geographic information for the given client IP address.
    ///
    /// An implementation must return [`GeoInfo`] with the country as an
    /// ISO 3166-1 alpha-2 code (for example `US`) and the region as the
    /// ISO 3166-2 subdivision code without the country prefix (for example
    /// `CA`). The permission model keys its country and region rules on these
    /// codes, matched case-insensitively, so the Fastly and other geo
    /// providers feed the same rules without translation.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Geo`] when the platform geo lookup fails
    /// unexpectedly. Returns `Ok(None)` when no data is available for the IP.
    fn lookup(&self, client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>>;

    /// The permissions this provider's data use requires.
    ///
    /// The default is empty, so the default (disabled) geo provider requires no
    /// permission.
    fn required_permissions(&self) -> crate::permissions::PermissionSet {
        crate::permissions::PermissionSet::none()
    }
}
