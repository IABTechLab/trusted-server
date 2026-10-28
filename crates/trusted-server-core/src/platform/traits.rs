use std::net::IpAddr;

use error_stack::Report;

use super::{GeoInfo, PlatformBackendSpec, PlatformError, RuntimeServices, StoreId, StoreName};

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
///
/// The one interface through which the host answers how it names a backend
/// and how it bounds a transport timer. Startup validation asks the same
/// implementation the runtime services carry, so the names validation
/// predicts are the names the host registers.
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
    /// A host that embeds the transport timeout in the backend name overrides
    /// this to round budget-derived values to a coarse ladder, so per-request
    /// wall-clock jitter neither defeats cross-request connection pooling nor
    /// accumulates registrations toward the host's backend limit. The default
    /// keeps the exact bounded value.
    fn canonicalize_transport_timeout_ms(&self, remaining_ms: u32, configured_ms: u32) -> u32 {
        remaining_ms.min(configured_ms)
    }

    /// How many backend names the auction may register on this host, or
    /// `None` where a backend name is not a limited resource.
    fn auction_backend_budget(&self) -> Option<usize> {
        None
    }

    /// How many distinct backend names one provider with this configured
    /// timeout can reach through [`Self::canonicalize_transport_timeout_ms`].
    ///
    /// Plan validation adds every provider's count and refuses a plan that
    /// could exceed [`Self::auction_backend_budget`]. A host whose names do
    /// not carry the timer reaches one name per provider.
    fn transport_timeout_bucket_count(&self, configured_ms: u32) -> usize {
        let _ = configured_ms;
        1
    }
}

/// Object-safe geo lookup.
///
/// Asynchronous because a geo module may reach a backend, a key-value store
/// or a secret to resolve a location, and a module that cannot make those
/// calls cannot be written at all. A module that resolves from data already
/// in hand still declares an async method and returns immediately.
///
/// Uses `#[async_trait(?Send)]` for the same reason as
/// [`PlatformHttpClient`](super::PlatformHttpClient): the trait object stays
/// `Send + Sync` so it can be shared and run multi-threaded, while the future
/// it returns is pinned to one thread because the host SDKs produce `!Send`
/// futures on wasm32.
#[async_trait::async_trait(?Send)]
pub trait PlatformGeo: Send + Sync {
    /// Look up geographic information for the given client IP address.
    ///
    /// An implementation must return [`GeoInfo`] with the country as an
    /// ISO 3166-1 alpha-2 code (for example `US`) and the region as the
    /// ISO 3166-2 subdivision code without the country prefix (for example
    /// `CA`). The permission model keys its country and region rules on these
    /// codes, matched case-insensitively, so the Fastly and other geo
    /// modules feed the same rules without translation.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Geo`] when the platform geo lookup fails
    /// unexpectedly. Returns `Ok(None)` when no data is available for the IP.
    async fn lookup(
        &self,
        client_ip: Option<IpAddr>,
        services: &RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>>;

    /// The permissions this module's data use requires.
    ///
    /// The default is empty, so the default (disabled) geo module requires no
    /// permission.
    fn required_permissions(&self) -> crate::permissions::PermissionSet {
        crate::permissions::PermissionSet::none()
    }
}

/// A geo module as core calls it, with the request's module call, which
/// carries the request's evidence, the settings and the services as a device
/// module's call does, so a module whose one service answers location and
/// device from the same request can answer both from one call. A type that
/// resolves from the client address alone implements [`PlatformGeo`] and is
/// called through [`PlatformGeoModule`](super::PlatformGeoModule).
///
/// Core builds the call in one place, `locate_request` on `dyn GeoModule`,
/// so every path that asks for a location hands the module the same
/// evidence. No path asks again when a response is finished.
///
/// Uses `#[async_trait(?Send)]` for the same reason as [`PlatformGeo`].
#[async_trait::async_trait(?Send)]
pub trait GeoModule: Send + Sync {
    /// The name the module is called as, for the call and the logs.
    fn id(&self) -> &'static str;

    /// Resolves the request's location, `None` when the module has no data
    /// for it, with the country as an ISO 3166-1 alpha-2 code and the region
    /// as the ISO 3166-2 subdivision code without the country prefix, as
    /// [`PlatformGeo::lookup`] gives them.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Geo`] when the lookup fails unexpectedly.
    async fn locate(
        &self,
        call: crate::module_context::ModuleCall<'_>,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>>;

    /// The permissions this module's data use requires. Empty by default.
    fn required_permissions(&self) -> crate::permissions::PermissionSet {
        crate::permissions::PermissionSet::none()
    }
}
