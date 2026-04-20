//! Store helpers and legacy Fastly-backed store types.
//!
//! The Fastly config/secret store types predate the [`crate::platform`]
//! abstraction and will be removed once all call sites have migrated to the
//! platform traits. New code should use
//! [`crate::platform::PlatformConfigStore`],
//! [`crate::platform::PlatformSecretStore`], and the management write methods
//! via [`crate::platform::RuntimeServices`].

pub(crate) mod api_client;
pub(crate) mod config_store;
pub mod kv_store;
pub(crate) mod secret_store;

pub use api_client::FastlyApiClient;
pub use config_store::FastlyConfigStore;
pub use secret_store::FastlySecretStore;
